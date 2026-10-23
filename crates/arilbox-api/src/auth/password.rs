//! Password hashing with argon2id (`SPEC.md` §12).
//!
//! Hashing is CPU- and memory-heavy (64 MiB per hash), so it runs on the blocking pool and at
//! most [`MAX_CONCURRENT`] hashes run at once; further requests wait their turn.

use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::{Algorithm, Argon2, Params, Version};
use axum::http::StatusCode;
use tokio::sync::{OnceCell, Semaphore};

use crate::error::ApiError;

/// Shortest accepted password, in characters.
pub const MIN_PASSWORD_CHARS: usize = 10;
/// Longest accepted password, in bytes (bounds hashing work per request).
pub const MAX_PASSWORD_BYTES: usize = 1024;
/// Hashes computed at the same time.
const MAX_CONCURRENT: usize = 2;

/// argon2id cost parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HashParams {
    /// Memory in KiB.
    pub memory_kib: u32,
    /// Passes over memory.
    pub iterations: u32,
    /// Lanes.
    pub parallelism: u32,
}

impl HashParams {
    /// The production parameters: 64 MiB, 3 iterations, 1 lane.
    pub const SPEC: Self = Self {
        memory_kib: 64 * 1024,
        iterations: 3,
        parallelism: 1,
    };

    /// Minimal cost, for tests only.
    pub const INSECURE_FAST: Self = Self {
        memory_kib: 8,
        iterations: 1,
        parallelism: 1,
    };
}

/// Rejects passwords that are too short or too long (`400 weak_password` / `invalid_request`).
pub fn check_policy(password: &str) -> Result<(), ApiError> {
    if password.len() > MAX_PASSWORD_BYTES {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            format!("Password must be at most {MAX_PASSWORD_BYTES} bytes"),
        ));
    }
    if password.chars().count() < MIN_PASSWORD_CHARS {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "weak_password",
            format!("Password must be at least {MIN_PASSWORD_CHARS} characters"),
        ));
    }
    Ok(())
}

/// Hashes and verifies passwords.
pub(crate) struct Hasher {
    argon: Argon2<'static>,
    permits: Semaphore,
    /// Hash of a random password, verified against when a user doesn't exist so that unknown
    /// and known usernames take the same time.
    dummy: OnceCell<String>,
}

impl Hasher {
    pub(crate) fn new(params: HashParams) -> Result<Self, argon2::Error> {
        let params = Params::new(
            params.memory_kib,
            params.iterations,
            params.parallelism,
            None,
        )?;
        Ok(Self {
            argon: Argon2::new(Algorithm::Argon2id, Version::V0x13, params),
            permits: Semaphore::new(MAX_CONCURRENT),
            dummy: OnceCell::new(),
        })
    }

    /// A PHC string (`$argon2id$v=19$…`) with a random salt.
    pub(crate) async fn hash(&self, password: &str) -> Result<String, ApiError> {
        let argon = self.argon.clone();
        let password = password.to_owned();
        self.blocking(move || {
            argon
                .hash_password(password.as_bytes())
                .map(|h| h.to_string())
                .map_err(|e| {
                    tracing::error!(error = %e, "hashing password");
                    ApiError::internal()
                })
        })
        .await
    }

    /// Whether `password` matches `phc`. `None` (unknown user) does the same work and is false.
    pub(crate) async fn verify(
        &self,
        password: &str,
        phc: Option<String>,
    ) -> Result<bool, ApiError> {
        let known = phc.is_some();
        let phc = match phc {
            Some(phc) => phc,
            None => self.dummy().await?,
        };
        let argon = self.argon.clone();
        let password = password.to_owned();
        let matches = self
            .blocking(move || {
                Ok(argon
                    .verify_password(password.as_bytes(), phc.as_str())
                    .is_ok())
            })
            .await?;
        Ok(known && matches)
    }

    async fn dummy(&self) -> Result<String, ApiError> {
        self.dummy
            .get_or_try_init(|| async {
                let mut random = [0u8; 32];
                crate::auth::random_bytes(&mut random)?;
                self.hash(&crate::auth::hex(&random)).await
            })
            .await
            .cloned()
    }

    async fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> Result<T, ApiError> + Send + 'static,
    ) -> Result<T, ApiError> {
        // The semaphore is never closed, so acquiring only fails if that invariant breaks.
        let _permit = self
            .permits
            .acquire()
            .await
            .map_err(|_| ApiError::internal())?;
        tokio::task::spawn_blocking(work).await.map_err(|e| {
            tracing::error!(error = %e, "password hashing task failed");
            ApiError::internal()
        })?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hash_and_verify() {
        let h = Hasher::new(HashParams::INSECURE_FAST).unwrap();
        let phc = h.hash("correct horse").await.unwrap();
        assert!(phc.starts_with("$argon2id$v=19$"), "{phc}");
        assert!(!phc.contains("correct horse"));
        assert!(h.verify("correct horse", Some(phc.clone())).await.unwrap());
        assert!(!h.verify("wrong horse", Some(phc.clone())).await.unwrap());
        // Unknown user never matches, even with the "right" password.
        assert!(!h.verify("correct horse", None).await.unwrap());
        // Salts differ.
        assert_ne!(phc, h.hash("correct horse").await.unwrap());
    }

    #[test]
    fn spec_parameters_are_encoded() {
        let p = HashParams::SPEC;
        assert_eq!((p.memory_kib, p.iterations, p.parallelism), (65536, 3, 1));
        assert!(Hasher::new(p).is_ok());
    }

    #[test]
    fn policy() {
        assert_eq!(check_policy("short").unwrap_err().code, "weak_password");
        assert!(check_policy("long enough!").is_ok());
        // Ten characters, counted as characters, not bytes.
        assert!(check_policy("ééééééééé").is_err());
        assert!(check_policy("éééééééééé").is_ok());
        assert_eq!(
            check_policy(&"x".repeat(MAX_PASSWORD_BYTES + 1))
                .unwrap_err()
                .code,
            "invalid_request"
        );
    }
}

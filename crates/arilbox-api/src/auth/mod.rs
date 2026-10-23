//! Authentication (`SPEC.md` §12): first-run setup, password login and server-side sessions.
//!
//! v1 has a single admin user. Until it exists, the daemon holds a one-time setup token
//! ([`Auth::start_setup`]) that it prints to the journal; whoever presents it chooses the
//! username and password. Sessions are random 256-bit IDs in an `HttpOnly; SameSite=Strict`
//! cookie; the database stores only their SHA-256.

mod limiter;
mod password;

use std::net::IpAddr;
use std::time::{Duration, Instant};

use arilbox_core::Username;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use ring::rand::{SecureRandom, SystemRandom};
use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::ApiError;
use limiter::Limiter;
use password::Hasher;
pub use password::{HashParams, MAX_PASSWORD_BYTES, MIN_PASSWORD_CHARS, check_policy};

/// Name of the session cookie.
pub const SESSION_COOKIE: &str = "arilbox_session";
/// Sessions are touched in the database at most this often (limits writes on busy pages).
const TOUCH_INTERVAL_S: i64 = 60;

/// Tunables. [`Default`] gives the values in `SPEC.md` §12.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// Add `Secure` to the cookie (always, except plain-HTTP dev mode).
    pub secure_cookie: bool,
    /// A session unused this long expires.
    pub idle_timeout: Duration,
    /// A session expires this long after login regardless of use.
    pub absolute_timeout: Duration,
    /// Failed attempts per IP within `failure_window` before attempts are refused.
    pub max_failures: usize,
    /// Window for `max_failures`.
    pub failure_window: Duration,
    /// argon2id cost.
    pub hash: HashParams,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            secure_cookie: true,
            idle_timeout: Duration::from_secs(8 * 3600),
            absolute_timeout: Duration::from_secs(7 * 24 * 3600),
            max_failures: 5,
            failure_window: Duration::from_secs(15 * 60),
            hash: HashParams::SPEC,
        }
    }
}

/// The logged-in user of a request. Extracting it from a request without a valid session
/// gives `401 unauthenticated`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentUser {
    /// Database ID.
    pub id: i64,
    /// Login name.
    pub username: Username,
    /// SHA-256 of this request's session ID.
    pub(crate) session_hash: String,
}

impl<S: Send + Sync> FromRequestParts<S> for CurrentUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, ApiError> {
        parts
            .extensions
            .get::<CurrentUser>()
            .cloned()
            .ok_or_else(unauthenticated)
    }
}

/// `401 unauthenticated`.
pub fn unauthenticated() -> ApiError {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        "unauthenticated",
        "Not logged in or the session has expired",
    )
}

/// A new session: the user and the secret to put in the cookie.
#[derive(Debug)]
pub(crate) struct NewSession {
    pub user: CurrentUser,
    pub token: String,
}

/// Users, sessions, setup token and login throttling.
pub struct Auth {
    db: SqlitePool,
    config: AuthConfig,
    hasher: Hasher,
    limiter: Limiter,
    /// SHA-256 of the pending setup token. The lock also serialises setup attempts.
    setup_token: tokio::sync::Mutex<Option<String>>,
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Auth")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl Auth {
    /// Fails only for invalid argon2 parameters.
    pub fn new(db: SqlitePool, config: AuthConfig) -> Result<Self, argon2::Error> {
        Ok(Self {
            db,
            hasher: Hasher::new(config.hash)?,
            limiter: Limiter::new(config.max_failures, config.failure_window),
            config,
            setup_token: tokio::sync::Mutex::new(None),
        })
    }

    /// Whether first-run setup is still needed (no user exists).
    pub async fn setup_required(&self) -> Result<bool, ApiError> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&self.db)
            .await
            .map_err(db_error)?;
        Ok(count == 0)
    }

    /// If setup is needed, makes a new one-time setup token (replacing any earlier one) and
    /// returns it for the operator; `None` once a user exists.
    pub async fn start_setup(&self) -> Result<Option<String>, ApiError> {
        let mut slot = self.setup_token.lock().await;
        if !self.setup_required().await? {
            *slot = None;
            return Ok(None);
        }
        let token = new_token()?;
        *slot = Some(sha256_hex(&token));
        Ok(Some(token))
    }

    /// Creates the admin user with the setup token and logs them in.
    pub(crate) async fn complete_setup(
        &self,
        ip: IpAddr,
        token: &str,
        username: Username,
        password: &str,
    ) -> Result<NewSession, ApiError> {
        self.check_throttle(ip)?;
        let mut slot = self.setup_token.lock().await;
        if !self.setup_required().await? {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "setup_done",
                "Setup has already been completed",
            ));
        }
        let valid = slot
            .as_deref()
            .is_some_and(|expected| ct_eq(expected, &sha256_hex(token)));
        if !valid {
            self.limiter.fail(ip, Instant::now());
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "invalid_setup_token",
                "The setup token is wrong or has expired; see the arilboxd log for the current one",
            ));
        }
        check_policy(password)?;
        let hash = self.hasher.hash(password).await?;
        let now = rfc3339_now();
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO users (username, password_hash, created_at, updated_at) \
             VALUES (?, ?, ?, ?) RETURNING id",
        )
        .bind(username.as_str())
        .bind(&hash)
        .bind(&now)
        .bind(&now)
        .fetch_one(&self.db)
        .await
        .map_err(db_error)?;
        *slot = None;
        self.limiter.succeed(ip);
        tracing::info!(%username, "first-run setup completed");
        self.create_session(id, username).await
    }

    /// Checks credentials and starts a session.
    pub(crate) async fn login(
        &self,
        ip: IpAddr,
        username: &str,
        password: &str,
    ) -> Result<NewSession, ApiError> {
        self.check_throttle(ip)?;
        if self.setup_required().await? {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "setup_required",
                "No user exists yet; complete first-run setup",
            ));
        }
        let user = match Username::new(username) {
            Ok(name) => sqlx::query("SELECT id, password_hash FROM users WHERE username = ?")
                .bind(name.as_str())
                .fetch_optional(&self.db)
                .await
                .map_err(db_error)?
                .map(|row| {
                    (
                        row.get::<i64, _>("id"),
                        name,
                        row.get::<String, _>("password_hash"),
                    )
                }),
            Err(_) => None,
        };
        let ok = password.len() <= MAX_PASSWORD_BYTES
            && self
                .hasher
                .verify(password, user.as_ref().map(|(_, _, h)| h.clone()))
                .await?;
        match user {
            Some((id, name, _)) if ok => {
                self.limiter.succeed(ip);
                self.purge_expired().await;
                self.create_session(id, name).await
            }
            _ => {
                self.limiter.fail(ip, Instant::now());
                Err(ApiError::new(
                    StatusCode::UNAUTHORIZED,
                    "invalid_credentials",
                    "Wrong username or password",
                ))
            }
        }
    }

    /// The user for a session ID from a cookie, if the session is valid. Expired sessions are
    /// deleted.
    pub async fn authenticate(&self, token: &str) -> Result<Option<CurrentUser>, ApiError> {
        if !is_token(token) {
            return Ok(None);
        }
        let hash = sha256_hex(token);
        let Some(row) = sqlx::query(
            "SELECT s.user_id, s.created_at, s.last_seen_at, u.username \
             FROM sessions s JOIN users u ON u.id = s.user_id WHERE s.id_hash = ?",
        )
        .bind(&hash)
        .fetch_optional(&self.db)
        .await
        .map_err(db_error)?
        else {
            return Ok(None);
        };
        let now = unix_now();
        let created: i64 = row.get("created_at");
        let last_seen: i64 = row.get("last_seen_at");
        if now - last_seen >= secs(self.config.idle_timeout)
            || now - created >= secs(self.config.absolute_timeout)
        {
            self.delete_session(&hash).await?;
            return Ok(None);
        }
        if now - last_seen >= TOUCH_INTERVAL_S {
            sqlx::query("UPDATE sessions SET last_seen_at = ? WHERE id_hash = ?")
                .bind(now)
                .bind(&hash)
                .execute(&self.db)
                .await
                .map_err(db_error)?;
        }
        let Ok(username) = Username::new(row.get::<String, _>("username")) else {
            tracing::error!("stored username fails validation");
            return Err(ApiError::internal());
        };
        Ok(Some(CurrentUser {
            id: row.get("user_id"),
            username,
            session_hash: hash,
        }))
    }

    /// Ends the user's current session.
    pub(crate) async fn logout(&self, user: &CurrentUser) -> Result<(), ApiError> {
        self.delete_session(&user.session_hash).await
    }

    /// Changes the password after checking the current one, and ends the user's other sessions.
    pub(crate) async fn change_password(
        &self,
        ip: IpAddr,
        user: &CurrentUser,
        current: &str,
        new: &str,
    ) -> Result<(), ApiError> {
        self.check_throttle(ip)?;
        let hash: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = ?")
            .bind(user.id)
            .fetch_one(&self.db)
            .await
            .map_err(db_error)?;
        let ok =
            current.len() <= MAX_PASSWORD_BYTES && self.hasher.verify(current, Some(hash)).await?;
        if !ok {
            self.limiter.fail(ip, Instant::now());
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "wrong_password",
                "The current password is wrong",
            ));
        }
        check_policy(new)?;
        let hash = self.hasher.hash(new).await?;
        let mut tx = self.db.begin().await.map_err(db_error)?;
        sqlx::query("UPDATE users SET password_hash = ?, updated_at = ? WHERE id = ?")
            .bind(&hash)
            .bind(rfc3339_now())
            .bind(user.id)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        sqlx::query("DELETE FROM sessions WHERE user_id = ? AND id_hash != ?")
            .bind(user.id)
            .bind(&user.session_hash)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        tracing::info!(username = %user.username, "password changed");
        Ok(())
    }

    /// `Set-Cookie` value for a new session.
    pub(crate) fn session_cookie(&self, token: &str) -> HeaderValue {
        self.cookie(token, secs(self.config.absolute_timeout))
    }

    /// `Set-Cookie` value that removes the session cookie.
    pub(crate) fn clear_cookie(&self) -> HeaderValue {
        self.cookie("", 0)
    }

    fn cookie(&self, value: &str, max_age: i64) -> HeaderValue {
        let secure = if self.config.secure_cookie {
            "; Secure"
        } else {
            ""
        };
        let text = format!(
            "{SESSION_COOKIE}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{secure}"
        );
        // Only hex digits and fixed ASCII go in, so this always is a valid header value.
        HeaderValue::from_str(&text).unwrap_or_else(|_| HeaderValue::from_static(""))
    }

    fn check_throttle(&self, ip: IpAddr) -> Result<(), ApiError> {
        match self.limiter.check(ip, Instant::now()) {
            None => Ok(()),
            Some(wait) => Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "too_many_attempts",
                "Too many failed attempts; try again later",
            )
            .retry_after(wait.as_secs().max(1))),
        }
    }

    async fn create_session(
        &self,
        user_id: i64,
        username: Username,
    ) -> Result<NewSession, ApiError> {
        let token = new_token()?;
        let hash = sha256_hex(&token);
        let now = unix_now();
        sqlx::query(
            "INSERT INTO sessions (id_hash, user_id, created_at, last_seen_at) VALUES (?, ?, ?, ?)",
        )
        .bind(&hash)
        .bind(user_id)
        .bind(now)
        .bind(now)
        .execute(&self.db)
        .await
        .map_err(db_error)?;
        Ok(NewSession {
            user: CurrentUser {
                id: user_id,
                username,
                session_hash: hash,
            },
            token,
        })
    }

    async fn delete_session(&self, hash: &str) -> Result<(), ApiError> {
        sqlx::query("DELETE FROM sessions WHERE id_hash = ?")
            .bind(hash)
            .execute(&self.db)
            .await
            .map_err(db_error)?;
        Ok(())
    }

    /// Deletes expired sessions. Best effort: failures are logged.
    async fn purge_expired(&self) {
        let now = unix_now();
        let result = sqlx::query("DELETE FROM sessions WHERE last_seen_at <= ? OR created_at <= ?")
            .bind(now - secs(self.config.idle_timeout))
            .bind(now - secs(self.config.absolute_timeout))
            .execute(&self.db)
            .await;
        if let Err(e) = result {
            tracing::warn!(error = %e, "purging expired sessions");
        }
    }
}

/// The session ID from the request's `Cookie` headers.
pub fn session_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|pair| {
            let (name, value) = pair.trim().split_once('=')?;
            (name == SESSION_COOKIE).then(|| value.to_owned())
        })
}

/// Fills `buf` from the OS random source.
pub(crate) fn random_bytes(buf: &mut [u8]) -> Result<(), ApiError> {
    SystemRandom::new().fill(buf).map_err(|_| {
        tracing::error!("OS random source failed");
        ApiError::internal()
    })
}

/// Lowercase hex.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// A random 256-bit token as 64 hex digits.
fn new_token() -> Result<String, ApiError> {
    let mut bytes = [0u8; 32];
    random_bytes(&mut bytes)?;
    Ok(hex(&bytes))
}

fn is_token(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn sha256_hex(s: &str) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, s.as_bytes()).as_ref())
}

/// Constant-time comparison of two hex digests.
fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

fn unix_now() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

fn rfc3339_now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
}

fn secs(d: Duration) -> i64 {
    i64::try_from(d.as_secs()).unwrap_or(i64::MAX)
}

fn db_error(e: sqlx::Error) -> ApiError {
    tracing::error!(error = %e, "auth database error");
    ApiError::internal()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn auth() -> Auth {
        let db = crate::db::memory().await.unwrap();
        Auth::new(
            db,
            AuthConfig {
                hash: HashParams::INSECURE_FAST,
                ..AuthConfig::default()
            },
        )
        .unwrap()
    }

    fn ip() -> IpAddr {
        "192.0.2.7".parse().unwrap()
    }

    fn admin() -> Username {
        Username::new("admin").unwrap()
    }

    #[tokio::test]
    async fn setup_then_login() {
        let a = auth().await;
        assert!(a.setup_required().await.unwrap());
        let token = a.start_setup().await.unwrap().unwrap();
        assert!(is_token(&token));

        let err = a.login(ip(), "admin", "x").await.unwrap_err();
        assert_eq!(err.code, "setup_required");

        let err = a
            .complete_setup(ip(), "nope", admin(), "long enough pw")
            .await
            .unwrap_err();
        assert_eq!(err.code, "invalid_setup_token");
        let err = a
            .complete_setup(ip(), &token, admin(), "short")
            .await
            .unwrap_err();
        assert_eq!(err.code, "weak_password");
        assert!(a.setup_required().await.unwrap(), "nothing created");

        let s = a
            .complete_setup(ip(), &token, admin(), "long enough pw")
            .await
            .unwrap();
        assert_eq!(s.user.username, admin());
        assert_eq!(a.authenticate(&s.token).await.unwrap(), Some(s.user));

        // The token is single-use and setup can't run twice.
        let err = a
            .complete_setup(ip(), &token, admin(), "long enough pw")
            .await
            .unwrap_err();
        assert_eq!(err.code, "setup_done");
        assert_eq!(a.start_setup().await.unwrap(), None);

        let s = a.login(ip(), "admin", "long enough pw").await.unwrap();
        assert!(a.authenticate(&s.token).await.unwrap().is_some());
        for (user, pass) in [
            ("admin", "wrong"),
            ("nobody", "long enough pw"),
            ("../x", "y"),
        ] {
            let err = a.login(ip(), user, pass).await.unwrap_err();
            assert_eq!(
                (err.status, err.code),
                (StatusCode::UNAUTHORIZED, "invalid_credentials")
            );
        }
    }

    #[tokio::test]
    async fn new_setup_token_replaces_old() {
        let a = auth().await;
        let old = a.start_setup().await.unwrap().unwrap();
        let new = a.start_setup().await.unwrap().unwrap();
        assert_ne!(old, new);
        assert!(
            a.complete_setup(ip(), &old, admin(), "long enough pw")
                .await
                .is_err()
        );
        assert!(
            a.complete_setup(ip(), &new, admin(), "long enough pw")
                .await
                .is_ok()
        );
    }

    async fn with_user() -> (Auth, NewSession) {
        let a = auth().await;
        let token = a.start_setup().await.unwrap().unwrap();
        let s = a
            .complete_setup(ip(), &token, admin(), "long enough pw")
            .await
            .unwrap();
        (a, s)
    }

    #[tokio::test]
    async fn throttles_after_five_failures() {
        let (a, _) = with_user().await;
        for _ in 0..5 {
            assert_eq!(
                a.login(ip(), "admin", "wrong").await.unwrap_err().code,
                "invalid_credentials"
            );
        }
        let err = a.login(ip(), "admin", "long enough pw").await.unwrap_err();
        assert_eq!(
            (err.status, err.code),
            (StatusCode::TOO_MANY_REQUESTS, "too_many_attempts")
        );
        assert!(err.retry_after_s.unwrap() > 800);
        // Another client is unaffected.
        assert!(
            a.login("192.0.2.8".parse().unwrap(), "admin", "long enough pw")
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn sessions_expire() {
        let (a, s) = with_user().await;
        let hash = sha256_hex(&s.token);
        let set = |col: &'static str, ago: i64| {
            let db = a.db.clone();
            let hash = hash.clone();
            async move {
                sqlx::query(&format!("UPDATE sessions SET {col} = ? WHERE id_hash = ?"))
                    .bind(unix_now() - ago)
                    .bind(&hash)
                    .execute(&db)
                    .await
                    .unwrap();
            }
        };
        // Recently used: valid, and the touch moves last_seen forward.
        set("last_seen_at", 120).await;
        assert!(a.authenticate(&s.token).await.unwrap().is_some());
        let seen: i64 = sqlx::query_scalar("SELECT last_seen_at FROM sessions")
            .fetch_one(&a.db)
            .await
            .unwrap();
        assert!(unix_now() - seen < 5);

        // Idle for 8 h: gone.
        set("last_seen_at", 8 * 3600).await;
        assert!(a.authenticate(&s.token).await.unwrap().is_none());
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
            .fetch_one(&a.db)
            .await
            .unwrap();
        assert_eq!(count, 0, "expired session deleted");

        // Active but 7 days old: gone.
        let s = a.login(ip(), "admin", "long enough pw").await.unwrap();
        let hash = sha256_hex(&s.token);
        sqlx::query("UPDATE sessions SET created_at = ? WHERE id_hash = ?")
            .bind(unix_now() - 7 * 24 * 3600)
            .bind(&hash)
            .execute(&a.db)
            .await
            .unwrap();
        assert!(a.authenticate(&s.token).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn only_hashes_are_stored() {
        let (a, s) = with_user().await;
        let stored: String = sqlx::query_scalar("SELECT id_hash FROM sessions")
            .fetch_one(&a.db)
            .await
            .unwrap();
        assert_ne!(stored, s.token);
        let pw: String = sqlx::query_scalar("SELECT password_hash FROM users")
            .fetch_one(&a.db)
            .await
            .unwrap();
        assert!(pw.starts_with("$argon2id$"));
        // Garbage and the stored hash itself are not valid cookies.
        assert!(a.authenticate(&stored).await.unwrap().is_none());
        assert!(
            a.authenticate("x'; DROP TABLE users; --")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn password_change_ends_other_sessions() {
        let (a, s1) = with_user().await;
        let s2 = a.login(ip(), "admin", "long enough pw").await.unwrap();
        let err = a
            .change_password(ip(), &s1.user, "wrong", "brand new password")
            .await
            .unwrap_err();
        assert_eq!(err.code, "wrong_password");
        let err = a
            .change_password(ip(), &s1.user, "long enough pw", "short")
            .await
            .unwrap_err();
        assert_eq!(err.code, "weak_password");
        a.change_password(ip(), &s1.user, "long enough pw", "brand new password")
            .await
            .unwrap();
        assert!(a.authenticate(&s1.token).await.unwrap().is_some());
        assert!(a.authenticate(&s2.token).await.unwrap().is_none());
        assert!(a.login(ip(), "admin", "long enough pw").await.is_err());
        assert!(a.login(ip(), "admin", "brand new password").await.is_ok());
    }

    #[test]
    fn cookies() {
        let mut headers = HeaderMap::new();
        headers.append(header::COOKIE, "a=1; arilbox_session=abc".parse().unwrap());
        assert_eq!(session_token(&headers), Some("abc".into()));
        headers.clear();
        headers.append(header::COOKIE, "x=1".parse().unwrap());
        headers.append(header::COOKIE, "arilbox_session=def".parse().unwrap());
        assert_eq!(session_token(&headers), Some("def".into()));
        headers.clear();
        headers.append(header::COOKIE, "xarilbox_session=1".parse().unwrap());
        assert_eq!(session_token(&headers), None);
    }

    #[tokio::test]
    async fn cookie_attributes() {
        let a = auth().await;
        let c = a.session_cookie("ab");
        assert_eq!(
            c,
            "arilbox_session=ab; Path=/; HttpOnly; SameSite=Strict; Max-Age=604800; Secure"
        );
        assert!(a.clear_cookie().to_str().unwrap().contains("Max-Age=0"));
    }

    #[test]
    fn helpers() {
        assert_eq!(hex(&[0, 15, 255]), "000fff");
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "ab"));
        assert!(!is_token("ABC"));
    }
}

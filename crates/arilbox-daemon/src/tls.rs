//! TLS setup: load the configured certificate, or generate a self-signed one on first start
//! (`SPEC.md` §12).

use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, bail};
use rustls::ServerConfig;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::TlsAcceptor;

/// Builds a TLS acceptor, generating a self-signed certificate if neither file exists.
pub fn acceptor(cert: &Path, key: &Path) -> anyhow::Result<TlsAcceptor> {
    match (cert.exists(), key.exists()) {
        (true, true) => {}
        (false, false) => generate_self_signed(cert, key)?,
        // Never overwrite half of a user-supplied pair.
        _ => bail!(
            "only one of {} and {} exists; provide both or remove both to regenerate",
            cert.display(),
            key.display()
        ),
    }
    Ok(TlsAcceptor::from(Arc::new(server_config(cert, key)?)))
}

fn server_config(cert: &Path, key: &Path) -> anyhow::Result<ServerConfig> {
    let certs = CertificateDer::pem_file_iter(cert)
        .and_then(|it| it.collect::<Result<Vec<_>, _>>())
        .with_context(|| format!("reading certificate {}", cert.display()))?;
    let key_der = PrivateKeyDer::from_pem_file(key)
        .with_context(|| format!("reading private key {}", key.display()))?;

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .context("TLS protocol versions")?
        .with_no_client_auth()
        .with_single_cert(certs, key_der)
        .context("certificate and key do not form a valid pair")?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(config)
}

fn generate_self_signed(cert: &Path, key: &Path) -> anyhow::Result<()> {
    let mut names = vec![
        "localhost".to_owned(),
        "127.0.0.1".to_owned(),
        "::1".to_owned(),
    ];
    if let Ok(host) = std::fs::read_to_string("/proc/sys/kernel/hostname") {
        let host = host.trim();
        if !host.is_empty() && host != "localhost" {
            names.push(host.to_owned());
        }
    }
    let generated =
        rcgen::generate_simple_self_signed(names).context("generating self-signed certificate")?;

    for path in [cert, key] {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)
                .with_context(|| format!("creating {}", dir.display()))?;
        }
    }
    write_new(key, generated.signing_key.serialize_pem().as_bytes(), 0o600)?;
    write_new(cert, generated.cert.pem().as_bytes(), 0o644)?;
    tracing::warn!(
        cert = %cert.display(),
        "generated a self-signed TLS certificate; browsers will warn until you replace it"
    );
    Ok(())
}

/// Writes a new file with `mode`; fails if it already exists.
fn write_new(path: &Path, data: &[u8], mode: u32) -> anyhow::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .with_context(|| format!("creating {}", path.display()))?;
    file.write_all(data)
        .and_then(|()| file.sync_all())
        .with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("arilbox-test-tls-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn generates_then_reuses_certificate() {
        let dir = TempDir::new("gen");
        let (cert, key) = (dir.0.join("tls/cert.pem"), dir.0.join("tls/key.pem"));

        acceptor(&cert, &key).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&key), 0o600);
        assert_eq!(mode(&dir.0.join("tls")), 0o700);

        let first = std::fs::read(&cert).unwrap();
        acceptor(&cert, &key).unwrap();
        assert_eq!(std::fs::read(&cert).unwrap(), first, "must not regenerate");
    }

    #[test]
    fn refuses_half_a_pair() {
        let dir = TempDir::new("half");
        std::fs::create_dir_all(&dir.0).unwrap();
        let (cert, key) = (dir.0.join("cert.pem"), dir.0.join("key.pem"));
        std::fs::write(&cert, "not a cert").unwrap();
        assert!(acceptor(&cert, &key).is_err());
        assert!(!key.exists());
    }

    #[test]
    fn rejects_garbage_pem() {
        let dir = TempDir::new("garbage");
        std::fs::create_dir_all(&dir.0).unwrap();
        let (cert, key) = (dir.0.join("cert.pem"), dir.0.join("key.pem"));
        std::fs::write(&cert, "garbage").unwrap();
        std::fs::write(&key, "garbage").unwrap();
        assert!(acceptor(&cert, &key).is_err());
    }
}

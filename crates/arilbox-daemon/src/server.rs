//! HTTPS accept loop: rustls in front of the axum router (HTTP/1.1 and HTTP/2).

use std::future::Future;
use std::time::Duration;

use axum::Router;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

/// Clients that don't finish the TLS handshake in time are dropped.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Serves `app` over TLS until `shutdown` resolves. Stops accepting new connections on
/// shutdown; connections already open are closed when the runtime exits.
pub async fn serve_tls(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    app: Router,
    shutdown: impl Future<Output = ()>,
) -> anyhow::Result<()> {
    tokio::pin!(shutdown);
    loop {
        let (tcp, peer) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(conn) => conn,
                Err(e) => {
                    // E.g. EMFILE: back off instead of spinning.
                    tracing::warn!(error = %e, "accept failed");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
            () = &mut shutdown => return Ok(()),
        };

        let acceptor = acceptor.clone();
        // The peer address, for login throttling (axum's `ConnectInfo`).
        let app = app
            .clone()
            .layer(axum::Extension(axum::extract::ConnectInfo(peer)));
        tokio::spawn(async move {
            let tls = match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(tcp)).await {
                Ok(Ok(tls)) => tls,
                Ok(Err(e)) => {
                    tracing::debug!(%peer, error = %e, "TLS handshake failed");
                    return;
                }
                Err(_) => {
                    tracing::debug!(%peer, "TLS handshake timed out");
                    return;
                }
            };
            let result = auto::Builder::new(TokioExecutor::new())
                .serve_connection_with_upgrades(TokioIo::new(tls), TowerToHyperService::new(app))
                .await;
            if let Err(e) = result {
                tracing::debug!(%peer, error = %e, "connection error");
            }
        });
    }
}

//! `GET /api/v1/containers/{name}/console?rows=&cols=`: WebSocket terminal (`SPEC.md` §7.3).
//!
//! Binary frames carry raw terminal bytes in both directions. Text frames from the client are
//! JSON control messages: `{ "type": "resize", "rows": 40, "cols": 120 }`. The server closes
//! the socket when the shell exits; closing the socket ends the shell.
//!
//! Browsers can't add the CSRF header to a WebSocket handshake, so instead the `Origin` must
//! match the `Host` the request was sent to (plus the session cookie, which is `SameSite`).

use arilbox_core::ContainerName;
use arilxc::{Console, TermSize};
use axum::body::Bytes;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::AppState;
use crate::auth::CurrentUser;
use crate::error::ApiError;
use crate::extract::query;

/// Largest frame accepted from the client.
const MAX_MESSAGE: usize = 64 * 1024;
/// Largest terminal dimension accepted.
const MAX_DIMENSION: u16 = 1000;
const DEFAULT_SIZE: TermSize = TermSize { rows: 24, cols: 80 };

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsoleQuery {
    rows: Option<u16>,
    cols: Option<u16>,
}

/// Messages a client sends in text frames.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Control {
    Resize { rows: u16, cols: u16 },
}

fn size(rows: u16, cols: u16) -> Option<TermSize> {
    let ok = |n: u16| (1..=MAX_DIMENSION).contains(&n);
    (ok(rows) && ok(cols)).then_some(TermSize { rows, cols })
}

/// The `Origin` header's host must equal the `Host` the request was sent to.
fn same_origin(headers: &HeaderMap, uri: &Uri) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let Some(origin_host) = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))
    else {
        return false;
    };
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .or_else(|| uri.authority().map(|a| a.as_str()));
    host.is_some_and(|h| h.eq_ignore_ascii_case(origin_host))
}

/// Upgrades to a console session after the same checks as any container endpoint.
pub(crate) async fn console(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(name): Path<String>,
    uri: Uri,
    headers: HeaderMap,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Result<Response, ApiError> {
    let name = ContainerName::new(name)?;
    let q: ConsoleQuery = query(&uri)?;
    let size = size(
        q.rows.unwrap_or(DEFAULT_SIZE.rows),
        q.cols.unwrap_or(DEFAULT_SIZE.cols),
    )
    .ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            format!("rows and cols must be between 1 and {MAX_DIMENSION}"),
        )
    })?;
    let ws = ws.map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Expected a WebSocket upgrade",
        )
    })?;
    if !same_origin(&headers, &uri) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "origin_mismatch",
            "WebSocket Origin does not match the host",
        ));
    }
    let console = state.containers.console(&name, size).await?;
    tracing::info!(container = %name, user = %user.username, "console opened");
    Ok(ws
        .max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |socket| pump(socket, console, name)))
}

/// Copies bytes both ways until either side ends, then drops the console (ending the shell).
async fn pump(socket: WebSocket, console: Console, name: ContainerName) {
    let Console {
        mut output,
        mut input,
        session,
    } = console;
    let (mut tx, mut rx) = socket.split();

    let to_client = async {
        let mut buf = vec![0u8; 16 * 1024];
        // Read errors (EIO once the shell is gone) end the session like EOF.
        while let Ok(n @ 1..) = output.read(&mut buf).await {
            if tx
                .send(Message::Binary(Bytes::copy_from_slice(&buf[..n])))
                .await
                .is_err()
            {
                return;
            }
        }
        let close = CloseFrame {
            code: 1000,
            reason: "console closed".into(),
        };
        let _ = tx.send(Message::Close(Some(close))).await;
    };

    let from_client = async {
        while let Some(Ok(msg)) = rx.next().await {
            match msg {
                Message::Binary(data) => {
                    if input.write_all(&data).await.is_err() {
                        return;
                    }
                }
                Message::Text(text) => match serde_json::from_str(text.as_str()) {
                    Ok(Control::Resize { rows, cols }) => match size(rows, cols) {
                        Some(s) => {
                            if let Err(e) = input.resize(s) {
                                tracing::warn!(error = %e, "console resize");
                            }
                        }
                        None => tracing::debug!(rows, cols, "ignoring out-of-range resize"),
                    },
                    Err(_) => tracing::debug!("ignoring unknown console control message"),
                },
                Message::Close(_) => return,
                Message::Ping(_) | Message::Pong(_) => {}
            }
        }
    };

    tokio::select! {
        () = to_client => {}
        () = from_client => {}
    }
    drop(session);
    tracing::info!(container = %name, "console closed");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(origin: Option<&str>, host: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, host.parse().unwrap());
        if let Some(o) = origin {
            h.insert(header::ORIGIN, o.parse().unwrap());
        }
        h
    }

    #[test]
    fn origin_check() {
        let uri: Uri = "/api/v1/containers/web/console".parse().unwrap();
        assert!(same_origin(
            &headers(Some("https://pve1:8443"), "pve1:8443"),
            &uri
        ));
        assert!(same_origin(
            &headers(Some("http://localhost:5173"), "localhost:5173"),
            &uri
        ));
        assert!(same_origin(
            &headers(Some("https://PVE1:8443"), "pve1:8443"),
            &uri
        ));
        for bad in [
            None,
            Some("https://evil.example"),
            Some("https://pve1:8444"),
            Some("pve1:8443"),
            Some("null"),
        ] {
            assert!(!same_origin(&headers(bad, "pve1:8443"), &uri), "{bad:?}");
        }
        // HTTP/2: no Host header, the authority is in the URI.
        let mut h = HeaderMap::new();
        h.insert(header::ORIGIN, "https://pve1:8443".parse().unwrap());
        let uri: Uri = "https://pve1:8443/api/v1/containers/web/console"
            .parse()
            .unwrap();
        assert!(same_origin(&h, &uri));
    }

    #[test]
    fn control_messages() {
        assert_eq!(
            serde_json::from_str::<Control>(r#"{"type":"resize","rows":40,"cols":120}"#).unwrap(),
            Control::Resize {
                rows: 40,
                cols: 120
            }
        );
        assert!(serde_json::from_str::<Control>(r#"{"type":"exec","cmd":"x"}"#).is_err());
        assert!(
            serde_json::from_str::<Control>(r#"{"type":"resize","rows":-1,"cols":1}"#).is_err()
        );
        assert_eq!(size(0, 80), None);
        assert_eq!(size(24, 1001), None);
        assert_eq!(size(24, 80), Some(DEFAULT_SIZE));
    }
}

#[cfg(test)]
mod ws_tests {
    use std::net::SocketAddr;

    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::{Error as WsError, Message as WsMessage};

    use crate::routes::tests::{TestApp, test_app};

    /// Serves the app on a loopback port.
    async fn serve(app: &TestApp) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = app.anon.clone();
        tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        addr
    }

    async fn connect(
        addr: SocketAddr,
        path: &str,
        cookie: Option<&str>,
        origin: &str,
    ) -> Result<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        u16,
    > {
        let mut req = format!("ws://{addr}{path}").into_client_request().unwrap();
        req.headers_mut().insert("origin", origin.parse().unwrap());
        if let Some(c) = cookie {
            req.headers_mut().insert("cookie", c.parse().unwrap());
        }
        match tokio_tungstenite::connect_async(req).await {
            Ok((ws, _)) => Ok(ws),
            Err(WsError::Http(res)) => Err(res.status().as_u16()),
            Err(e) => panic!("{e}"),
        }
    }

    #[tokio::test]
    async fn console_round_trip() {
        let app = test_app(false).await;
        let addr = serve(&app).await;
        let origin = format!("http://{addr}");
        let path = "/api/v1/containers/web/console?rows=30&cols=100";

        assert_eq!(connect(addr, path, None, &origin).await.err(), Some(401));
        assert_eq!(
            connect(addr, path, Some(&app.cookie), &origin).await.err(),
            Some(404)
        );

        // Created but stopped: 409.
        app.backend
            .create(&arilxc::CreateSpec {
                name: arilbox_core::ContainerName::new("web").unwrap(),
                template: Default::default(),
                storage: arilbox_core::StorageName::new("local-dir").unwrap(),
                limits: Default::default(),
                network: arilxc::NetworkSpec {
                    bridge: arilbox_core::BridgeName::new("arilbox0").unwrap(),
                    mode: Default::default(),
                },
                gpu: false,
                nesting: false,
                autostart: false,
            })
            .await
            .unwrap();
        use arilxc::ContainerBackend;
        assert_eq!(
            connect(addr, path, Some(&app.cookie), &origin).await.err(),
            Some(409)
        );
        app.backend
            .start(&arilbox_core::ContainerName::new("web").unwrap())
            .await
            .unwrap();

        // Cross-site page: refused even with the cookie.
        assert_eq!(
            connect(addr, path, Some(&app.cookie), "https://evil.example")
                .await
                .err(),
            Some(403)
        );
        let bad = "/api/v1/containers/web/console?rows=0&cols=100";
        assert_eq!(
            connect(addr, bad, Some(&app.cookie), &origin).await.err(),
            Some(400)
        );

        let mut ws = connect(addr, path, Some(&app.cookie), &origin)
            .await
            .unwrap();
        ws.send(WsMessage::Binary(b"hi\r".to_vec().into()))
            .await
            .unwrap();
        ws.send(WsMessage::Text(
            r#"{"type":"resize","rows":50,"cols":132}"#.into(),
        ))
        .await
        .unwrap();
        let mut out = Vec::new();
        while !String::from_utf8_lossy(&out).contains("hi\r\n") {
            match tokio::time::timeout(std::time::Duration::from_secs(5), ws.next()).await {
                Ok(Some(Ok(WsMessage::Binary(b)))) => out.extend_from_slice(&b),
                other => panic!("unexpected {other:?}"),
            }
        }
        assert!(String::from_utf8_lossy(&out).starts_with("mock console: web"));
        ws.close(None).await.unwrap();

        // The resize arrives after the echo is written; give it a moment.
        for _ in 0..50 {
            if app.backend.console_sizes().len() == 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let sizes: Vec<(u16, u16)> = app
            .backend
            .console_sizes()
            .iter()
            .map(|s| (s.rows, s.cols))
            .collect();
        assert_eq!(sizes, [(30, 100), (50, 132)]);
    }
}

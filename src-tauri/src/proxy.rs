//! The in-app origin.
//!
//! The web app assumes it is served same-origin with the backend: it calls
//! `fetch("/api/...")` with `credentials: "same-origin"`, points `<img src>` at
//! `/api/...` (so the session cookie authenticates the image request), opens an
//! `EventSource` on `/api/chatbot/...`, and builds its exam-room / whiteboard
//! WebSocket URLs from `window.location.host`. In the browser deployment
//! `server.ts` provides that origin; a Tauri webview loaded from
//! `tauri://localhost` would break every one of those.
//!
//! So we reproduce `server.ts` in-process: a loopback HTTP server that serves
//! the bundled SPA and reverse-proxies `/api/*` (HTTP *and* WebSocket) to the
//! backend. The webview is pointed at it, `window.location.host` becomes
//! `127.0.0.1:<port>`, and the frontend needs no changes at all.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ws::{Message as AxumMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use tauri::{AppHandle, Runtime};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;

/// First port we try. Kept fixed across launches on purpose: the session cookie
/// is scoped to `127.0.0.1:<port>`, so a port that moved between launches would
/// sign the user out every time.
const PREFERRED_PORT: u16 = 18420;
const PORT_SCAN: u16 = 32;

struct Inner<R: Runtime> {
    app: AppHandle<R>,
    http: reqwest::Client,
    /// Backend origin, no trailing slash, e.g. `https://api.example.com`.
    backend: String,
    /// Vite's dev server, when running under `tauri dev`. Non-`/api` requests
    /// are proxied there instead of read from the embedded bundle, so HMR keeps
    /// working through our origin.
    dev_url: Option<String>,
}

type Shared<R> = Arc<Inner<R>>;

/// Binds the loopback server and spawns it. Returns the port the webview should
/// be pointed at.
pub fn start<R: Runtime>(
    app: AppHandle<R>,
    backend: String,
    dev_url: Option<String>,
) -> Result<u16, std::io::Error> {
    let listener = bind()?;
    let port = listener.local_addr()?.port();

    let state: Shared<R> = Arc::new(Inner {
        app,
        http: reqwest::Client::builder()
            // The frontend sets its own 20s deadline per request; letting the
            // proxy time out first would turn every slow call into an opaque
            // 502 instead of the client's translated error message.
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .expect("failed to build the HTTP client"),
        backend: backend.trim_end_matches('/').to_string(),
        dev_url: dev_url.map(|u| u.trim_end_matches('/').to_string()),
    });

    let router = Router::new().fallback(handle::<R>).with_state(state);

    std::thread::Builder::new()
        .name("hezarfen-origin".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("failed to start the proxy runtime");
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener)
                    .expect("failed to adopt the listening socket");
                if let Err(err) = axum::serve(listener, router).await {
                    log::error!("origin server stopped: {err}");
                }
            });
        })?;

    Ok(port)
}

/// Prefers [`PREFERRED_PORT`] so the cookie jar survives restarts, but falls
/// back to the next free port rather than refusing to launch.
fn bind() -> Result<std::net::TcpListener, std::io::Error> {
    let mut last = None;
    for offset in 0..PORT_SCAN {
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, PREFERRED_PORT + offset));
        match std::net::TcpListener::bind(addr) {
            Ok(listener) => {
                listener.set_nonblocking(true)?;
                return Ok(listener);
            }
            Err(err) => last = Some(err),
        }
    }
    // Every candidate was taken; let the OS choose so the app still starts.
    let listener = std::net::TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .map_err(|err| last.unwrap_or(err))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

fn is_api(path: &str) -> bool {
    path == "/api" || path.starts_with("/api/")
}

/// `/api/exams/x` -> `/exams/x`, `/api` -> `` — the same rewrite nginx and
/// `server.ts` do, so backend paths are untouched by the prefix.
fn backend_path(uri: &Uri) -> String {
    let path = uri.path();
    let stripped = path
        .strip_prefix("/api")
        .filter(|rest| rest.is_empty() || rest.starts_with('/'))
        .unwrap_or(path);
    match uri.query() {
        Some(q) => format!("{stripped}?{q}"),
        None => stripped.to_string(),
    }
}

fn wants_websocket(headers: &HeaderMap) -> bool {
    headers
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
}

async fn handle<R: Runtime>(State(state): State<Shared<R>>, req: Request) -> Response {
    let api = is_api(req.uri().path());

    if wants_websocket(req.headers()) {
        // Both the exam-room/whiteboard sockets (/api/**) and Vite's HMR socket
        // arrive here; the only difference is which upstream they bridge to.
        let upstream = if api {
            format!("{}{}", ws_scheme(&state.backend), backend_path(req.uri()))
        } else if let Some(dev) = &state.dev_url {
            format!("{}{}", ws_scheme(dev), path_and_query(req.uri()))
        } else {
            return (StatusCode::NOT_FOUND, "no websocket upstream").into_response();
        };
        return bridge_websocket(req, upstream).await;
    }

    if api {
        let target = format!("{}{}", state.backend, backend_path(req.uri()));
        return proxy_http(&state.http, req, target, true).await;
    }

    if let Some(dev) = state.dev_url.clone() {
        let target = format!("{dev}{}", path_and_query(req.uri()));
        return proxy_http(&state.http, req, target, false).await;
    }

    serve_asset(&state.app, req.uri().path())
}

fn path_and_query(uri: &Uri) -> &str {
    uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/")
}

fn ws_scheme(origin: &str) -> String {
    if let Some(rest) = origin.strip_prefix("https") {
        format!("wss{rest}")
    } else if let Some(rest) = origin.strip_prefix("http") {
        format!("ws{rest}")
    } else {
        origin.to_string()
    }
}

/// Serves a file from the bundle, falling back to `index.html` so client-side
/// routes deep-link correctly (the SPA fallback `server.ts` implements).
fn serve_asset<R: Runtime>(app: &AppHandle<R>, path: &str) -> Response {
    let resolver = app.asset_resolver();
    let wanted = if path == "/" { "/index.html" } else { path };

    let asset = resolver
        .get(wanted.to_string())
        .or_else(|| resolver.get("/index.html".to_string()));

    let Some(asset) = asset else {
        return (StatusCode::NOT_FOUND, "asset not found").into_response();
    };

    let mut headers = HeaderMap::new();
    if let Ok(mime) = HeaderValue::from_str(&asset.mime_type) {
        headers.insert(header::CONTENT_TYPE, mime);
    }
    // Hashed assets are immutable; the shell must never be cached, or a new
    // build would keep booting the previous bundle.
    let cache = if wanted.starts_with("/assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-store"
    };
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));

    (headers, asset.bytes).into_response()
}

async fn proxy_http(
    client: &reqwest::Client,
    req: Request,
    target: String,
    rewrite_cookies: bool,
) -> Response {
    let (parts, body) = req.into_parts();

    let method = match reqwest::Method::from_bytes(parts.method.as_str().as_bytes()) {
        Ok(m) => m,
        Err(_) => return (StatusCode::BAD_REQUEST, "bad method").into_response(),
    };

    let mut headers = parts.headers.clone();
    // Hop-by-hop headers must not be forwarded, and the browser's Host (our
    // loopback address) would confuse the backend's cookie-domain and
    // absolute-URL logic — reqwest sets the right one from the target.
    for name in [
        header::HOST,
        header::CONNECTION,
        header::TRANSFER_ENCODING,
        header::CONTENT_LENGTH,
        header::ACCEPT_ENCODING,
    ] {
        headers.remove(name);
    }

    let has_body = !matches!(
        parts.method,
        axum::http::Method::GET | axum::http::Method::HEAD
    );
    let mut out = client.request(method, &target).headers(headers);
    if has_body {
        out = out.body(reqwest::Body::wrap_stream(body.into_data_stream()));
    }

    let upstream = match out.send().await {
        Ok(resp) => resp,
        Err(err) => {
            log::error!("proxy {target} failed: {err}");
            return (StatusCode::BAD_GATEWAY, "backend unreachable").into_response();
        }
    };

    let status =
        StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut headers = HeaderMap::new();
    for (name, value) in upstream.headers() {
        if matches!(
            *name,
            header::TRANSFER_ENCODING | header::CONNECTION | header::CONTENT_LENGTH
        ) {
            continue;
        }
        if rewrite_cookies && name == header::SET_COOKIE {
            if let Some(cookie) = rewrite_set_cookie(value) {
                headers.append(header::SET_COOKIE, cookie);
            }
            continue;
        }
        headers.append(name.clone(), value.clone());
    }

    let body = Body::from_stream(upstream.bytes_stream());
    (status, headers, body).into_response()
}

/// The session cookie is minted for the backend's own host over TLS, so it
/// arrives with `Domain=` and `Secure`. Our origin is plain HTTP on 127.0.0.1;
/// the webview would silently drop such a cookie and the user could never stay
/// signed in. Both attributes are safe to drop here because the cookie never
/// leaves the device's loopback interface. `SameSite=None` is downgraded to
/// `Lax` for the same reason: browsers reject `None` without `Secure`.
fn rewrite_set_cookie(value: &HeaderValue) -> Option<HeaderValue> {
    let raw = value.to_str().ok()?;
    let kept: Vec<&str> = raw
        .split(';')
        .map(str::trim)
        .filter(|attr| {
            let lower = attr.to_ascii_lowercase();
            !(lower == "secure" || lower.starts_with("domain=") || lower == "samesite=none")
        })
        .collect();
    let mut cookie = kept.join("; ");
    if !cookie.to_ascii_lowercase().contains("samesite=") {
        cookie.push_str("; SameSite=Lax");
    }
    HeaderValue::from_str(&cookie).ok()
}

/// Holds one upstream socket open per browser socket and pipes frames both
/// ways. The session cookie is forwarded on the handshake — it is what
/// authenticates the exam-room and whiteboard sockets.
async fn bridge_websocket(req: Request, upstream_url: String) -> Response {
    let (mut parts, _body) = req.into_parts();

    let cookie = parts.headers.get(header::COOKIE).cloned();
    let protocol = parts.headers.get("sec-websocket-protocol").cloned();

    let upgrade = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
        Ok(upgrade) => upgrade,
        Err(rejection) => return rejection.into_response(),
    };

    upgrade.on_upgrade(move |client| async move {
        let mut request = match upstream_url.as_str().into_client_request() {
            Ok(req) => req,
            Err(err) => {
                log::error!("bad websocket url {upstream_url}: {err}");
                return;
            }
        };
        if let Some(cookie) = cookie {
            request.headers_mut().insert(header::COOKIE, cookie);
        }
        if let Some(protocol) = protocol {
            request
                .headers_mut()
                .insert("sec-websocket-protocol", protocol);
        }

        let upstream = match tokio_tungstenite::connect_async(request).await {
            Ok((stream, _)) => stream,
            Err(err) => {
                log::error!("websocket upstream {upstream_url} failed: {err}");
                return;
            }
        };

        pipe(client, upstream).await;
    })
}

async fn pipe(
    client: WebSocket,
    upstream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) {
    let (mut client_tx, mut client_rx) = client.split();
    let (mut up_tx, mut up_rx) = upstream.split();

    let to_upstream = async {
        while let Some(Ok(msg)) = client_rx.next().await {
            let msg = match msg {
                AxumMessage::Text(text) => WsMessage::Text(text.as_str().into()),
                AxumMessage::Binary(bin) => WsMessage::Binary(bin),
                AxumMessage::Ping(p) => WsMessage::Ping(p),
                AxumMessage::Pong(p) => WsMessage::Pong(p),
                AxumMessage::Close(_) => break,
            };
            if up_tx.send(msg).await.is_err() {
                break;
            }
        }
        let _ = up_tx.close().await;
    };

    let to_client = async {
        while let Some(Ok(msg)) = up_rx.next().await {
            let msg = match msg {
                WsMessage::Text(text) => AxumMessage::Text(text.as_str().into()),
                WsMessage::Binary(bin) => AxumMessage::Binary(bin),
                WsMessage::Ping(p) => AxumMessage::Ping(p),
                WsMessage::Pong(p) => AxumMessage::Pong(p),
                WsMessage::Close(_) => break,
                WsMessage::Frame(_) => continue,
            };
            if client_tx.send(msg).await.is_err() {
                break;
            }
        }
        let _ = client_tx.close().await;
    };

    tokio::join!(to_upstream, to_client);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_the_api_prefix_like_the_web_deployment() {
        let uri: Uri = "/api/exams/1?page=2".parse().unwrap();
        assert_eq!(backend_path(&uri), "/exams/1?page=2");
        assert_eq!(backend_path(&"/api".parse().unwrap()), "");
        // A path that merely starts with the letters "api" is not the prefix.
        assert_eq!(backend_path(&"/apiary".parse().unwrap()), "/apiary");
    }

    #[test]
    fn makes_the_session_cookie_storable_on_a_loopback_origin() {
        let raw = HeaderValue::from_static(
            "session=abc; Path=/; Domain=api.hezarfen.co; Secure; HttpOnly; SameSite=None",
        );
        let out = rewrite_set_cookie(&raw).unwrap();
        let out = out.to_str().unwrap();
        assert!(!out.to_ascii_lowercase().contains("secure"));
        assert!(!out.to_ascii_lowercase().contains("domain="));
        assert!(out.contains("SameSite=Lax"));
        assert!(out.contains("HttpOnly"));
        assert!(out.starts_with("session=abc"));
    }
}

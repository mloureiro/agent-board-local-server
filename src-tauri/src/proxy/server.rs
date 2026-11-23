use std::convert::Infallible;
use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::header::{
    HeaderMap, HeaderValue, ACCEPT_RANGES, ALLOW, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE,
    CONTENT_TYPE, HOST, IF_RANGE, RANGE,
};
use http::{Method, Request, Response, StatusCode, Uri};
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::sleep;

use super::guard::Guard;
use super::headers::{apply_data, apply_page, ASSET_CACHE, PAGE_CACHE};
use super::ranges::{self, Selection};
use super::static_files::{content_type, Served, StaticFiles};
use super::upstream::{Body, Upstream};
use crate::applog::LogFile;

const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);
const ACCEPT_RETRY: Duration = Duration::from_millis(100);

pub type UpstreamPort = Arc<dyn Fn() -> Option<u16> + Send + Sync>;

pub struct ProxyConfig {
    pub port: u16,
    pub frontend: PathBuf,
    pub upstream_port: UpstreamPort,
    pub log: LogFile,
}

#[derive(Debug)]
pub enum ProxyError {
    Bind { port: u16, source: io::Error },
}

impl fmt::Display for ProxyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bind { port, source } if source.kind() == io::ErrorKind::AddrInUse => {
                write!(f, "Port {port} in use; change port in settings.json")
            }
            Self::Bind { port, source } => write!(f, "cannot listen on port {port}: {source}"),
        }
    }
}

impl std::error::Error for ProxyError {}

struct State {
    guard: Guard,
    files: StaticFiles,
    upstream: Upstream,
    upstream_port: UpstreamPort,
    log: LogFile,
}

pub struct Proxy {
    local: SocketAddr,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl Proxy {
    pub async fn start(config: ProxyConfig) -> Result<Self, ProxyError> {
        let listener = TcpListener::bind(("127.0.0.1", config.port))
            .await
            .map_err(|source| ProxyError::Bind {
                port: config.port,
                source,
            })?;
        let local = listener.local_addr().map_err(|source| ProxyError::Bind {
            port: config.port,
            source,
        })?;
        let state = Arc::new(State {
            guard: Guard::new(local.port()),
            files: StaticFiles::new(&config.frontend),
            upstream: Upstream::default(),
            upstream_port: config.upstream_port,
            log: config.log,
        });
        let (stop, stopped) = watch::channel(false);
        let task = tokio::spawn(accept_loop(listener, state, stopped));
        Ok(Self { local, stop, task })
    }

    pub fn port(&self) -> u16 {
        self.local.port()
    }

    /// Open connections, streams included, are dropped.
    pub async fn stop(self) {
        let _ = self.stop.send(true);
        let _ = self.task.await;
    }
}

async fn accept_loop(listener: TcpListener, state: Arc<State>, mut stopped: watch::Receiver<bool>) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, peer)) = accepted else {
                    sleep(ACCEPT_RETRY).await;
                    continue;
                };
                let state = state.clone();
                connections.spawn(async move {
                    let service = service_fn(move |request| {
                        let state = state.clone();
                        async move { Ok::<_, Infallible>(handle(&state, peer, request).await) }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .timer(TokioTimer::new())
                        .header_read_timeout(HEADER_READ_TIMEOUT)
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
            _ = stopped.changed() => break,
        }
    }
    connections.shutdown().await;
}

fn is_data_path(path: &str) -> bool {
    path.starts_with("/trpc") || path.starts_with("/api/") || path == "/healthz"
}

fn empty() -> Body {
    Empty::<Bytes>::new()
        .map_err(|never| match never {})
        .boxed()
}

fn answer(status: StatusCode, data: bool) -> Response<Body> {
    let mut response = Response::new(empty());
    *response.status_mut() = status;
    finish(response, data)
}

fn finish(mut response: Response<Body>, data: bool) -> Response<Body> {
    response.headers_mut().remove(CONTENT_LENGTH);
    apply_page(response.headers_mut());
    if data {
        apply_data(response.headers_mut());
    }
    response
}

fn access_line(method: &Method, uri: &Uri, status: StatusCode) -> String {
    format!("{method} {} {}", uri.path(), status.as_u16())
}

async fn handle(state: &State, peer: SocketAddr, request: Request<Incoming>) -> Response<Body> {
    let method = request.method().clone();
    let uri = request.uri().clone();
    let path = uri.path().to_string();
    let data = is_data_path(&path);

    let response = match state.guard.check(&method, request.headers()) {
        Err(refusal) => answer(refusal.status(), data),
        Ok(()) if data => forward(state, peer, request).await,
        Ok(()) => serve_static(state, &method, &path, request.headers()).await,
    };
    state
        .log
        .write(&access_line(&method, &uri, response.status()));
    response
}

async fn forward(
    state: &State,
    peer: SocketAddr,
    mut request: Request<Incoming>,
) -> Response<Body> {
    let Some(port) = (state.upstream_port)() else {
        return answer(StatusCode::BAD_GATEWAY, true);
    };
    let headers = request.headers_mut();
    state.guard.strip_forwarding(headers);
    if let Ok(value) = HeaderValue::from_str(&peer.ip().to_string()) {
        headers.insert("x-forwarded-for", value);
    }
    headers.insert("x-forwarded-proto", HeaderValue::from_static("http"));
    if let Some(host) = headers.get(HOST).cloned() {
        headers.insert("x-forwarded-host", host);
    }

    match state.upstream.forward(port, request).await {
        Some(response) => finish(response, true),
        None => answer(StatusCode::BAD_GATEWAY, true),
    }
}

/// Caddy sets `Cache-Control` per route before the file server answers, so a 404 or a 405 under `/assets/` carries
/// the asset value and every other static answer carries `no-cache`.
async fn serve_static(
    state: &State,
    method: &Method,
    path: &str,
    request_headers: &HeaderMap,
) -> Response<Body> {
    let cache = HeaderValue::from_static(if path.starts_with("/assets/") {
        ASSET_CACHE
    } else {
        PAGE_CACHE
    });
    let plain = |status: StatusCode| {
        let mut response = answer(status, false);
        response.headers_mut().insert(CACHE_CONTROL, cache.clone());
        response
    };
    if !matches!(*method, Method::GET | Method::HEAD) {
        let mut response = plain(StatusCode::METHOD_NOT_ALLOWED);
        response
            .headers_mut()
            .insert(ALLOW, HeaderValue::from_static("GET, HEAD"));
        return response;
    }
    match state.files.resolve(path) {
        Served::Refused => plain(StatusCode::BAD_REQUEST),
        Served::NotFound => plain(StatusCode::NOT_FOUND),
        Served::File { path, immutable } => match tokio::fs::read(&path).await {
            Err(_) => plain(StatusCode::NOT_FOUND),
            Ok(content) => file_response(
                method,
                request_headers,
                content_type(&path),
                immutable,
                content,
            ),
        },
    }
}

/// No validator is ever sent, so an `If-Range` can never match and the whole file answers it.
fn requested_range(request_headers: &HeaderMap, size: u64) -> Selection {
    if request_headers.contains_key(IF_RANGE) {
        return Selection::Whole;
    }
    match request_headers.get(RANGE).map(HeaderValue::to_str) {
        None => Selection::Whole,
        Some(Ok(value)) => ranges::select(value, size),
        Some(Err(_)) => Selection::Invalid,
    }
}

fn sized(body: Vec<u8>, head: bool) -> (Body, usize) {
    let length = body.len();
    let body = if head {
        empty()
    } else {
        Full::new(Bytes::from(body))
            .map_err(|never| match never {})
            .boxed()
    };
    (body, length)
}

fn file_response(
    method: &Method,
    request_headers: &HeaderMap,
    content_type: &'static str,
    immutable: bool,
    content: Vec<u8>,
) -> Response<Body> {
    let head = *method == Method::HEAD;
    let size = content.len() as u64;
    let selection = requested_range(request_headers, size);

    if matches!(selection, Selection::Invalid | Selection::NoOverlap) {
        let (message, content_range) = match selection {
            Selection::NoOverlap => (
                "invalid range: failed to overlap\n",
                Some(format!("bytes */{size}")),
            ),
            _ => ("invalid range\n", None),
        };
        let (body, length) = sized(message.as_bytes().to_vec(), head);
        let mut response = Response::new(body);
        *response.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
        let headers = response.headers_mut();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        if let Some(value) = content_range.and_then(|text| HeaderValue::from_str(&text).ok()) {
            headers.insert(CONTENT_RANGE, value);
        }
        let mut response = finish(response, false);
        response
            .headers_mut()
            .insert(CONTENT_LENGTH, HeaderValue::from(length));
        return response;
    }

    let (status, content_type_value, range, bytes) = match selection {
        Selection::Spans(spans) if spans.len() == 1 => (
            StatusCode::PARTIAL_CONTENT,
            HeaderValue::from_static(content_type),
            Some(spans[0].content_range(size)),
            ranges::slice(&content, spans[0]).to_vec(),
        ),
        Selection::Spans(spans) => {
            let boundary = ranges::boundary();
            let value = format!("multipart/byteranges; boundary={boundary}");
            (
                StatusCode::PARTIAL_CONTENT,
                HeaderValue::from_str(&value).expect("a boundary is hex"),
                None,
                ranges::multipart(&boundary, &spans, size, content_type, &content),
            )
        }
        _ => (
            StatusCode::OK,
            HeaderValue::from_static(content_type),
            None,
            content,
        ),
    };
    let (body, length) = sized(bytes, head);
    let mut response = Response::new(body);
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, content_type_value);
    headers.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_static(if immutable { ASSET_CACHE } else { PAGE_CACHE }),
    );
    if let Some(value) = range.and_then(|text| HeaderValue::from_str(&text).ok()) {
        headers.insert(CONTENT_RANGE, value);
    }
    let mut response = finish(response, false);
    response
        .headers_mut()
        .insert(CONTENT_LENGTH, HeaderValue::from(length));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_access_line_holds_the_path_and_never_the_query() {
        let uri: Uri = "/trpc/task.list?input=%7B%22token%22%3A%22abc%22%7D"
            .parse()
            .unwrap();

        let line = access_line(&Method::GET, &uri, StatusCode::OK);

        assert_eq!(line, "GET /trpc/task.list 200");
    }
}

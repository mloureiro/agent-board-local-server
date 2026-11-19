use std::convert::Infallible;
use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http::header::{HeaderValue, ALLOW, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, HOST};
use http::{Method, Request, Response, StatusCode};
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};

use super::guard::Guard;
use super::headers::{apply_data, apply_page, ASSET_CACHE, PAGE_CACHE};
use super::static_files::{content_type, Served, StaticFiles};
use super::upstream::{Body, Upstream};

const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);

pub type UpstreamPort = Arc<dyn Fn() -> Option<u16> + Send + Sync>;

pub struct ProxyConfig {
    pub port: u16,
    pub user_name: String,
    pub secret: String,
    pub frontend: PathBuf,
    pub upstream_port: UpstreamPort,
}

#[derive(Debug)]
pub enum ProxyError {
    Bind { port: u16, source: io::Error },
    SignIn,
}

impl fmt::Display for ProxyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bind { port, source } if source.kind() == io::ErrorKind::AddrInUse => {
                write!(f, "Port {port} in use; change port in settings.json")
            }
            Self::Bind { port, source } => write!(f, "cannot listen on port {port}: {source}"),
            Self::SignIn => write!(f, "the user name or the secret is not a valid header value"),
        }
    }
}

impl std::error::Error for ProxyError {}

struct State {
    guard: Guard,
    files: StaticFiles,
    upstream: Upstream,
    upstream_port: UpstreamPort,
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
            guard: Guard::new(local.port(), &config.user_name, &config.secret)
                .map_err(|_| ProxyError::SignIn)?,
            files: StaticFiles::new(&config.frontend),
            upstream: Upstream::default(),
            upstream_port: config.upstream_port,
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
                let Ok((stream, peer)) = accepted else { continue };
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

async fn handle(state: &State, peer: SocketAddr, request: Request<Incoming>) -> Response<Body> {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let data = is_data_path(&path);

    let response = match state.guard.check(&method, request.headers()) {
        Err(refusal) => answer(refusal.status(), data),
        Ok(()) if data => forward(state, peer, request).await,
        Ok(()) => serve_static(state, &method, &path).await,
    };
    eprintln!("{method} {path} {}", response.status().as_u16());
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
    state.guard.sign_in(headers);
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

async fn serve_static(state: &State, method: &Method, path: &str) -> Response<Body> {
    if !matches!(*method, Method::GET | Method::HEAD) {
        let mut response = answer(StatusCode::METHOD_NOT_ALLOWED, false);
        response
            .headers_mut()
            .insert(ALLOW, HeaderValue::from_static("GET, HEAD"));
        return response;
    }
    match state.files.resolve(path) {
        Served::Refused => answer(StatusCode::BAD_REQUEST, false),
        Served::NotFound => answer(StatusCode::NOT_FOUND, false),
        Served::File { path, immutable } => match tokio::fs::read(&path).await {
            Err(_) => answer(StatusCode::NOT_FOUND, false),
            Ok(content) => {
                let length = content.len();
                let body = if *method == Method::HEAD {
                    empty()
                } else {
                    Full::new(Bytes::from(content))
                        .map_err(|never| match never {})
                        .boxed()
                };
                let mut response = Response::new(body);
                let headers = response.headers_mut();
                headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type(&path)));
                headers.insert(
                    CACHE_CONTROL,
                    HeaderValue::from_static(if immutable { ASSET_CACHE } else { PAGE_CACHE }),
                );
                let mut response = finish(response, false);
                response
                    .headers_mut()
                    .insert(CONTENT_LENGTH, HeaderValue::from(length));
                response
            }
        },
    }
}

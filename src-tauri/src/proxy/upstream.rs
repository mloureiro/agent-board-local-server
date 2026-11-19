use std::time::Duration;

use bytes::Bytes;
use http::{Request, Response};
use http_body_util::combinators::BoxBody;
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub type Body = BoxBody<Bytes, BoxError>;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const POOL_IDLE: Duration = Duration::from_secs(5);

pub struct Upstream {
    client: Client<HttpConnector, Incoming>,
}

impl Default for Upstream {
    fn default() -> Self {
        let mut connector = HttpConnector::new();
        connector.set_connect_timeout(Some(CONNECT_TIMEOUT));
        connector.set_nodelay(true);
        let client = Client::builder(TokioExecutor::new())
            .pool_idle_timeout(POOL_IDLE)
            .build(connector);
        Self { client }
    }
}

impl Upstream {
    /// Forwards the request and returns only the status and the streamed body: the caller decides every header
    /// the browser sees, so nothing the backend sent can reach it. `None` means the backend could not be reached.
    pub async fn forward(&self, port: u16, request: Request<Incoming>) -> Option<Response<Body>> {
        let (mut parts, body) = request.into_parts();
        let target = parts.uri.path_and_query().map_or("/", |pq| pq.as_str());
        parts.uri = format!("http://127.0.0.1:{port}{target}").parse().ok()?;
        parts.version = http::Version::HTTP_11;

        let answer = self
            .client
            .request(Request::from_parts(parts, body))
            .await
            .ok()?;
        let (answer, body) = answer.into_parts();
        let mut response = Response::new(body.map_err(BoxError::from).boxed());
        *response.status_mut() = answer.status;
        Some(response)
    }
}

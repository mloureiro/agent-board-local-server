use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent_board_local_server::proxy::headers::{DATA, PAGE};
use agent_board_local_server::proxy::{Proxy, ProxyConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::time::timeout;

const SECRET: &str = "s3cret-value";

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn names(&self) -> BTreeSet<&str> {
        self.headers.iter().map(|(name, _)| name.as_str()).collect()
    }
}

fn split_head(raw: &str) -> (u16, Vec<(String, String)>) {
    let mut lines = raw.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .and_then(|code| code.parse().ok())
        .expect("a status line");
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    (status, headers)
}

async fn send(port: u16, request: &str) -> Reply {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    timeout(Duration::from_secs(5), stream.read_to_end(&mut raw))
        .await
        .expect("the proxy closes the connection")
        .unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").expect("a complete response");
    let (status, headers) = split_head(head);
    Reply {
        status,
        headers,
        body: body.to_string(),
    }
}

fn get(path: &str, port: u16) -> String {
    format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n")
}

struct Stub {
    port: u16,
    seen: Arc<Mutex<Vec<String>>>,
}

/// Answers every request with `answer` after recording the request head it received.
async fn stub(answer: &'static str) -> Stub {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let record = record.clone();
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if stream.read_exact(&mut byte).await.is_err() {
                        return;
                    }
                    head.push(byte[0]);
                }
                record
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&head).to_ascii_lowercase());
                let _ = stream.write_all(answer.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    Stub { port, seen }
}

struct Fixture {
    proxy: Proxy,
    _site: tempfile::TempDir,
}

impl Fixture {
    async fn start(upstream_port: u16) -> Self {
        let site = tempfile::tempdir().unwrap();
        let frontend = site.path().join("frontend");
        std::fs::create_dir_all(frontend.join("assets")).unwrap();
        std::fs::write(frontend.join("index.html"), "<html>board</html>").unwrap();
        std::fs::write(frontend.join("assets/app.js"), "console.log(1)").unwrap();
        std::fs::write(site.path().join("secret.txt"), "top secret").unwrap();

        let lookup = Arc::new(AtomicU16::new(upstream_port));
        let proxy = Proxy::start(ProxyConfig {
            port: 0,
            user_name: "ines".into(),
            secret: SECRET.into(),
            frontend,
            upstream_port: Arc::new(move || match lookup.load(Ordering::SeqCst) {
                0 => None,
                port => Some(port),
            }),
        })
        .await
        .unwrap();
        Self { proxy, _site: site }
    }

    fn port(&self) -> u16 {
        self.proxy.port()
    }
}

fn page_and_data_names() -> BTreeSet<&'static str> {
    PAGE.iter()
        .chain(DATA)
        .map(|(name, _)| *name)
        .chain(["connection", "content-length", "date", "transfer-encoding"])
        .collect()
}

#[tokio::test]
async fn the_backend_receives_the_proxy_sign_in_and_nothing_a_client_claimed() {
    let backend = stub("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}").await;
    let fixture = Fixture::start(backend.port).await;
    let port = fixture.port();

    let request = format!(
        "POST /trpc/card.create HTTP/1.1\r\nHost: localhost:{port}\r\nOrigin: http://localhost:{port}\r\n\
         Remote-User: root\r\nremote-groups: wheel\r\nRemote-Email: x@evil.test\r\n\
         Board-Proxy-Secret: guess\r\nConnection: close, Remote-User\r\nX-Forwarded-For: 6.6.6.6\r\n\
         Authorization: Bearer machine\r\nContent-Length: 0\r\n\r\n"
    );
    let reply = send(port, &request).await;
    assert_eq!(reply.status, 200);

    let seen = backend.seen.lock().unwrap();
    let head = &seen[0];
    assert!(head.starts_with("post /trpc/card.create http/1.1"));
    assert_eq!(head.matches("remote-user:").count(), 1);
    assert!(head.contains("remote-user: ines\r\n"));
    assert!(head.contains("remote-groups: admins\r\n"));
    assert!(head.contains(&format!("board-proxy-secret: {SECRET}\r\n")));
    assert!(head.contains("authorization: bearer machine\r\n"));
    for forged in ["root", "wheel", "x@evil.test", "guess", "6.6.6.6"] {
        assert!(!head.contains(forged), "{forged} reached the backend");
    }
}

#[tokio::test]
async fn nothing_but_the_status_and_body_of_a_backend_answer_survives_and_the_data_headers_are_written(
) {
    let backend = stub(
        "HTTP/1.1 200 OK\r\nSet-Cookie: a=b\r\nClear-Site-Data: \"storage\"\r\nLocation: /elsewhere\r\n\
         Refresh: 0;url=/x\r\nContent-Type: text/html\r\nX-Made-Up: 1\r\nContent-Length: 6\r\n\r\n<html>",
    )
    .await;
    let fixture = Fixture::start(backend.port).await;
    let reply = send(fixture.port(), &get("/api/anything", fixture.port())).await;

    assert_eq!(reply.status, 200);
    assert_eq!(reply.body, "<html>");
    assert!(
        reply.names().is_subset(&page_and_data_names()),
        "{:?}",
        reply.names()
    );
    for (name, value) in DATA {
        assert_eq!(reply.header(name), Some(*value), "{name}");
    }
    assert_eq!(reply.header("referrer-policy"), Some("no-referrer"));
}

#[tokio::test]
async fn an_unreachable_backend_answers_502_with_the_data_headers_and_no_body() {
    let nothing_listening = {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        listener.local_addr().unwrap().port()
    };
    for upstream in [0, nothing_listening] {
        let fixture = Fixture::start(upstream).await;
        let reply = send(fixture.port(), &get("/healthz", fixture.port())).await;
        assert_eq!(reply.status, 502);
        assert!(reply.body.is_empty());
        for (name, value) in DATA {
            assert_eq!(reply.header(name), Some(*value), "{name}");
        }
    }
}

#[tokio::test]
async fn a_refused_request_never_reaches_the_backend_and_keeps_the_headers_of_its_path() {
    let backend = stub("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
    let fixture = Fixture::start(backend.port).await;
    let port = fixture.port();

    let rebinding =
        format!("GET /trpc/x HTTP/1.1\r\nHost: evil.test:{port}\r\nConnection: close\r\n\r\n");
    let foreign_write = format!(
        "POST /api/x HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: https://evil.test\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    let cross_site_page = format!(
        "GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nSec-Fetch-Site: cross-site\r\nConnection: close\r\n\r\n"
    );

    let first = send(port, &rebinding).await;
    let second = send(port, &foreign_write).await;
    let third = send(port, &cross_site_page).await;

    assert_eq!((first.status, second.status, third.status), (421, 403, 403));
    for refused in [&first, &second] {
        assert!(refused.body.is_empty());
        for (name, value) in DATA {
            assert_eq!(refused.header(name), Some(*value), "{name}");
        }
    }
    assert!(third.header("content-type").is_none());
    assert!(third
        .header("content-security-policy")
        .unwrap()
        .contains("script-src 'self'"));
    assert!(backend.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_event_stream_reaches_the_client_before_the_backend_has_finished() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let backend_port = listener.local_addr().unwrap().port();
    let (first_seen, release) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut sink = [0u8; 1024];
        let _ = stream.read(&mut sink).await.unwrap();
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n8\r\ndata: 1\n\r\n",
            )
            .await
            .unwrap();
        let _ = release.await;
        stream
            .write_all(b"8\r\ndata: 2\n\r\n0\r\n\r\n")
            .await
            .unwrap();
    });

    let fixture = Fixture::start(backend_port).await;
    let mut client = TcpStream::connect(("127.0.0.1", fixture.port()))
        .await
        .unwrap();
    client
        .write_all(get("/api/machine/events", fixture.port()).as_bytes())
        .await
        .unwrap();

    let mut received = String::new();
    let mut buffer = [0u8; 1024];
    while !received.contains("data: 1") {
        let read = timeout(Duration::from_secs(5), client.read(&mut buffer))
            .await
            .expect("the first event arrives while the stream is still open")
            .unwrap();
        assert!(read > 0, "the stream ended early");
        received.push_str(&String::from_utf8_lossy(&buffer[..read]));
    }
    assert!(!received.contains("data: 2"));
    assert!(received
        .to_ascii_lowercase()
        .contains("content-type: application/json"));
    assert!(!received.to_ascii_lowercase().contains("text/event-stream"));

    first_seen.send(()).unwrap();
    let mut rest = Vec::new();
    timeout(Duration::from_secs(5), client.read_to_end(&mut rest))
        .await
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&rest).contains("data: 2"));
}

#[tokio::test]
async fn the_frontend_is_served_with_the_page_headers_and_the_cache_rules_of_its_path() {
    let fixture = Fixture::start(0).await;
    let port = fixture.port();

    let root = send(port, &get("/", port)).await;
    let deep = send(port, &get("/boards/7/cards?x=1", port)).await;
    let asset = send(port, &get("/assets/app.js", port)).await;
    let missing = send(port, &get("/assets/gone.js", port)).await;

    assert_eq!(
        (root.status, root.body.as_str()),
        (200, "<html>board</html>")
    );
    assert_eq!(root.header("cache-control"), Some("no-cache"));
    assert_eq!(
        root.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    for (name, value) in PAGE {
        assert_eq!(root.header(name), Some(*value), "{name}");
    }
    assert_eq!(deep.body, root.body);
    assert_eq!(asset.status, 200);
    assert_eq!(
        asset.header("cache-control"),
        Some("public, max-age=31536000, immutable")
    );
    assert_eq!(missing.status, 404);
    assert_eq!(
        missing.header("cache-control"),
        Some("public, max-age=31536000, immutable")
    );

    let write = send(
        port,
        &format!("POST /nowhere HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
    )
    .await;
    assert_eq!(write.status, 405);
    assert_eq!(write.header("cache-control"), Some("no-cache"));
}

#[tokio::test]
async fn a_path_that_leaves_the_frontend_folder_is_refused() {
    let fixture = Fixture::start(0).await;
    let port = fixture.port();
    for path in [
        "/assets/..%2f..%2fsecret.txt",
        "/assets/%2e%2e/%2e%2e/secret.txt",
    ] {
        let reply = send(port, &get(path, port)).await;
        assert_eq!(reply.status, 400, "{path}");
        assert!(!reply.body.contains("top secret"));
    }
}

#[tokio::test]
async fn a_port_that_is_taken_is_reported_with_the_way_out() {
    let fixture = Fixture::start(0).await;
    let error = Proxy::start(ProxyConfig {
        port: fixture.port(),
        user_name: "ines".into(),
        secret: SECRET.into(),
        frontend: fixture._site.path().to_path_buf(),
        upstream_port: Arc::new(|| None),
    })
    .await
    .err()
    .expect("the port is in use");
    assert_eq!(
        error.to_string(),
        format!(
            "Port {} in use; change port in settings.json",
            fixture.port()
        )
    );
}

fn ranged(method: &str, path: &str, port: u16, extra: &str) -> String {
    format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{extra}Connection: close\r\n\r\n"
    )
}

#[tokio::test]
async fn a_file_answers_a_byte_range_with_the_slice_and_says_where_it_sits() {
    let fixture = Fixture::start(0).await;
    let port = fixture.port();

    let whole = send(port, &get("/assets/app.js", port)).await;
    let first = send(
        port,
        &ranged("GET", "/assets/app.js", port, "Range: bytes=0-6\r\n"),
    )
    .await;
    let tail = send(
        port,
        &ranged("GET", "/assets/app.js", port, "Range: bytes=-2\r\n"),
    )
    .await;
    let page = send(port, &ranged("GET", "/", port, "Range: bytes=0-5\r\n")).await;

    assert_eq!(whole.header("accept-ranges"), Some("bytes"));
    assert_eq!((first.status, first.body.as_str()), (206, "console"));
    assert_eq!(first.header("content-range"), Some("bytes 0-6/14"));
    assert_eq!(first.header("content-length"), Some("7"));
    assert_eq!(first.header("accept-ranges"), Some("bytes"));
    assert_eq!(
        first.header("cache-control"),
        Some("public, max-age=31536000, immutable")
    );
    assert_eq!(first.header("content-type"), whole.header("content-type"));
    assert_eq!((tail.status, tail.body.as_str()), (206, "1)"));
    assert_eq!(tail.header("content-range"), Some("bytes 12-13/14"));
    assert_eq!((page.status, page.body.as_str()), (206, "<html>"));
    assert_eq!(page.header("cache-control"), Some("no-cache"));
}

#[tokio::test]
async fn a_range_nobody_can_satisfy_is_refused_with_416_and_no_file_headers() {
    let fixture = Fixture::start(0).await;
    let port = fixture.port();

    let beyond = send(
        port,
        &ranged("GET", "/assets/app.js", port, "Range: bytes=99-\r\n"),
    )
    .await;
    let malformed = send(
        port,
        &ranged("GET", "/assets/app.js", port, "Range: bytes=abc\r\n"),
    )
    .await;

    assert_eq!(beyond.status, 416);
    assert_eq!(beyond.header("content-range"), Some("bytes */14"));
    assert_eq!(beyond.body, "invalid range: failed to overlap\n");
    assert_eq!(malformed.status, 416);
    assert_eq!(malformed.header("content-range"), None);
    assert_eq!(malformed.body, "invalid range\n");
    for reply in [&beyond, &malformed] {
        assert_eq!(
            reply.header("content-type"),
            Some("text/plain; charset=utf-8")
        );
        assert_eq!(reply.header("accept-ranges"), None);
        assert_eq!(reply.header("cache-control"), None);
        assert_eq!(reply.header("x-content-type-options"), Some("nosniff"));
    }
}

#[tokio::test]
async fn several_ranges_answer_with_a_multipart_body_that_names_its_boundary() {
    let fixture = Fixture::start(0).await;
    let port = fixture.port();

    let reply = send(
        port,
        &ranged("GET", "/assets/app.js", port, "Range: bytes=0-1,4-5\r\n"),
    )
    .await;

    let content_type = reply.header("content-type").unwrap();
    let boundary = content_type
        .strip_prefix("multipart/byteranges; boundary=")
        .expect("a multipart content type");
    assert_eq!(reply.status, 206);
    assert_eq!(
        reply.body,
        format!(
            "--{boundary}\r\nContent-Range: bytes 0-1/14\r\nContent-Type: text/javascript; charset=utf-8\r\n\r\nco\
             \r\n--{boundary}\r\nContent-Range: bytes 4-5/14\r\nContent-Type: text/javascript; charset=utf-8\r\n\r\nol\
             \r\n--{boundary}--\r\n"
        )
    );
    assert_eq!(
        reply.header("content-length"),
        Some(reply.body.len().to_string().as_str())
    );
    assert_eq!(reply.header("content-range"), None);
}

#[tokio::test]
async fn head_with_a_range_reports_the_slice_without_sending_it() {
    let fixture = Fixture::start(0).await;
    let port = fixture.port();

    let reply = send(
        port,
        &ranged("HEAD", "/assets/app.js", port, "Range: bytes=0-6\r\n"),
    )
    .await;

    assert_eq!((reply.status, reply.body.as_str()), (206, ""));
    assert_eq!(reply.header("content-length"), Some("7"));
    assert_eq!(reply.header("content-range"), Some("bytes 0-6/14"));
}

#[tokio::test]
async fn an_if_range_validator_is_never_matched_so_the_whole_file_answers() {
    let fixture = Fixture::start(0).await;
    let port = fixture.port();

    let reply = send(
        port,
        &ranged(
            "GET",
            "/assets/app.js",
            port,
            "Range: bytes=0-6\r\nIf-Range: \"anything\"\r\n",
        ),
    )
    .await;

    assert_eq!((reply.status, reply.body.as_str()), (200, "console.log(1)"));
    assert_eq!(reply.header("content-range"), None);
}

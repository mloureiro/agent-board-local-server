use std::collections::BTreeMap;
use std::io;
use std::net::TcpListener;
use std::path::Path;
use std::time::Duration;

use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout, Instant};

use crate::paths::{pglite_dir, Resources};
use crate::settings::Settings;

pub const HEALTH_POLL: Duration = Duration::from_millis(250);
pub const HEALTH_LIMIT: Duration = Duration::from_secs(180);
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

const PATH_SAFE: AsciiSet = NON_ALPHANUMERIC
    .remove(b'/')
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

fn pglite_url(role: &str, data_dir: &Path) -> String {
    let path = utf8_percent_encode(&data_dir.to_string_lossy(), &PATH_SAFE).to_string();
    format!("pglite://{role}local{path}")
}

/// The whole environment of the backend. Nothing from the app's own environment is passed on,
/// and neither the development sign-in bypass nor the self-update version is ever set.
pub fn environment(
    settings: &Settings,
    proxy_secret: &str,
    port: u16,
    resources: &Resources,
) -> BTreeMap<String, String> {
    let database = pglite_dir(&settings.data_dir);
    BTreeMap::from([
        ("NODE_ENV".into(), "production".into()),
        ("HOST".into(), "127.0.0.1".into()),
        ("PORT".into(), port.to_string()),
        ("PROXY_SECRET".into(), proxy_secret.into()),
        ("DATABASE_URL".into(), pglite_url("", &database)),
        (
            "APP_DATABASE_URL".into(),
            pglite_url("board_app:x@", &database),
        ),
        (
            "MIGRATIONS_DIR".into(),
            resources.migrations().to_string_lossy().into_owned(),
        ),
        (
            "PGLITE_ASSETS_DIR".into(),
            resources.pglite_assets().to_string_lossy().into_owned(),
        ),
        ("ALLOWED_GROUPS".into(), "admins".into()),
    ])
}

/// The previous port is reused when it is still free, so a restarted backend keeps its address.
pub fn free_port(preferred: Option<u16>) -> io::Result<u16> {
    if let Some(port) = preferred {
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return Ok(port);
        }
    }
    Ok(TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port())
}

async fn healthy(port: u16) -> bool {
    let probe = async {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
        stream
            .write_all(b"GET /healthz HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .await?;
        let mut status = [0u8; 12];
        stream.read_exact(&mut status).await?;
        Ok::<_, io::Error>(&status == b"HTTP/1.1 200")
    };
    matches!(timeout(PROBE_TIMEOUT, probe).await, Ok(Ok(true)))
}

/// A first start runs initdb and every migration, hence the long limit.
pub async fn wait_healthy(port: u16, poll: Duration, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if healthy(port).await {
            return true;
        }
        sleep(poll).await;
    }
    false
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tokio::net::TcpListener as AsyncListener;

    use super::*;

    #[test]
    fn the_backend_environment_is_exactly_this() {
        let settings = Settings {
            user_name: "ines".into(),
            data_dir: PathBuf::from("/Users/ines/Library/Application Support/app/data"),
            port: 4785,
            start_at_login: true,
        };
        let resources = Resources::new("/App/Contents/Resources");
        let url = "/Users/ines/Library/Application%20Support/app/data/pglite";

        assert_eq!(
            environment(&settings, "s3cret", 51000, &resources),
            BTreeMap::from([
                ("ALLOWED_GROUPS".into(), "admins".into()),
                (
                    "APP_DATABASE_URL".into(),
                    format!("pglite://board_app:x@local{url}")
                ),
                ("DATABASE_URL".into(), format!("pglite://local{url}")),
                ("HOST".into(), "127.0.0.1".into()),
                (
                    "MIGRATIONS_DIR".into(),
                    "/App/Contents/Resources/resources/drizzle".into()
                ),
                ("NODE_ENV".into(), "production".into()),
                (
                    "PGLITE_ASSETS_DIR".into(),
                    "/App/Contents/Resources/resources/pglite".into()
                ),
                ("PORT".into(), "51000".into()),
                ("PROXY_SECRET".into(), "s3cret".into()),
            ])
        );
    }

    async fn serve_once(reply: &'static str) -> u16 {
        let listener = AsyncListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut request = [0u8; 256];
                let _ = stream.read(&mut request).await;
                let _ = stream.write_all(reply.as_bytes()).await;
            }
        });
        port
    }

    #[tokio::test]
    async fn only_a_200_from_healthz_counts_as_ready() {
        let ready = serve_once("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
        let failing =
            serve_once("HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n").await;

        assert!(wait_healthy(ready, Duration::from_millis(10), Duration::from_secs(2)).await);
        assert!(
            !wait_healthy(
                failing,
                Duration::from_millis(10),
                Duration::from_millis(200)
            )
            .await
        );
    }
}

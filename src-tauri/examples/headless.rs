use std::path::PathBuf;
use std::sync::Arc;

use agent_board_local_server::lock::{self, LockError};
use agent_board_local_server::paths::{AppPaths, Resources, Sidecars};
use agent_board_local_server::runtime::{self, Config, BACKEND};
use agent_board_local_server::secrets::MemoryStore;
use agent_board_local_server::supervisor::{Status, StatusBoard};
use tokio::signal::unix::{signal, SignalKind};

const PACKAGING: &str = env!("CARGO_MANIFEST_DIR");

struct Args {
    app_root: PathBuf,
    port: Option<u16>,
}

fn parse() -> Result<Args, String> {
    let mut app_root = None;
    let mut port = None;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--app-root" => app_root = Some(PathBuf::from(value)),
            "--port" => {
                port = Some(
                    value
                        .parse()
                        .map_err(|_| format!("{value} is not a port"))?,
                )
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(Args {
        app_root: app_root.ok_or("--app-root <dir> is required")?,
        port,
    })
}

#[tokio::main]
async fn main() {
    let args = match parse() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    let paths = AppPaths::new(&args.app_root);
    let lock = match lock::acquire(&paths.lock_file()) {
        Ok(lock) => lock,
        Err(LockError::AlreadyRunning) => {
            eprintln!("another instance holds {}", paths.lock_file().display());
            std::process::exit(1);
        }
        Err(LockError::Io(error)) => {
            eprintln!("cannot take the app lock: {error}");
            std::process::exit(1);
        }
    };

    let config = Config {
        sidecars: Sidecars::beside(&PathBuf::from(PACKAGING).join("binaries")),
        resources: Resources::new(PACKAGING),
        login_name: std::env::var("USER").unwrap_or_else(|_| "user".into()),
        port_override: args.port,
        paths,
    };
    let board = Arc::new(StatusBoard::default());
    let mut changes = board.subscribe();
    let reporter = board.clone();
    tokio::spawn(async move {
        while changes.changed().await.is_ok() {
            match reporter.get(BACKEND) {
                Some(Status::Running { port }) => println!("backend running on 127.0.0.1:{port}"),
                Some(status) => println!("{}", status.label("Backend")),
                None => {}
            }
        }
    });

    let handle = match runtime::start(config, lock, Arc::new(MemoryStore::default()), board).await {
        Ok(handle) => handle,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };

    let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
    let mut int = signal(SignalKind::interrupt()).expect("SIGINT handler");
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
    handle.stop().await;
    println!("stopped");
}

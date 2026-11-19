use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agent_board_local_server::lock::{self, LockError};
use agent_board_local_server::paths::{AppPaths, Resources, Sidecars, APP_IDENTIFIER};
use agent_board_local_server::runtime::{self, Config, WorkerConfig, BACKEND, PROXY};
use agent_board_local_server::secrets::{Keychain, MemoryStore, SecretStore};
use agent_board_local_server::supervisor::{Status, StatusBoard};
use agent_board_local_server::worker::credentials::run_helper_if_requested;
use agent_board_local_server::worker::processes::{DISPATCH, WORKER_LISTEN, WORKER_WATCH};
use tokio::signal::unix::{signal, SignalKind};

const PACKAGING: &str = env!("CARGO_MANIFEST_DIR");
const PROXY_WAIT: Duration = Duration::from_secs(240);

struct Args {
    app_root: PathBuf,
    port: Option<u16>,
    upstream: Option<u16>,
    worker: bool,
    enrol_from_stdin: bool,
}

fn loopback_port(url: &str) -> Result<u16, String> {
    url.strip_prefix("http://127.0.0.1:")
        .and_then(|port| port.trim_end_matches('/').parse().ok())
        .ok_or_else(|| format!("{url} is not an http://127.0.0.1:<port> address"))
}

fn parse() -> Result<Args, String> {
    let mut app_root = None;
    let mut port = None;
    let mut upstream = None;
    let mut worker = false;
    let mut enrol_from_stdin = false;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--worker" => worker = true,
            "--enrol-from-stdin" => enrol_from_stdin = true,
            "--app-root" | "--port" | "--upstream" => {
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
                    _ => upstream = Some(loopback_port(&value)?),
                }
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    let app_root = std::path::absolute(app_root.ok_or("--app-root <dir> is required")?)
        .map_err(|error| error.to_string())?;
    Ok(Args {
        app_root,
        port,
        upstream,
        worker: worker || enrol_from_stdin,
        enrol_from_stdin,
    })
}

/// One Keychain service per scratch app root, so runs never share or touch the app's own items.
fn worker_keychain(app_root: &Path) -> Keychain {
    Keychain::with_service(format!("{APP_IDENTIFIER}.headless:{}", app_root.display()))
}

/// The CLI runs this executable as its credentials command with the worker environment, which names the root.
fn helper_keychain() -> Keychain {
    let root = std::env::var_os("AGENT_BOARD_CONFIG_DIR")
        .map(PathBuf::from)
        .and_then(|config| Some(config.parent()?.parent()?.to_path_buf()))
        .unwrap_or_default();
    worker_keychain(&root)
}

fn read_credentials_from_stdin() -> Result<(String, String), String> {
    let mut lines = std::io::stdin().lock().lines();
    let mut next = |what: &str| {
        lines
            .next()
            .ok_or_else(|| format!("stdin has no {what} line"))?
            .map_err(|error| error.to_string())
    };
    Ok((next("token")?, next("data key")?))
}

#[tokio::main]
async fn main() {
    if let Some(code) = run_helper_if_requested(std::env::args_os(), &helper_keychain()) {
        std::process::exit(code);
    }
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

    let worker_secrets: Arc<dyn SecretStore> = if args.worker {
        Arc::new(worker_keychain(&args.app_root))
    } else {
        Arc::new(MemoryStore::default())
    };
    let worker = Some(WorkerConfig {
        secrets: worker_secrets,
        app_executable: std::env::current_exe().expect("the runner knows its own path"),
    });
    let config = Config {
        sidecars: Sidecars::beside(&PathBuf::from(PACKAGING).join("binaries")),
        resources: Resources::new(PACKAGING),
        login_name: std::env::var("USER").unwrap_or_else(|_| "user".into()),
        port_override: args.port,
        upstream_override: args.upstream,
        worker,
        paths,
    };
    let board = Arc::new(StatusBoard::default());
    let mut changes = board.subscribe();
    let reporter = board.clone();
    tokio::spawn(async move {
        let mut printed = BTreeMap::new();
        while changes.changed().await.is_ok() {
            for (name, title) in [
                (BACKEND, "backend"),
                (PROXY, "proxy"),
                (WORKER_LISTEN, "worker listener"),
                (WORKER_WATCH, "worker watch"),
                (DISPATCH, "last dispatch tick"),
            ] {
                let Some(status) = reporter.get(name) else {
                    continue;
                };
                let line = match status {
                    Status::Running { port } => format!("{title} running on 127.0.0.1:{port}"),
                    other => other.label(title),
                };
                if printed.insert(name, line.clone()).as_deref() != Some(line.as_str()) {
                    println!("{line}");
                }
            }
        }
    });

    let handle = match runtime::start(
        config,
        lock,
        Arc::new(MemoryStore::default()),
        board.clone(),
    )
    .await
    {
        Ok(handle) => handle,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };

    let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
    let mut int = signal(SignalKind::interrupt()).expect("SIGINT handler");
    let exit_code = {
        let mut enrolment =
            std::pin::pin!(enrol_from_stdin(&handle, &board, args.enrol_from_stdin));
        let mut enrolled = false;
        loop {
            tokio::select! {
                result = &mut enrolment, if !enrolled => {
                    enrolled = true;
                    match result {
                        Ok(true) => println!("worker enrolled"),
                        Ok(false) => {}
                        Err(message) => {
                            println!("worker enrolment failed: {message}");
                            break 1;
                        }
                    }
                }
                _ = term.recv() => break 0,
                _ = int.recv() => break 0,
            }
        }
    };
    handle.stop().await;
    println!("stopped");
    std::process::exit(exit_code);
}

async fn enrol_from_stdin(
    handle: &runtime::Handle,
    board: &StatusBoard,
    requested: bool,
) -> Result<bool, String> {
    if !requested {
        return Ok(false);
    }
    println!("waiting for the machine token and the data key on stdin, one per line");
    let read = tokio::task::spawn_blocking(read_credentials_from_stdin);
    let mut changes = board.subscribe();
    let ready = tokio::time::timeout(PROXY_WAIT, async {
        while !matches!(board.get(PROXY), Some(Status::Running { .. })) {
            if changes.changed().await.is_err() {
                return;
            }
        }
    })
    .await;
    ready.map_err(|_| "the proxy did not start".to_owned())?;
    let (token, data_key) = read.await.map_err(|error| error.to_string())??;
    handle
        .enrol(&token, &data_key)
        .await
        .map_err(|error| error.to_string())?;
    Ok(true)
}

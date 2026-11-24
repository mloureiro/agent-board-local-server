use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::BufRead;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use agent_board_local_server::activity::Activity;
use agent_board_local_server::applog::LogFile;
use agent_board_local_server::lock::{self, LockError};
use agent_board_local_server::paths::{AppPaths, Resources, Sidecars, APP_LOG};
use agent_board_local_server::runtime::{self, Config, WorkerConfig};
use agent_board_local_server::secrets::{Keychain, MemoryStore, SecretStore};
use agent_board_local_server::settings::login_name;
use agent_board_local_server::supervisor::table::PROXY;
use agent_board_local_server::supervisor::{changed_labels, Status, StatusBoard};
use agent_board_local_server::uninstall;
use agent_board_local_server::worker::credentials::{helper_keychain, run_helper_if_requested};
use tokio::signal::unix::{signal, SignalKind};

const PACKAGING: &str = env!("CARGO_MANIFEST_DIR");
const PROXY_WAIT: Duration = Duration::from_secs(240);

struct Args {
    app_root: PathBuf,
    port: Option<u16>,
    upstream: Option<u16>,
    worker: bool,
    enrol_from_stdin: bool,
    core_dir: PathBuf,
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
    let mut core_dir = PathBuf::from(PACKAGING);
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--worker" => worker = true,
            "--enrol-from-stdin" => enrol_from_stdin = true,
            "--app-root" | "--port" | "--upstream" | "--core" => {
                let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
                match flag.as_str() {
                    "--app-root" => app_root = Some(PathBuf::from(value)),
                    "--core" => {
                        core_dir = std::path::absolute(value).map_err(|error| error.to_string())?
                    }
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
        core_dir,
    })
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

fn uninstall_if_requested() -> Option<i32> {
    let mut rest: Vec<OsString> = Vec::new();
    let mut app_root = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--app-root" {
            app_root = Some(args.next()?);
        } else {
            rest.push(arg);
        }
    }
    match uninstall::parse_args(&rest)? {
        Ok(options) => {
            let Some(root) = app_root.and_then(|root| std::path::absolute(root).ok()) else {
                eprintln!("--uninstall needs --app-root <dir>");
                return Some(2);
            };
            if AppPaths::default_root().as_deref() == Some(root.as_path()) {
                eprintln!("the runner never uninstalls the default app root");
                return Some(2);
            }
            Some(uninstall::run_at(&root, options))
        }
        Err(usage) => {
            eprintln!("{usage}");
            Some(2)
        }
    }
}

#[tokio::main]
async fn main() {
    let helper_store = helper_keychain();
    if let Some(code) = run_helper_if_requested(
        std::env::args_os(),
        helper_store.as_ref().map(|store| store as &dyn SecretStore),
    ) {
        std::process::exit(code);
    }
    if let Some(code) = uninstall_if_requested() {
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
    let _lock = match lock::acquire(&paths.lock_file()) {
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
        Arc::new(Keychain::for_root(&args.app_root))
    } else {
        Arc::new(MemoryStore::default())
    };
    let worker = Some(WorkerConfig {
        secrets: worker_secrets,
        app_executable: std::env::current_exe().expect("the runner knows its own path"),
    });
    let config = Config {
        sidecars: Sidecars::beside(&args.core_dir.join("binaries")),
        resources: Resources::new(&args.core_dir),
        login_name: login_name(),
        app_log: LogFile::new(paths.log_file(APP_LOG)),
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
            for line in changed_labels(&reporter, &mut printed) {
                println!("{line}");
            }
        }
    });

    let activity = Arc::new(Activity::default());
    let handle = match runtime::start(config, board.clone(), activity.clone()).await {
        Ok(handle) => handle,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };

    let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
    let mut int = signal(SignalKind::interrupt()).expect("SIGINT handler");
    let mut back_up = signal(SignalKind::user_defined1()).expect("SIGUSR1 handler");
    let exit_code = {
        let mut enrolment =
            std::pin::pin!(enrol_from_stdin(&handle, &board, args.enrol_from_stdin));
        let mut enrolled = false;
        loop {
            tokio::select! {
                result = &mut enrolment, if !enrolled => {
                    enrolled = true;
                    match result {
                        Ok(_) => {}
                        Err(message) => {
                            println!("worker enrolment failed: {message}");
                            break 1;
                        }
                    }
                }
                _ = back_up.recv() => match handle.back_up_now().await {
                    Ok(path) => println!("backup written to {}", path.display()),
                    Err(error) => println!("backup failed: {error}"),
                },
                _ = term.recv() => break 0,
                _ = int.recv() => break 0,
            }
        }
    };
    activity.quit();
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
    let done = handle
        .enrol(&token, &data_key)
        .await
        .map_err(|error| error.to_string())?;
    println!("worker enrolled as {}", done.machine.unwrap_or_default());
    loop {
        let next = tokio::task::spawn_blocking(read_credentials_from_stdin).await;
        let Ok(Ok((token, data_key))) = next else {
            return Ok(true);
        };
        match handle.enrol(&token, &data_key).await {
            Ok(done) => println!("worker re-enrolled as {}", done.machine.unwrap_or_default()),
            Err(error) => println!("worker re-enrolment failed: {error}"),
        }
    }
}

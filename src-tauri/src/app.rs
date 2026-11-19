use std::sync::Arc;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, RunEvent};
use tokio::sync::Mutex;

use crate::lock::{self, LockError};
use crate::paths::{AppPaths, Resources, Sidecars};
use crate::runtime::{self, Config, Handle, BACKEND};
use crate::secrets::Keychain;
use crate::settings::Settings;
use crate::supervisor::StatusBoard;

const QUIT: &str = "quit";

type Running = Arc<Mutex<Option<Handle>>>;

fn login_name() -> String {
    std::env::var("USER").unwrap_or_else(|_| "user".into())
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let Some(root) = AppPaths::default_root() else {
                eprintln!("HOME is not set; the app root cannot be found");
                std::process::exit(1);
            };
            let paths = AppPaths::new(root);
            let lock = match lock::acquire(&paths.lock_file()) {
                Ok(lock) => lock,
                Err(LockError::AlreadyRunning) => std::process::exit(0),
                Err(LockError::Io(error)) => {
                    eprintln!("cannot take the app lock: {error}");
                    std::process::exit(1);
                }
            };

            let status =
                MenuItem::with_id(app, "status", "Backend: starting", false, None::<&str>)?;
            let quit = MenuItem::with_id(app, QUIT, "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&status, &quit])?;

            let board = Arc::new(StatusBoard::default());
            let running: Running = Arc::new(Mutex::new(None));
            let startup = running
                .clone()
                .try_lock_owned()
                .expect("nobody else holds the lock yet");
            app.manage(running);

            TrayIconBuilder::new()
                .icon(tauri::include_image!("icons/tray.png"))
                .icon_as_template(true)
                .tooltip("agent-board local server")
                .menu(&menu)
                .on_menu_event(|app, event| {
                    if event.id().as_ref() == QUIT {
                        quit_app(app);
                    }
                })
                .build(app)?;

            let mut changes = board.subscribe();
            let reader = board.clone();
            tauri::async_runtime::spawn(async move {
                while changes.changed().await.is_ok() {
                    if let Some(current) = reader.get(BACKEND) {
                        let label = current.label("Backend");
                        eprintln!("{label}");
                        let _ = status.set_text(label);
                    }
                }
            });

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut startup = startup;
                let resources = handle.path().resource_dir().unwrap_or_default();
                let exe_dir = std::env::current_exe()
                    .ok()
                    .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
                    .unwrap_or_default();
                let config = Config {
                    sidecars: Sidecars::beside(&exe_dir),
                    resources: Resources::new(resources),
                    login_name: login_name(),
                    port_override: None,
                    paths: paths.clone(),
                };
                match runtime::start(config, lock, Arc::new(Keychain), board.clone()).await {
                    Ok(running) => {
                        apply_autostart(&handle, &paths);
                        *startup = Some(running);
                    }
                    Err(error) => eprintln!("{error}"),
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("the app could not start")
        .run(|_app, event| {
            // Tauri asks to exit when no window is left, and this app never has one; only Quit carries a code.
            if let RunEvent::ExitRequested {
                api, code: None, ..
            } = event
            {
                api.prevent_exit();
            }
        });
}

/// The startup guard on the shared slot makes a quit that arrives mid-start wait until what started can be stopped.
fn quit_app(app: &AppHandle) {
    let running = app.state::<Running>().inner().clone();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Some(handle) = running.lock().await.take() {
            handle.stop().await;
        }
        app.exit(0);
    });
}

/// A development build would register a login item that points into the build directory.
fn apply_autostart(app: &AppHandle, paths: &AppPaths) {
    if cfg!(debug_assertions) {
        return;
    }
    use tauri_plugin_autostart::ManagerExt;
    let Ok(settings) = Settings::load_or_create(paths, &login_name()) else {
        return;
    };
    let launcher = app.autolaunch();
    let result = if settings.start_at_login {
        launcher.enable()
    } else {
        launcher.disable()
    };
    if let Err(error) = result {
        eprintln!("cannot update the login item: {error}");
    }
}

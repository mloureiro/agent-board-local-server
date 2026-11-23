use std::path::{Path, PathBuf};

pub const APP_IDENTIFIER: &str = "dev.mloureiro.agent-board-local-server";
pub const ROOT_ENV: &str = "AGENT_BOARD_LOCAL_SERVER_ROOT";

#[derive(Debug, Clone)]
pub struct AppPaths {
    root: PathBuf,
}

impl AppPaths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn default_root() -> Option<PathBuf> {
        let home = std::env::var_os("HOME")?;
        Some(
            PathBuf::from(home)
                .join("Library/Application Support")
                .join(APP_IDENTIFIER),
        )
    }

    /// `AGENT_BOARD_LOCAL_SERVER_ROOT` moves every file of the app to another folder, so a build can run
    /// beside an installed one; the Keychain items move with it (see `Keychain::for_root`).
    pub fn configured_root() -> Option<PathBuf> {
        match std::env::var_os(ROOT_ENV) {
            Some(root) if !root.is_empty() => std::path::absolute(root).ok(),
            _ => Self::default_root(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn settings_file(&self) -> PathBuf {
        self.root.join("settings.json")
    }

    pub fn lock_file(&self) -> PathBuf {
        self.root.join("app.lock")
    }

    pub fn run_dir(&self) -> PathBuf {
        self.root.join("run")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    pub fn pidfile(&self, name: &str) -> PathBuf {
        self.run_dir().join(format!("{name}.pid"))
    }

    pub fn log_file(&self, name: &str) -> PathBuf {
        self.logs_dir().join(format!("{name}.log"))
    }

    pub fn default_data_dir(&self) -> PathBuf {
        self.root.join("data")
    }

    pub fn default_backups_dir(&self) -> PathBuf {
        self.root.join("backups")
    }

    pub fn worker_dir(&self) -> PathBuf {
        self.root.join("worker")
    }

    pub fn worker_bin_dir(&self) -> PathBuf {
        self.worker_dir().join("bin")
    }

    pub fn worker_board(&self) -> PathBuf {
        self.worker_bin_dir().join("board")
    }

    pub fn worker_board_local(&self) -> PathBuf {
        self.worker_bin_dir().join("board-local")
    }

    pub fn worker_credentials(&self) -> PathBuf {
        self.worker_dir()
            .join(crate::worker::credentials::HELPER_LINK)
    }

    pub fn worker_credentials_pending(&self) -> PathBuf {
        self.worker_dir()
            .join(crate::worker::credentials::PENDING_HELPER_LINK)
    }

    pub fn worker_config_dir(&self) -> PathBuf {
        self.worker_dir().join("config")
    }

    pub fn worker_state_dir(&self) -> PathBuf {
        self.worker_dir().join("state")
    }

    pub fn worker_run_dir(&self) -> PathBuf {
        self.worker_dir().join("run")
    }
}

pub fn pglite_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("pglite")
}

pub fn core_commit_file(data_dir: &Path) -> PathBuf {
    data_dir.join("core-commit")
}

#[derive(Debug, Clone)]
pub struct Sidecars {
    pub board_server: PathBuf,
    pub board: PathBuf,
}

impl Sidecars {
    /// A bundled app has the sidecars without a target suffix; the staging folder keeps the suffix.
    pub fn beside(dir: &Path) -> Self {
        Self {
            board_server: locate(dir, "board-server"),
            board: locate(dir, "board"),
        }
    }
}

fn locate(dir: &Path, name: &str) -> PathBuf {
    let plain = dir.join(name);
    if plain.exists() {
        return plain;
    }
    let staged = dir.join(format!("{name}-{}", env!("TARGET_TRIPLE")));
    if staged.exists() {
        return staged;
    }
    plain
}

/// Resources are bundled with their relative path kept, so the folders sit under `resources/`.
#[derive(Debug, Clone)]
pub struct Resources {
    root: PathBuf,
}

impl Resources {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn migrations(&self) -> PathBuf {
        self.root.join("resources/drizzle")
    }

    pub fn frontend(&self) -> PathBuf {
        self.root.join("resources/frontend")
    }

    pub fn core_build(&self) -> PathBuf {
        self.root.join("resources/core-build.json")
    }

    pub fn pglite_assets(&self) -> PathBuf {
        self.root.join("resources/pglite")
    }
}

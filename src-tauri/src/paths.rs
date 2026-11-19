use std::path::{Path, PathBuf};

pub const APP_IDENTIFIER: &str = "dev.mloureiro.agent-board-local-server";

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
}

pub fn pglite_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("pglite")
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

    pub fn pglite_assets(&self) -> PathBuf {
        self.root.join("resources/pglite")
    }
}

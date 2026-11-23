pub mod activity;
mod app;
pub mod applog;
pub mod backend;
pub mod core_build;
pub mod enrol_window;
pub mod localtime;
pub mod lock;
pub mod paths;
pub mod proxy;
pub mod runtime;
pub mod secrets;
pub mod settings;
pub mod supervisor;
pub mod tray;
pub mod worker;

pub use app::run;

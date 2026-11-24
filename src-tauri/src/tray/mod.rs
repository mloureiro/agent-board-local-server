mod actions;
mod health;
mod menu;
mod open_board;

pub use actions::handle_click;
pub use health::{first_failing, show as show_health, HealthMark};
pub use menu::{TrayMenu, QUIT};

//! Local HTTP host composition for panes, screens and desktop APIs.

mod http;
mod collab_env;
#[cfg(test)]
mod collab_contract_tests;
mod register;
pub mod pane_info;
pub use http::{spawn_http_server, spawn_http_server_opts, push_viewer_control};
pub use collab_env::install_collab_env;
pub use register::unregister_clients;
#[cfg(test)]
mod remote_contract_tests;

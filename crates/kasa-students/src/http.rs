use std::sync::Arc;
use kasa_socket::backend::{Backend, resolve_cwd};
use axum::{extract::Query, http::header, response::IntoResponse, routing::{get, post}, Json};
pub mod schedule;
pub mod tasks;
pub use schedule::{ScheduleItem, schedule_add, schedule_delete, schedule_snapshot, schedule_toggle};
pub use tasks::{PaneTaskView, pane_tasks_snapshot};
#[cfg(test)]
use kasa_socket::test_support;

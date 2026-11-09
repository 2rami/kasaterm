//! Browser integration stays in a window-free host crate so the independent
//! Kasa Browser app and Chrome can share device routing without sharing UI or profiles.

pub mod route;
pub mod target;
pub mod phone;

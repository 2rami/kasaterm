//! Account-to-account rooms: read-only views of published conversations and immutable fork material.
//! Joining a room never grants control of the publisher's machine.

pub mod store;
pub mod client;

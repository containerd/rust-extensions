//! `containerd-shim-client` contains TTRPC bindings and client/server code to interact with
//! containerd's runtime v2 shims.
//!
//! This crate relies on [ttrpc-rust](https://github.com/containerd/ttrpc-rust) crate to generate
//! protobuf definitions and re-exports the TTRPC client for convenience.
//!
//! Here is a quick example:
//! ```rust
//! use containerd_shim_client as client;
//!
//! use client::api;
//! use client::ttrpc::context::Context;
//!
//! // Create TTRPC client
//! let client = client::Client::connect("unix:///socket.sock")?;
//!
//! // Get task client
//! let task_client = client::TaskClient::new(client);
//! let context = Context::default();
//!
//! // Send request and receive response
//! let request = api::ConnectRequest::default();
//! let response = task_client.connect(Context::default(), &request);
//! ```

// Supress warning: redundant field names in struct initialization
#![allow(clippy::redundant_field_names)]

pub use protobuf;
pub use ttrpc;

#[rustfmt::skip]
pub mod events;
#[rustfmt::skip]
pub mod shim;

/// Includes event names shims can publish to containerd.
pub mod topics;

/// TTRPC client reexport for easier access.
pub use ttrpc::Client;

pub use shim::shim as api;
pub use shim::shim_ttrpc::{Task, TaskClient};

/// Shim events.
pub use shim::events_ttrpc::{Events, EventsClient};

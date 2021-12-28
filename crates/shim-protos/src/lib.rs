/*
   Copyright The containerd Authors.

   Licensed under the Apache License, Version 2.0 (the "License");
   you may not use this file except in compliance with the License.
   You may obtain a copy of the License at

       http://www.apache.org/licenses/LICENSE-2.0

   Unless required by applicable law or agreed to in writing, software
   distributed under the License is distributed on an "AS IS" BASIS,
   WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
   See the License for the specific language governing permissions and
   limitations under the License.
*/

//! `containerd-shim-protos` contains TTRPC bindings and client/server code to interact with
//! containerd's runtime v2 shims.
//!
//! This crate relies on [ttrpc-rust](https://github.com/containerd/ttrpc-rust) crate to generate
//! protobuf definitions and re-exports the TTRPC client for convenience.
//!
//! Here is a quick example:
//! ```no_run
//! use containerd_shim_protos as client;
//!
//! use client::api;
//! use client::ttrpc::context::Context;
//!
//! // Create TTRPC client
//! let client = client::Client::connect("unix:///socket.sock").unwrap();
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
pub mod cgroups;
#[rustfmt::skip]
pub mod shim;

/// Includes event names shims can publish to containerd.
pub mod topics;

/// TTRPC client reexport for easier access.
pub use ttrpc::Client;

/// Shim task service.
pub use shim::shim_ttrpc::{create_task, Task, TaskClient};

/// Shim events service.
pub use shim::events_ttrpc::{create_events, Events, EventsClient};

/// Reexport auto-generated public data structures.
pub mod api {
    pub use crate::shim::empty::*;
    pub use crate::shim::events::*;
    pub use crate::shim::mount::*;
    pub use crate::shim::shim::*;
    pub use crate::shim::task::*;
}

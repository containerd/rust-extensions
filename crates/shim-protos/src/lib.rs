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

#![cfg_attr(feature = "docs", doc = include_str!("../README.md"))]
#![allow(warnings)]

pub use protobuf;
pub use ttrpc;

pub mod cgroups;
pub mod events;
#[cfg(feature = "sandbox")]
mod sandbox;
pub mod shim;
pub mod types;
pub mod windows;

/// Includes event names shims can publish to containerd.
pub mod topics;

pub mod shim_sync {
    /// TTRPC client reexport for easier access.
    pub use ttrpc::Client;

    /// Shim events service.
    pub use crate::shim::events_ttrpc::{create_events, Events, EventsClient};
    /// Shim task service.
    pub use crate::shim::shim_ttrpc::{create_task, Task, TaskClient};
}

pub use shim_sync::*;

#[cfg(feature = "async")]
pub mod shim_async {
    /// TTRPC client reexport for easier access.
    pub use ttrpc::asynchronous::Client;

    /// Shim events service.
    pub use crate::shim::events_ttrpc_async::{create_events, Events, EventsClient};
    /// Shim task service.
    pub use crate::shim::shim_ttrpc_async::{create_task, Task, TaskClient};
}

/// Reexport auto-generated public data structures.
pub mod api {
    pub use crate::shim::{empty::*, events::*, mount::*, shim::*, task::*};
}

#[cfg(feature = "sandbox")]
pub use sandbox::sandbox as sandbox_api;

#[cfg(feature = "sandbox")]
pub mod sandbox_sync {
    pub use crate::sandbox::sandbox_ttrpc::*;
}

#[cfg(all(feature = "sandbox", feature = "async"))]
pub mod sandbox_async {
    pub use crate::sandbox::sandbox_async::*;
}

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

pub mod oci {
    include!(concat!(env!("OUT_DIR"), "/shim/oci.rs"));
}

pub mod events {
    include!(concat!(env!("OUT_DIR"), "/shim/events.rs"));
}

pub mod events_ttrpc {
    include!(concat!(env!("OUT_DIR"), "/shim/events_ttrpc.rs"));
}

#[cfg(feature = "async")]
pub mod events_ttrpc_async {
    include!(concat!(env!("OUT_DIR"), "/shim_async/events_ttrpc.rs"));
}

pub mod shim {
    include!(concat!(env!("OUT_DIR"), "/shim/shim.rs"));
}

pub mod shim_ttrpc {
    include!(concat!(env!("OUT_DIR"), "/shim/shim_ttrpc.rs"));
}

#[cfg(feature = "async")]
pub mod shim_ttrpc_async {
    include!(concat!(env!("OUT_DIR"), "/shim_async/shim_ttrpc.rs"));
}

pub(crate) mod empty {
    pub use crate::types::empty::*;
}

pub(crate) mod mount {
    pub use crate::types::mount::*;
}

pub(crate) mod task {
    pub use crate::types::task::*;
}

mod fieldpath {
    pub use crate::types::fieldpath::*;
}

mod gogo {
    pub use crate::types::gogo::*;
}

/// Shim events service.
pub use events_ttrpc::{create_events, Events, EventsClient};
/// Shim task service.
pub use shim_ttrpc::{create_task, Task, TaskClient};

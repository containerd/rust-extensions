// Supress warning: redundant field names in struct initialization
#![allow(clippy::redundant_field_names)]

pub use protobuf;
pub use ttrpc;

#[rustfmt::skip]
pub mod events;
#[rustfmt::skip]
pub mod shim;

pub mod topics;

pub use ttrpc::Client;

pub use shim::shim as api;
pub use shim::shim_ttrpc::{Task, TaskClient};

pub use shim::events_ttrpc::{Events, EventsClient};

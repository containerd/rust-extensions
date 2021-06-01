// Supress warning: redundant field names in struct initialization
#![allow(clippy::redundant_field_names)]

/// Propagate protobuf module we've used in this crate.
pub use protobuf;

#[rustfmt::skip]
pub mod events;
#[rustfmt::skip]
pub mod shim;

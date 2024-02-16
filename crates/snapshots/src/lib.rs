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
// No way to derive Eq with tonic :(
// See https://github.com/hyperium/tonic/issues/1056
#![allow(clippy::derive_partial_eq_without_eq)]

use std::{collections::HashMap, fmt::Debug, ops::AddAssign, time::SystemTime};

use serde::{Deserialize, Serialize};
use tokio_stream::Stream;
pub use tonic;

mod convert;
mod wrap;

pub use wrap::server;

/// Generated GRPC apis.
pub mod api {
    #[allow(clippy::tabs_in_doc_comments)]
    #[allow(rustdoc::invalid_rust_codeblocks)]

    /// Generated snapshots bindings.
    pub mod snapshots {
        pub mod v1 {
            tonic::include_proto!("containerd.services.snapshots.v1");
        }
    }

    /// Generated `containerd.types` types.
    pub mod types {
        tonic::include_proto!("containerd.types");
    }
}

/// Snapshot kinds.
#[derive(Debug, Serialize, Deserialize, PartialEq, Default)]
pub enum Kind {
    #[default]
    Unknown,
    View,
    Active,
    Committed,
}

/// Information about a particular snapshot.
#[derive(Debug, Serialize, Deserialize)]
pub struct Info {
    /// Active or committed snapshot.
    pub kind: Kind,
    /// Name of key of snapshot.
    pub name: String,
    /// Name of parent snapshot.
    pub parent: String,
    /// Labels for a snapshot.
    pub labels: HashMap<String, String>,
    /// Created time.
    pub created_at: SystemTime,
    /// Last updated time.
    pub updated_at: SystemTime,
}

impl Default for Info {
    fn default() -> Self {
        Info {
            kind: Default::default(),
            name: Default::default(),
            parent: Default::default(),
            labels: Default::default(),
            created_at: SystemTime::now(),
            updated_at: SystemTime::now(),
        }
    }
}

/// Defines statistics for disk resources consumed by the snapshot.
///
// These resources only include the resources consumed by the snapshot itself and does not include
// resources usage by the parent.
#[derive(Debug, Default)]
pub struct Usage {
    /// Number of inodes in use.
    pub inodes: i64,
    /// Provides usage of snapshot in bytes.
    pub size: i64,
}

/// Add the provided usage to the current usage.
impl AddAssign for Usage {
    fn add_assign(&mut self, rhs: Self) {
        self.inodes += rhs.inodes;
        self.size += rhs.size;
    }
}

/// Snapshotter defines the methods required to implement a snapshot snapshotter for
/// allocating, snapshotting and mounting filesystem changesets. The model works
/// by building up sets of changes with parent-child relationships.
///
/// A snapshot represents a filesystem state. Every snapshot has a parent, where
/// the empty parent is represented by the empty string. A diff can be taken
/// between a parent and its snapshot to generate a classic layer.
#[tonic::async_trait]
pub trait Snapshotter: Send + Sync + 'static {
    /// Error type returned from the underlying snapshotter implementation.
    ///
    /// This type must be convertable to GRPC status.
    type Error: Debug + Into<tonic::Status> + Send;

    /// Returns the info for an active or committed snapshot by name or key.
    ///
    /// Should be used for parent resolution, existence checks and to discern
    /// the kind of snapshot.
    async fn stat(&self, key: String) -> Result<Info, Self::Error>;

    /// Update updates the info for a snapshot.
    ///
    /// Only mutable properties of a snapshot may be updated.
    async fn update(
        &self,
        info: Info,
        fieldpaths: Option<Vec<String>>,
    ) -> Result<Info, Self::Error>;

    /// Usage returns the resource usage of an active or committed snapshot
    /// excluding the usage of parent snapshots.
    ///
    /// The running time of this call for active snapshots is dependent on
    /// implementation, but may be proportional to the size of the resource.
    /// Callers should take this into consideration.
    async fn usage(&self, key: String) -> Result<Usage, Self::Error>;

    /// Mounts returns the mounts for the active snapshot transaction identified
    /// by key.
    ///
    /// Can be called on an read-write or readonly transaction. This is
    /// available only for active snapshots.
    ///
    /// This can be used to recover mounts after calling View or Prepare.
    async fn mounts(&self, key: String) -> Result<Vec<api::types::Mount>, Self::Error>;

    /// Creates an active snapshot identified by key descending from the provided parent.
    /// The returned mounts can be used to mount the snapshot to capture changes.
    ///
    /// If a parent is provided, after performing the mounts, the destination will start
    /// with the content of the parent. The parent must be a committed snapshot.
    /// Changes to the mounted destination will be captured in relation to the parent.
    /// The default parent, "", is an empty directory.
    ///
    /// The changes may be saved to a committed snapshot by calling [Snapshotter::commit]. When
    /// one is done with the transaction, [Snapshotter::remove] should be called on the key.
    ///
    /// Multiple calls to [Snapshotter::prepare] or [Snapshotter::view] with the same key should fail.
    async fn prepare(
        &self,
        key: String,
        parent: String,
        labels: HashMap<String, String>,
    ) -> Result<Vec<api::types::Mount>, Self::Error>;

    /// View behaves identically to [Snapshotter::prepare] except the result may not be
    /// committed back to the snapshot snapshotter. View call returns a readonly view on
    /// the parent, with the active snapshot being tracked by the given key.
    ///
    /// This method operates identically to [Snapshotter::prepare], except that mounts returned
    /// may have the readonly flag set. Any modifications to the underlying
    /// filesystem will be ignored. Implementations may perform this in a more
    /// efficient manner that differs from what would be attempted with [Snapshotter::prepare].
    ///
    /// Commit may not be called on the provided key and will return an error.
    /// To collect the resources associated with key, [Snapshotter::remove] must be called with
    /// key as the argument.
    async fn view(
        &self,
        key: String,
        parent: String,
        labels: HashMap<String, String>,
    ) -> Result<Vec<api::types::Mount>, Self::Error>;

    /// Capture the changes between key and its parent into a snapshot identified by name.
    ///
    /// The name can then be used with the snapshotter's other methods to create subsequent snapshots.
    ///
    /// A committed snapshot will be created under name with the parent of the
    /// active snapshot.
    ///
    /// After commit, the snapshot identified by key is removed.
    async fn commit(
        &self,
        name: String,
        key: String,
        labels: HashMap<String, String>,
    ) -> Result<(), Self::Error>;

    /// Remove the committed or active snapshot by the provided key.
    ///
    /// All resources associated with the key will be removed.
    ///
    /// If the snapshot is a parent of another snapshot, its children must be
    /// removed before proceeding.
    async fn remove(&self, key: String) -> Result<(), Self::Error>;

    /// Cleaner defines a type capable of performing asynchronous resource cleanup.
    ///
    /// Cleaner interface should be used by snapshotters which implement fast
    /// removal and deferred resource cleanup. This prevents snapshots from needing
    /// to perform lengthy resource cleanup before acknowledging a snapshot key
    /// has been removed and available for re-use. This is also useful when
    /// performing multi-key removal with the intent of cleaning up all the
    /// resources after each snapshot key has been removed.
    async fn clear(&self) -> Result<(), Self::Error> {
        Ok(())
    }

    /// The type of the stream that returns all snapshots.
    ///
    /// An instance of this type is returned by [`Snapshotter::list`] on success.
    type InfoStream: Stream<Item = Result<Info, Self::Error>> + Send + 'static;

    /// Returns a stream containing all snapshots.
    ///
    /// Once `type_alias_impl_trait` is stabilized or if the implementer is willing to use unstable
    /// features, this function can be implemented using `try_stream` and `yield`. For example, a
    /// function that lists a single snapshot with the default values would be implemented as
    /// follows:
    ///
    ///```ignore
    ///     type InfoStream = impl Stream<Item = Result<Info, Self::Error>> + Send + 'static;
    ///     fn list(&self) -> Result<Self::InfoStream, Self::Error> {
    ///         Ok(async_stream::try_stream! {
    ///             yield Info::default();
    ///         })
    ///     }
    /// ```
    async fn list(
        &self,
        snapshotter: String,
        filters: Vec<String>,
    ) -> Result<Self::InfoStream, Self::Error>;
}

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

//! Trait wrapper to server GRPC requests.

use std::convert::TryInto;
use std::fmt::Debug;

use tokio_stream::wrappers::ReceiverStream;

use crate::api::snapshots::v1::{
    snapshots_server::{Snapshots, SnapshotsServer},
    *,
};
use crate::Snapshotter;

pub struct Wrapper<S: Snapshotter> {
    snapshotter: S,
}

/// Helper to create snapshots server from any object that implements [Snapshotter] trait.
pub fn server<S: Snapshotter>(snapshotter: S) -> SnapshotsServer<Wrapper<S>> {
    SnapshotsServer::new(Wrapper { snapshotter })
}

#[tonic::async_trait]
impl<S: Snapshotter> Snapshots for Wrapper<S> {
    async fn prepare(
        &self,
        request: tonic::Request<PrepareSnapshotRequest>,
    ) -> Result<tonic::Response<PrepareSnapshotResponse>, tonic::Status> {
        let request = request.into_inner();

        match self
            .snapshotter
            .prepare(request.key, request.parent, request.labels)
            .await
        {
            Ok(mounts) => {
                let message = PrepareSnapshotResponse { mounts };
                Ok(tonic::Response::new(message))
            }
            Err(err) => Err(status(err)),
        }
    }

    async fn view(
        &self,
        request: tonic::Request<ViewSnapshotRequest>,
    ) -> Result<tonic::Response<ViewSnapshotResponse>, tonic::Status> {
        let request = request.into_inner();

        match self
            .snapshotter
            .view(request.key, request.parent, request.labels)
            .await
        {
            Ok(mounts) => {
                let message = ViewSnapshotResponse { mounts };
                Ok(tonic::Response::new(message))
            }
            Err(err) => Err(status(err)),
        }
    }

    async fn mounts(
        &self,
        request: tonic::Request<MountsRequest>,
    ) -> Result<tonic::Response<MountsResponse>, tonic::Status> {
        let request = request.into_inner();

        match self.snapshotter.mounts(request.key).await {
            Ok(mounts) => {
                let message = MountsResponse { mounts };
                Ok(tonic::Response::new(message))
            }
            Err(err) => Err(status(err)),
        }
    }

    async fn commit(
        &self,
        request: tonic::Request<CommitSnapshotRequest>,
    ) -> Result<tonic::Response<()>, tonic::Status> {
        let request = request.into_inner();

        match self
            .snapshotter
            .commit(request.name, request.key, request.labels)
            .await
        {
            Ok(_) => Ok(tonic::Response::new(())),
            Err(err) => Err(status(err)),
        }
    }

    async fn remove(
        &self,
        request: tonic::Request<RemoveSnapshotRequest>,
    ) -> Result<tonic::Response<()>, tonic::Status> {
        let request = request.into_inner();

        match self.snapshotter.remove(request.key).await {
            Ok(_) => Ok(tonic::Response::new(())),
            Err(err) => Err(status(err)),
        }
    }

    async fn stat(
        &self,
        request: tonic::Request<StatSnapshotRequest>,
    ) -> Result<tonic::Response<StatSnapshotResponse>, tonic::Status> {
        let request = request.into_inner();

        match self.snapshotter.stat(request.key).await {
            Ok(info) => {
                let message = StatSnapshotResponse {
                    info: Some(info.into()),
                };

                Ok(tonic::Response::new(message))
            }
            Err(err) => Err(status(err)),
        }
    }

    async fn update(
        &self,
        request: tonic::Request<UpdateSnapshotRequest>,
    ) -> Result<tonic::Response<UpdateSnapshotResponse>, tonic::Status> {
        let request = request.into_inner();

        let info = match request.info {
            Some(info) => info,
            None => return Err(tonic::Status::failed_precondition("info is required")),
        };

        let info = match info.try_into() {
            Ok(info) => info,
            Err(err) => {
                let msg = format!("Failed to convert timestamp: {}", err);
                return Err(tonic::Status::invalid_argument(msg));
            }
        };

        let fields = request.update_mask.map(|mask| mask.paths);

        match self.snapshotter.update(info, fields).await {
            Ok(info) => {
                let message = UpdateSnapshotResponse {
                    info: Some(info.into()),
                };

                Ok(tonic::Response::new(message))
            }
            Err(err) => Err(status(err)),
        }
    }

    type ListStream = ReceiverStream<Result<ListSnapshotsResponse, tonic::Status>>;

    async fn list(
        &self,
        _request: tonic::Request<ListSnapshotsRequest>,
    ) -> Result<tonic::Response<Self::ListStream>, tonic::Status> {
        Err(tonic::Status::unimplemented("not implemented"))
    }

    async fn usage(
        &self,
        request: tonic::Request<UsageRequest>,
    ) -> Result<tonic::Response<UsageResponse>, tonic::Status> {
        let request = request.into_inner();

        match self.snapshotter.usage(request.key).await {
            Ok(usage) => {
                let message = UsageResponse {
                    size: usage.size,
                    inodes: usage.inodes,
                };

                Ok(tonic::Response::new(message))
            }
            Err(err) => Err(status(err)),
        }
    }

    async fn cleanup(
        &self,
        _request: tonic::Request<CleanupRequest>,
    ) -> Result<tonic::Response<()>, tonic::Status> {
        match self.snapshotter.clear().await {
            Ok(_) => Ok(tonic::Response::new(())),
            Err(err) => Err(status(err)),
        }
    }
}

fn status<E: Debug>(err: E) -> tonic::Status {
    let message = format!("{:?}", err);
    tonic::Status::internal(message)
}

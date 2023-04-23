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

use std::{convert::TryInto, mem, sync::Arc};

use futures::{stream::BoxStream, StreamExt};

use crate::{
    api::snapshots::v1::{
        snapshots_server::{Snapshots, SnapshotsServer},
        *,
    },
    Snapshotter,
};

pub struct Wrapper<S: Snapshotter> {
    snapshotter: Arc<S>,
}

/// Helper to create snapshots server from any object that implements [Snapshotter] trait.
pub fn server<S: Snapshotter>(snapshotter: Arc<S>) -> SnapshotsServer<Wrapper<S>> {
    SnapshotsServer::new(Wrapper { snapshotter })
}

#[tonic::async_trait]
impl<S: Snapshotter> Snapshots for Wrapper<S> {
    async fn prepare(
        &self,
        request: tonic::Request<PrepareSnapshotRequest>,
    ) -> Result<tonic::Response<PrepareSnapshotResponse>, tonic::Status> {
        let request = request.into_inner();

        let mounts = self
            .snapshotter
            .prepare(request.key, request.parent, request.labels)
            .await
            .map_err(Into::into)?;
        let message = PrepareSnapshotResponse { mounts };
        Ok(tonic::Response::new(message))
    }

    async fn view(
        &self,
        request: tonic::Request<ViewSnapshotRequest>,
    ) -> Result<tonic::Response<ViewSnapshotResponse>, tonic::Status> {
        let request = request.into_inner();
        let mounts = self
            .snapshotter
            .view(request.key, request.parent, request.labels)
            .await
            .map_err(Into::into)?;
        let message = ViewSnapshotResponse { mounts };
        Ok(tonic::Response::new(message))
    }

    async fn mounts(
        &self,
        request: tonic::Request<MountsRequest>,
    ) -> Result<tonic::Response<MountsResponse>, tonic::Status> {
        let request = request.into_inner();
        let mounts = self
            .snapshotter
            .mounts(request.key)
            .await
            .map_err(Into::into)?;
        let message = MountsResponse { mounts };
        Ok(tonic::Response::new(message))
    }

    async fn commit(
        &self,
        request: tonic::Request<CommitSnapshotRequest>,
    ) -> Result<tonic::Response<()>, tonic::Status> {
        let request = request.into_inner();
        self.snapshotter
            .commit(request.name, request.key, request.labels)
            .await
            .map_err(Into::into)?;
        Ok(tonic::Response::new(()))
    }

    async fn remove(
        &self,
        request: tonic::Request<RemoveSnapshotRequest>,
    ) -> Result<tonic::Response<()>, tonic::Status> {
        let request = request.into_inner();
        self.snapshotter
            .remove(request.key)
            .await
            .map_err(Into::into)?;
        Ok(tonic::Response::new(()))
    }

    async fn stat(
        &self,
        request: tonic::Request<StatSnapshotRequest>,
    ) -> Result<tonic::Response<StatSnapshotResponse>, tonic::Status> {
        let request = request.into_inner();
        let info = self
            .snapshotter
            .stat(request.key)
            .await
            .map_err(Into::into)?;
        let message = StatSnapshotResponse {
            info: Some(info.into()),
        };
        Ok(tonic::Response::new(message))
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

        let info = self
            .snapshotter
            .update(info, fields)
            .await
            .map_err(Into::into)?;
        let message = UpdateSnapshotResponse {
            info: Some(info.into()),
        };

        Ok(tonic::Response::new(message))
    }

    type ListStream = BoxStream<Result<ListSnapshotsResponse, tonic::Status>, 'static>;

    async fn list(
        &self,
        request: tonic::Request<ListSnapshotsRequest>,
    ) -> Result<tonic::Response<Self::ListStream>, tonic::Status> {
        let request = request.into_inner();
        let sn = self.snapshotter.clone();
        let output = async_stream::try_stream! {
            let walk_stream = sn.list(request.snapshotter, request.filters).await?;
            pin_utils::pin_mut!(walk_stream);
            let mut infos = Vec::<Info>::new();
            while let Some(info) = walk_stream.next().await {
                infos.push(info?.into());
                if infos.len() >= 100 {
                    yield ListSnapshotsResponse { info: mem::take(&mut infos) };
                }
            }

            if !infos.is_empty() {
                yield ListSnapshotsResponse { info: infos };
            }
        };
        Ok(tonic::Response::new(Box::pin(output)))
    }

    async fn usage(
        &self,
        request: tonic::Request<UsageRequest>,
    ) -> Result<tonic::Response<UsageResponse>, tonic::Status> {
        let request = request.into_inner();

        let usage = self
            .snapshotter
            .usage(request.key)
            .await
            .map_err(Into::into)?;
        let message = UsageResponse {
            size: usage.size,
            inodes: usage.inodes,
        };

        Ok(tonic::Response::new(message))
    }

    async fn cleanup(
        &self,
        _request: tonic::Request<CleanupRequest>,
    ) -> Result<tonic::Response<()>, tonic::Status> {
        self.snapshotter.clear().await.map_err(Into::into)?;
        Ok(tonic::Response::new(()))
    }
}

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

//! Various conversions between GRPC and native types.

use std::convert::{TryFrom, TryInto};

use thiserror::Error;
use tonic::Status;

use crate::api::snapshots::v1 as grpc;
use crate::{Info, Kind};

impl From<Kind> for i32 {
    fn from(kind: Kind) -> i32 {
        match kind {
            Kind::Unknown => 0,
            Kind::View => 1,
            Kind::Active => 2,
            Kind::Committed => 3,
        }
    }
}

impl TryFrom<i32> for Kind {
    type Error = Error;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        Ok(match value {
            0 => Kind::Unknown,
            1 => Kind::View,
            2 => Kind::Active,
            3 => Kind::Committed,
            _ => return Err(Error::InvalidEnumValue(value)),
        })
    }
}

impl TryFrom<grpc::Info> for Info {
    type Error = Error;

    fn try_from(info: grpc::Info) -> Result<Self, Self::Error> {
        Ok(Info {
            kind: info.kind.try_into()?,
            name: info.name,
            parent: info.parent,
            labels: info.labels,
            created_at: info.created_at.unwrap_or_default().try_into()?,
            updated_at: info.updated_at.unwrap_or_default().try_into()?,
        })
    }
}

impl From<Info> for grpc::Info {
    fn from(info: Info) -> Self {
        grpc::Info {
            name: info.name,
            parent: info.parent,
            kind: info.kind.into(),
            created_at: Some(info.created_at.into()),
            updated_at: Some(info.updated_at.into()),
            labels: info.labels,
        }
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Failed to convert GRPC timestamp: {0}")]
    Timestamp(#[from] prost_types::TimestampOutOfSystemRangeError),

    #[error("Invalid enum value: {0}")]
    InvalidEnumValue(i32),
}

impl From<Error> for tonic::Status {
    fn from(err: Error) -> Self {
        Status::internal(format!("{}", err))
    }
}

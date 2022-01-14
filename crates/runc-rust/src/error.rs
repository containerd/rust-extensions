/*
   copyright the containerd authors.

   licensed under the apache license, version 2.0 (the "license");
   you may not use this file except in compliance with the license.
   you may obtain a copy of the license at

       http://www.apache.org/licenses/license-2.0

   unless required by applicable law or agreed to in writing, software
   distributed under the license is distributed on an "as is" basis,
   without warranties or conditions of any kind, either express or implied.
   see the license for the specific language governing permissions and
   limitations under the license.
*/

// Forked from https://github.com/pwFoo/rust-runc/blob/master/src/lib.rs
/*
 * Copyright 2020 fsyncd, Berlin, Germany.
 * Additional material, copyright of the containerd authors.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use std::env;
use std::io;
use std::process::ExitStatus;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Unable to extract test files: {0}")]
    BundleExtractFailed(io::Error),

    #[error("Invalid path: {0}")]
    InvalidPath(io::Error),

    #[error(transparent)]
    JsonDeserializationFailed(#[from] serde_json::error::Error),

    #[error("Missing container statistics")]
    MissingContainerStats,

    #[error(transparent)]
    ProcessSpawnFailed(io::Error),

    #[error("Error occured in runc: {0}")]
    InvalidCommand(io::Error),

    #[error("Runc command failed: status={status}, stdout=\"{stdout}\", stderr=\"{stderr}\"")]
    CommandFailed {
        status: ExitStatus,
        stdout: String,
        stderr: String,
    },

    #[error("Runc IO unavailable: {0}")]
    UnavailableIO(io::Error),

    #[error("Runc command timed out: {0}")]
    CommandTimeout(tokio::time::error::Elapsed),

    #[error("Unable to parse runc version")]
    InvalidVersion,

    #[error("Unable to locate the runc")]
    NotFound,

    #[error("Error occurs with fs: {0}")]
    FileSystemError(io::Error),

    #[error("Failed to spec file: {0}")]
    SpecFileCreationError(io::Error),

    #[error(transparent)]
    SpecFileCleanupError(io::Error),

    #[error("Failed to find valid path for spec file")]
    SpecFileNotFound,

    #[error("Top command is missing a pid header")]
    TopMissingPidHeader,

    #[error("Top command returned an empty response")]
    TopShortResponseError,

    #[error("Unix socket connection error: {0}")]
    UnixSocketConnectionFailed(io::Error),

    #[error("Unable to bind to unix socket: {0}")]
    UnixSocketBindFailed(io::Error),

    #[error("Unix socket failed to receive pty")]
    UnixSocketReceiveMessageError,

    #[error("Unix socket unexpectedly closed")]
    UnixSocketUnexpectedCloseError,

    #[error("Failed to handle environment variable: {0}")]
    EnvError(env::VarError),

    #[error("Sorry, this part of api is not implemented: {0}")]
    Unimplemented(String),

    #[error("Error occured in runc client: {0}")]
    Other(Box<dyn std::error::Error + Send>),
}

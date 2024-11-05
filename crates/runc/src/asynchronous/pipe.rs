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

use std::os::unix::io::OwnedFd;

use tokio::net::unix::pipe;

/// Struct to represent a pipe that can be used to transfer stdio inputs and outputs.
///
/// With this Io driver, methods of [crate::Runc] may capture the output/error messages.
/// When one side of the pipe is closed, the state will be represented with [`None`].
#[derive(Debug)]
pub struct Pipe {
    pub rd: OwnedFd,
    pub wr: OwnedFd,
}

impl Pipe {
    pub fn new() -> std::io::Result<Self> {
        let (tx, rx) = pipe::pipe()?;
        let rd = tx.into_blocking_fd()?;
        let wr = rx.into_blocking_fd()?;
        Ok(Self { rd, wr })
    }
}

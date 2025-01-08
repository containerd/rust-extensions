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
        let rd = rx.into_blocking_fd()?;
        let wr = tx.into_blocking_fd()?;
        Ok(Self { rd, wr })
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::IntoRawFd;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    #[tokio::test]
    async fn test_pipe_creation() {
        let pipe = Pipe::new().expect("Failed to create pipe");
        assert!(
            pipe.rd.into_raw_fd() >= 0,
            "Read file descriptor is invalid"
        );
        assert!(
            pipe.wr.into_raw_fd() >= 0,
            "Write file descriptor is invalid"
        );
    }

    #[tokio::test]
    async fn test_pipe_write_read() {
        let pipe = Pipe::new().expect("Failed to create pipe");
        let mut read_end = pipe::Receiver::from_owned_fd(pipe.rd).unwrap();
        let mut write_end = pipe::Sender::from_owned_fd(pipe.wr).unwrap();
        let write_data = b"hello";

        write_end
            .write_all(write_data)
            .await
            .expect("Failed to write to pipe");

        let mut read_data = vec![0; write_data.len()];
        read_end
            .read_exact(&mut read_data)
            .await
            .expect("Failed to read from pipe");

        assert_eq!(
            read_data, write_data,
            "Data read from pipe does not match data written"
        );
    }

    #[tokio::test]
    async fn test_pipe_async_write_read() {
        let pipe = Pipe::new().expect("Failed to create pipe");
        let mut read_end = pipe::Receiver::from_owned_fd(pipe.rd).unwrap();
        let mut write_end = pipe::Sender::from_owned_fd(pipe.wr).unwrap();

        let write_data = b"hello";
        tokio::spawn(async move {
            write_end
                .write_all(write_data)
                .await
                .expect("Failed to write to pipe");
        });

        let mut read_data = vec![0; write_data.len()];
        read_end
            .read_exact(&mut read_data)
            .await
            .expect("Failed to read from pipe");

        assert_eq!(
            &read_data, write_data,
            "Data read from pipe does not match data written"
        );
    }
}

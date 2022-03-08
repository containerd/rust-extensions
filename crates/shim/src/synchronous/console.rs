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

use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use log::warn;

pub struct ConsoleSocket {
    pub listener: UnixListener,
    pub path: PathBuf,
    pub rmdir: bool,
}

impl ConsoleSocket {
    pub fn accept(&self) -> std::io::Result<UnixStream> {
        let (stream, _addr) = self.listener.accept()?;
        Ok(stream)
    }
}

impl Drop for ConsoleSocket {
    fn drop(&mut self) {
        if self.rmdir {
            let tmp_socket_dir = self.path.parent().unwrap();
            std::fs::remove_dir_all(tmp_socket_dir).unwrap_or_else(|e| {
                warn!(
                    "remove tmp console socket path {} : {}",
                    tmp_socket_dir.to_str().unwrap(),
                    e
                )
            })
        }
    }
}

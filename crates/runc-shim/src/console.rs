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

use std::os::unix::io::RawFd;

use log::{debug, warn};
use nix::cmsg_space;
use nix::sys::socket::{recvmsg, ControlMessageOwned, MsgFlags};
use nix::sys::termios::tcgetattr;
use nix::sys::uio::IoVec;

use containerd_shim::other;
use containerd_shim::Error;

pub fn receive_socket(stream_fd: RawFd) -> containerd_shim::Result<RawFd> {
    let mut buf = [0u8; 4096];
    let iovec = [IoVec::from_mut_slice(&mut buf)];
    let mut space = cmsg_space!([RawFd; 2]);
    let (path, fds) = match recvmsg(stream_fd, &iovec, Some(&mut space), MsgFlags::empty()) {
        Ok(msg) => {
            let mut iter = msg.cmsgs();
            if let Some(ControlMessageOwned::ScmRights(fds)) = iter.next() {
                (iovec[0].as_slice(), fds)
            } else {
                return Err(other!("received message is empty"));
            }
        }
        Err(e) => {
            return Err(other!("failed to receive message: {}", e));
        }
    };
    if fds.is_empty() {
        return Err(other!("received message is empty"));
    }
    let path = String::from_utf8(Vec::from(path)).unwrap_or_else(|e| {
        warn!("failed to get path from array {}", e);
        "".to_string()
    });
    let path = path.trim_matches(char::from(0));
    debug!(
        "copy_console: console socket get path: {}, fd: {}",
        path, &fds[0]
    );
    tcgetattr(fds[0])?;
    Ok(fds[0])
}

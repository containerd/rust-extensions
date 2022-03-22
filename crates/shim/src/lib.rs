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

//! A library to implement custom runtime v2 shims for containerd.
//!
//! # Runtime
//! Runtime v2 introduces a first class shim API for runtime authors to integrate with containerd.
//! The shim API is minimal and scoped to the execution lifecycle of a container.
//!
//! This crate simplifies shim v2 runtime development for containerd. It handles common tasks such
//! as command line parsing, setting up shim's TTRPC server, logging, events, etc.
//!
//! Clients are expected to implement [Shim] and [Task] traits with task handling routines.
//! This generally replicates same API as in Go [version](https://github.com/containerd/containerd/blob/main/runtime/v2/example/cmd/main.go).
//!
//! Once implemented, shim's bootstrap code is as easy as:
//! ```text
//! shim::run::<Service>("io.containerd.empty.v1")
//! ```
//!

use std::collections::hash_map::DefaultHasher;
use std::fs::File;
use std::hash::Hasher;
use std::os::unix::io::RawFd;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

use nix::ioctl_write_ptr_bad;

pub use containerd_shim_protos as protos;
pub use protos::shim::shim::DeleteResponse;
pub use protos::ttrpc::{context::Context, Result as TtrpcResult};

#[cfg(feature = "async")]
pub use crate::asynchronous::*;
pub use crate::error::{Error, Result};
#[cfg(not(feature = "async"))]
pub use crate::synchronous::*;

#[macro_use]
pub mod error;

mod args;
#[cfg(feature = "async")]
pub mod asynchronous;
pub mod cgroup;
pub mod event;
pub mod io;
mod logger;
pub mod monitor;
pub mod mount;
mod reap;
#[cfg(not(feature = "async"))]
pub mod synchronous;
pub mod util;

/// Generated request/response structures.
pub mod api {
    pub use super::protos::api::Status;
    pub use super::protos::shim::oci::Options;
    pub use super::protos::shim::shim::*;
    pub use super::protos::types::empty::Empty;
}

macro_rules! cfg_not_async {
    ($($item:item)*) => {
        $(
            #[cfg(not(feature = "async"))]
            #[cfg_attr(docsrs, doc(cfg(not(feature = "async"))))]
            $item
        )*
    }
}

macro_rules! cfg_async {
    ($($item:item)*) => {
        $(
            #[cfg(feature = "async")]
            #[cfg_attr(docsrs, doc(cfg(feature = "async")))]
            $item
        )*
    }
}

cfg_not_async! {
    pub use crate::synchronous::*;
    pub use crate::synchronous::console;
    pub use crate::synchronous::publisher;
    pub use protos::shim::shim_ttrpc::Task;
    pub use protos::ttrpc::TtrpcContext;
}

cfg_async! {
    pub use crate::asynchronous::*;
    pub use crate::asynchronous::console;
    pub use crate::asynchronous::container;
    pub use crate::asynchronous::processes;
    pub use crate::asynchronous::task;
    pub use crate::asynchronous::publisher;
    pub use protos::shim_async::Task;
    pub use protos::ttrpc::r#async::TtrpcContext;
}

ioctl_write_ptr_bad!(ioctl_set_winsz, libc::TIOCSWINSZ, libc::winsize);

const TTRPC_ADDRESS: &str = "TTRPC_ADDRESS";

/// Config of shim binary options provided by shim implementations
#[derive(Debug, Default)]
pub struct Config {
    /// Disables automatic configuration of logrus to use the shim FIFO
    pub no_setup_logger: bool,
    /// Disables the shim binary from reaping any child process implicitly
    pub no_reaper: bool,
    /// Disables setting the shim as a child subreaper.
    pub no_sub_reaper: bool,
}

/// Startup options received from containerd to start new shim instance.
///
/// These will be passed via [`Shim::start_shim`] to shim.
#[derive(Debug, Default)]
pub struct StartOpts {
    /// ID of the container.
    pub id: String,
    /// Binary path to publish events back to containerd.
    pub publish_binary: String,
    /// Address of the containerd's main socket.
    pub address: String,
    /// TTRPC socket address.
    pub ttrpc_address: String,
    /// Namespace for the container.
    pub namespace: String,

    pub debug: bool,
}

/// The shim process communicates with the containerd server through a communication channel
/// created by containerd. One endpoint of the communication channel is passed to shim process
/// through a file descriptor during forking, which is the fourth(3) file descriptor.
const SOCKET_FD: RawFd = 3;

#[cfg(target_os = "linux")]
pub const SOCKET_ROOT: &str = "/run/containerd";

#[cfg(target_os = "macos")]
pub const SOCKET_ROOT: &str = "/var/run/containerd";

/// Make socket path from containerd socket path, namespace and id.
pub fn socket_address(socket_path: &str, namespace: &str, id: &str) -> String {
    let path = PathBuf::from(socket_path)
        .join(namespace)
        .join(id)
        .display()
        .to_string();

    let hash = {
        let mut hasher = DefaultHasher::new();
        hasher.write(path.as_bytes());
        hasher.finish()
    };

    format!("unix://{}/{:x}.sock", SOCKET_ROOT, hash)
}

fn parse_sockaddr(addr: &str) -> &str {
    if let Some(addr) = addr.strip_prefix("unix://") {
        return addr;
    }

    if let Some(addr) = addr.strip_prefix("vsock://") {
        return addr;
    }

    addr
}

fn start_listener(address: &str) -> std::io::Result<UnixListener> {
    let path = parse_sockaddr(address);
    // Try to create the needed directory hierarchy.
    if let Some(parent) = Path::new(path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    UnixListener::bind(path)
}

pub struct Console {
    pub file: File,
}

#[cfg(test)]
mod tests {
    use crate::start_listener;

    #[test]
    fn test_start_listener() {
        let tmpdir = tempfile::tempdir().unwrap();
        let path = tmpdir.path().to_str().unwrap().to_owned();

        // A little dangerous, may be turned on under controlled environment.
        //assert!(start_listener("/").is_err());
        //assert!(start_listener("/tmp").is_err());

        let socket = path + "/ns1/id1/socket";
        let _listener = start_listener(&socket).unwrap();
        let _listener2 = start_listener(&socket).expect_err("socket should already in use");

        let socket2 = socket + "/socket";
        assert!(start_listener(&socket2).is_err());

        let path = tmpdir.path().to_str().unwrap().to_owned();
        let txt_file = path + "demo.txt";
        std::fs::write(&txt_file, "test").unwrap();
        assert!(start_listener(&txt_file).is_err());
        let context = std::fs::read_to_string(&txt_file).unwrap();
        assert_eq!(context, "test");
    }
}

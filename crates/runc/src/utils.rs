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

#![allow(unused)]

use std::env;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

use nix::sys::stat::Mode;
use nix::unistd::mkdir;
use path_absolutize::*;
use tempfile::{Builder, NamedTempFile};
use uuid::Uuid;

use crate::console::ConsoleSocket;
use crate::error::Error;

// helper to resolve path (such as path for runc binary, pid files, etc. )
pub fn abs_path_buf<P>(path: P) -> Result<PathBuf, Error>
where
    P: AsRef<Path>,
{
    Ok(path
        .as_ref()
        .absolutize()
        .map_err(Error::InvalidPath)?
        .to_path_buf())
}

fn path_to_string(path: impl AsRef<Path>) -> Result<String, Error> {
    path.as_ref()
        .to_str()
        .map(|v| v.to_string())
        .ok_or_else(|| {
            let e = std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("invalid UTF-8 string: {}", path.as_ref().to_string_lossy()),
            );
            Error::InvalidPath(e)
        })
}

pub fn abs_string<P>(path: P) -> Result<String, Error>
where
    P: AsRef<Path>,
{
    path_to_string(abs_path_buf(path)?)
}

pub fn new_temp_console_socket() -> Result<ConsoleSocket, Error> {
    let dir = env::var_os("XDG_RUNTIME_DIR")
        .map(|runtime_dir| {
            format!(
                "{}/pty{}",
                runtime_dir.to_string_lossy().parse::<String>().unwrap(),
                Uuid::new_v4(),
            )
        })
        .ok_or(Error::SpecFileNotFound)?;
    mkdir(Path::new(&dir), Mode::from_bits(0o711).unwrap()).map_err(Error::CreateDir)?;
    let file_name = Path::new(&dir).join("pty.sock");
    let listener = UnixListener::bind(file_name.as_path()).map_err(Error::UnixSocketBindFailed)?;
    Ok(ConsoleSocket {
        listener,
        path: file_name,
        rmdir: true,
    })
}

pub fn temp_filename_in_runtime_dir() -> Result<String, Error> {
    match env::var_os("XDG_RUNTIME_DIR") {
        Some(runtime_dir) => {
            let path = path_to_string(runtime_dir)?;
            Ok(format!("{}/runc-process-{}", path, Uuid::new_v4()))
        }
        None => Err(Error::SpecFileNotFound),
    }
}

pub fn make_temp_file_in_runtime_dir() -> Result<(NamedTempFile, String), Error> {
    let file_name = temp_filename_in_runtime_dir()?;
    let temp_file = Builder::new()
        .prefix(&file_name)
        .rand_bytes(0)
        .tempfile()
        .map_err(Error::SpecFileCreationFailed)?;
    Ok((temp_file, file_name))
}

/// Resolve a binary path according to the `PATH` environment variable.
///
/// Note, the case that `path` is already an absolute path is implicitly handled by
/// `dir.join(path.as_ref())`. `Path::join(parent_path, path)` directly returns `path` when `path`
/// is an absolute path.
pub fn binary_path<P>(path: P) -> Option<PathBuf>
where
    P: AsRef<Path>,
{
    env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths).find_map(|dir| {
            let full_path = dir.join(path.as_ref());
            if full_path.is_file() {
                Some(full_path)
            } else {
                None
            }
        })
    })
}

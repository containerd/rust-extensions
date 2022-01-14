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

use std::env;
use std::path::{Path, PathBuf};

use path_absolutize::*;
use tempfile::{Builder, NamedTempFile};
use uuid::Uuid;

use crate::error::Error;

// constants for flags
pub const ALL: &str = "--all";
pub const CONSOLE_SOCKET: &str = "--console-socket";
// pub const CRIU: &str = "--criu";
pub const DEBUG: &str = "--debug";
pub const DETACH: &str = "--detach";
pub const FORCE: &str = "--force";
pub const LOG: &str = "--log";
pub const LOG_FORMAT: &str = "--log-format";
pub const NO_NEW_KEYRING: &str = "--no-new-keyring";
pub const NO_PIVOT: &str = "--no-pivot";
pub const PID_FILE: &str = "--pid-file";
pub const ROOT: &str = "--root";
pub const ROOTLESS: &str = "--rootless";
pub const SYSTEMD_CGROUP: &str = "--systemd-cgroup";

// constants for log format
pub const JSON: &str = "json";
pub const TEXT: &str = "text";

// constant for command
pub const DEFAULT_COMMAND: &str = "runc";

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

pub fn abs_string<P>(path: P) -> Result<String, Error>
where
    P: AsRef<Path>,
{
    Ok(abs_path_buf(path)?
        .to_string_lossy()
        .parse::<String>()
        .unwrap())
}

pub fn make_temp_file_in_runtime_dir() -> Result<(NamedTempFile, String), Error> {
    let file_name = env::var_os("XDG_RUNTIME_DIR")
        .map(|runtime_dir| {
            format!(
                "{}/runc-process-{}",
                runtime_dir.to_string_lossy().parse::<String>().unwrap(),
                Uuid::new_v4(),
            )
        })
        .ok_or_else(|| Error::SpecFileNotFound)?;
    let temp_file = Builder::new()
        .prefix(&file_name)
        .tempfile()
        .map_err(Error::SpecFileCreationError)?;
    Ok((temp_file, file_name))
}

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

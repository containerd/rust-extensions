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

#[cfg(unix)]
use std::os::unix::io::RawFd;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[cfg(feature = "async")]
pub use crate::asynchronous::util::*;
#[cfg(not(feature = "async"))]
pub use crate::synchronous::util::*;
use crate::{
    api::Options,
    error::{Error, Result},
    protos::protobuf::{
        well_known_types::{any::Any, timestamp::Timestamp},
        MessageDyn,
    },
};

pub const CONFIG_FILE_NAME: &str = "config.json";
pub const OPTIONS_FILE_NAME: &str = "options.json";
pub const RUNTIME_FILE_NAME: &str = "runtime";

// Define JsonOptions here for Json serialize and deserialize
// as rust-protobuf hasn't released serde_derive feature,
// see https://github.com/stepancheg/rust-protobuf/#serde_derive-support
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JsonOptions {
    #[serde(default)]
    pub no_pivot_root: bool,
    #[serde(default)]
    pub no_new_keyring: bool,
    pub shim_cgroup: ::std::string::String,
    #[serde(default)]
    pub io_uid: u32,
    #[serde(default)]
    pub io_gid: u32,
    pub binary_name: ::std::string::String,
    pub root: ::std::string::String,
    #[serde(default)]
    pub systemd_cgroup: bool,
    pub criu_image_path: ::std::string::String,
    pub criu_work_path: ::std::string::String,
    #[serde(default)]
    pub task_api_address: ::std::string::String,
    #[serde(default)]
    pub task_api_version: u32,
}

impl From<Options> for JsonOptions {
    fn from(o: Options) -> Self {
        Self {
            no_pivot_root: o.no_pivot_root,
            no_new_keyring: o.no_new_keyring,
            shim_cgroup: o.shim_cgroup,
            io_uid: o.io_uid,
            io_gid: o.io_gid,
            binary_name: o.binary_name,
            root: o.root,
            systemd_cgroup: o.systemd_cgroup,
            criu_image_path: o.criu_image_path,
            criu_work_path: o.criu_work_path,
            task_api_address: o.task_api_address,
            task_api_version: o.task_api_version,
        }
    }
}

impl From<JsonOptions> for Options {
    fn from(j: JsonOptions) -> Self {
        Self {
            no_pivot_root: j.no_pivot_root,
            no_new_keyring: j.no_new_keyring,
            shim_cgroup: j.shim_cgroup,
            io_uid: j.io_uid,
            io_gid: j.io_gid,
            binary_name: j.binary_name,
            root: j.root,
            systemd_cgroup: j.systemd_cgroup,
            criu_image_path: j.criu_image_path,
            criu_work_path: j.criu_work_path,
            task_api_address: j.task_api_address,
            task_api_version: j.task_api_version,
            ..Default::default()
        }
    }
}

/// Connects to the unix socket at `address`, given as a plain path or as a `unix://` URL.
#[cfg(unix)]
pub fn connect(address: impl AsRef<str>) -> Result<RawFd> {
    use std::os::{fd::IntoRawFd, unix::net::UnixStream};

    let path = address.as_ref();
    let path = path.strip_prefix("unix://").unwrap_or(path);
    UnixStream::connect(path)
        .map(IntoRawFd::into_raw_fd)
        .map_err(io_error!(e, "connect {}", path))
}

/// Replace a file atomically, cleaning up the temporary file on failure.
/// Like OpenOptions::create_new, creation uses mode 0o666 filtered by umask.
pub(crate) fn write_str_atomic(filename: &std::path::Path, contents: &str) -> Result<()> {
    use std::io::Write;

    if filename.file_name().is_none() {
        return Err(crate::Error::InvalidArgument(format!(
            "invalid file path {}",
            filename.display()
        )));
    }
    let parent = filename
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let mut builder = tempfile::Builder::new();
    builder.prefix(".shim-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o666));
    }
    let mut file = builder.tempfile_in(parent).map_err(io_error!(
        e,
        "create temporary file for {}",
        filename.display()
    ))?;
    file.write_all(contents.as_bytes()).map_err(io_error!(
        e,
        "write temporary file for {}",
        filename.display()
    ))?;
    file.persist(filename).map_err(|e| crate::Error::IoError {
        context: format!("replace {}", filename.display()),
        err: e.error,
    })?;
    Ok(())
}

pub fn timestamp() -> Result<Timestamp> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?;

    let ts = Timestamp {
        seconds: now.as_secs() as _,
        nanos: now.subsec_nanos() as _,
        ..Default::default()
    };

    Ok(ts)
}

pub fn convert_to_timestamp(exited_at: Option<OffsetDateTime>) -> Timestamp {
    let mut ts = Timestamp::new();
    if let Some(ea) = exited_at {
        ts.seconds = ea.unix_timestamp();
        ts.nanos = ea.nanosecond() as i32;
    }
    ts
}

pub fn convert_to_any(obj: Box<dyn MessageDyn>) -> Result<Any> {
    let mut data = Vec::new();
    obj.write_to_vec_dyn(&mut data)?;

    let mut any = Any::new();
    any.value = data;
    any.type_url = obj.descriptor_dyn().full_name().to_string();

    Ok(any)
}

#[deprecated(note = "use Option::filter or bool::then instead")]
pub trait IntoOption
where
    Self: Sized,
{
    fn none_if<F>(self, callback: F) -> Option<Self>
    where
        F: Fn(&Self) -> bool,
    {
        Some(self).filter(|value| !callback(value))
    }
}

#[allow(deprecated)]
impl<T> IntoOption for T {}

#[deprecated(note = "use Option::filter or bool::then_some instead")]
pub trait AsOption {
    fn as_option(&self) -> Option<&Self>;
}

#[allow(deprecated)]
impl AsOption for str {
    fn as_option(&self) -> Option<&Self> {
        Some(self).filter(|value| !value.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state");
        // A stale file from the old naming scheme must not prevent writes.
        std::fs::write(dir.path().join(".state"), "stale").unwrap();
        write_str_atomic(&path, "old").unwrap();
        write_str_atomic(&path, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        let target_dir = dir.path().join("directory");
        std::fs::create_dir(&target_dir).unwrap();
        assert!(write_str_atomic(&target_dir, "invalid").is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 3);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_preserves_creation_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let reference = dir.path().join("reference");
        std::fs::write(&reference, "reference").unwrap();
        let path = dir.path().join("state");
        write_str_atomic(&path, "contents").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "contents");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode(),
            std::fs::metadata(&reference).unwrap().permissions().mode()
        );
    }

    // macOS filesystems can reject invalid UTF-8 filenames with EILSEQ.
    // Keep this separate from the permission test, which runs on all Unix targets.
    #[cfg(target_os = "linux")]
    #[test]
    fn atomic_write_accepts_non_utf8_paths() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(std::ffi::OsStr::from_bytes(b"state-\xff"));
        write_str_atomic(&path, "contents").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "contents");
    }

    // Windows refuses to replace a file while another replace of it is in
    // flight, so two writers racing is only well-defined on Unix.
    #[cfg(unix)]
    #[test]
    fn concurrent_atomic_writes_leave_one_complete_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state");
        std::thread::scope(|scope| {
            for value in ["a".repeat(8192), "b".repeat(8192)] {
                let path = &path;
                scope.spawn(move || write_str_atomic(path, &value).unwrap());
            }
        });
        let value = std::fs::read_to_string(&path).unwrap();
        assert!(value == "a".repeat(8192) || value == "b".repeat(8192));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn test_timestamp() {
        let ts = timestamp().unwrap();
        assert!(ts.seconds > 0);
    }

    #[cfg(unix)]
    #[test]
    fn test_connect_accepts_unix_url() {
        use std::os::unix::net::UnixListener;

        let tmpdir = tempfile::tempdir().unwrap();
        let path = tmpdir.path().join("socket");
        let _listener = UnixListener::bind(&path).unwrap();
        let path = path.to_str().unwrap();

        for address in [path.to_string(), format!("unix://{path}")] {
            let fd = connect(&address).unwrap();
            nix::unistd::close(fd).unwrap();
        }
    }
}

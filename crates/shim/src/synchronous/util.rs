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

use std::path::Path;

use containerd_shim_protos::shim::oci::Options;
#[cfg(unix)]
use libc::mode_t;
use log::warn;
use oci_spec::runtime::Spec;

use crate::{
    util::{JsonOptions, OPTIONS_FILE_NAME, RUNTIME_FILE_NAME},
    Error,
};

pub fn read_file_to_str<P: AsRef<Path>>(filename: P) -> crate::Result<String> {
    std::fs::read_to_string(&filename).map_err(io_error!(e, "read {}", filename.as_ref().display()))
}

pub fn read_options(bundle: impl AsRef<Path>) -> crate::Result<Options> {
    let path = bundle.as_ref().join(OPTIONS_FILE_NAME);
    let opts_str = read_file_to_str(path)?;
    let json_opt: JsonOptions = serde_json::from_str(&opts_str)?;
    Ok(json_opt.into())
}

pub fn read_runtime(bundle: impl AsRef<Path>) -> crate::Result<String> {
    let path = bundle.as_ref().join(RUNTIME_FILE_NAME);
    read_file_to_str(path)
}

pub fn read_address() -> crate::Result<String> {
    let path = Path::new("address");
    read_file_to_str(path)
}

pub fn read_pid_from_file(pid_path: &Path) -> crate::Result<i32> {
    let pid_str = read_file_to_str(pid_path)?;
    let pid = pid_str.parse::<i32>()?;
    Ok(pid)
}

pub fn write_str_to_path(filename: &Path, s: &str) -> crate::Result<()> {
    crate::util::write_str_atomic(filename, s)
}

pub fn write_options(bundle: &str, opt: &Options) -> crate::Result<()> {
    let json_opt = JsonOptions::from(opt.to_owned());
    let opts_str = serde_json::to_string(&json_opt)?;
    let path = Path::new(bundle).join(OPTIONS_FILE_NAME);
    write_str_to_path(path.as_path(), opts_str.as_str())
}

pub fn write_runtime(bundle: &str, binary_name: &str) -> crate::Result<()> {
    let path = Path::new(bundle).join(RUNTIME_FILE_NAME);
    write_str_to_path(path.as_path(), binary_name)
}

pub fn write_address(address: &str) -> crate::Result<()> {
    let path = Path::new("address");
    write_str_to_path(path, address)
}

pub fn read_spec_from_file(bundle: &str) -> crate::Result<Spec> {
    let path = Path::new(bundle).join("config.json");
    Spec::load(path).map_err(other_error!("read spec file"))
}

#[cfg(unix)]
pub fn mkdir(path: impl AsRef<Path>, mode: mode_t) -> crate::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    // mode_t is u16 on macOS, while DirBuilder takes u32.
    #[allow(clippy::unnecessary_cast)]
    let mode = mode as u32;
    let path = path.as_ref();
    match std::fs::DirBuilder::new().mode(mode).create(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        Err(e) => Err(Error::IoError {
            context: format!("mkdir {}", path.display()),
            err: e,
        }),
    }
}

/// A helper to help remove temperate file or dir when it became useless
pub struct HelperRemoveFile {
    path: String,
}

impl HelperRemoveFile {
    pub fn new(path: String) -> Self {
        Self { path }
    }
}

impl Drop for HelperRemoveFile {
    fn drop(&mut self) {
        std::fs::remove_file(&self.path)
            .unwrap_or_else(|e| warn!("remove dir {} error: {}", &self.path, e));
    }
}

#[cfg(target_os = "windows")]
// helper to configure pause thread until signaled. Useful in attaching a debugger
// https://github.com/microsoft/hcsshim/blob/v0.10.0-rc.7/cmd/containerd-shim-runhcs-v1/serve.go#L313-L315
// use with https://github.com/moby/docker-signal
pub(crate) fn setup_debugger_event() {
    use std::{env, io, process};

    use log::{debug, error};
    use windows_sys::Win32::System::Threading::{WaitForSingleObject, INFINITE};

    let debugger = env::var("SHIM_DEBUGGER").unwrap_or_else(|_| "".to_string());
    if debugger.is_empty() {
        return;
    }
    let event_name = format!("Global\\debugger-{}", process::id());
    debug!("Halting until signalled: {}", event_name);
    let e = match create_event(event_name) {
        Ok(e) => e,
        Err(e) => {
            error!("failed to create event for debugger: {}", e);
            return;
        }
    };
    match unsafe { WaitForSingleObject(e, INFINITE) } {
        0 => {}
        _ => {
            error!(
                "failed to wait for debugger event: {}",
                io::Error::last_os_error()
            );
            return;
        }
    }
    debug!("signal received, continuing");
}

#[cfg(target_os = "windows")]
fn create_event(name: String) -> crate::Result<isize> {
    use std::{ffi::OsStr, io, os::windows::prelude::OsStrExt};

    use windows_sys::Win32::System::Threading::CreateEventW;

    let name = OsStr::new(name.as_str())
        .encode_wide()
        .chain(Some(0)) // add NULL termination
        .collect::<Vec<_>>();

    let result = unsafe { CreateEventW(std::ptr::null_mut(), 0, 0, name.as_ptr()) };
    match result {
        0 => Err(Error::Other(io::Error::last_os_error().to_string())),
        _ => Ok(result),
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn mkdir_accepts_directories_but_rejects_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("child");
        super::mkdir(&path, 0o700).unwrap();
        super::mkdir(&path, 0o755).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let file = dir.path().join("file");
        std::fs::write(&file, "untouched").unwrap();
        assert!(super::mkdir(&file, 0o700).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "untouched");
    }
}

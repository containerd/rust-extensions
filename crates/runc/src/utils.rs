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

use std::{
    env,
    io::Write,
    path::{Path, PathBuf},
};

use serde::Serialize;
use tempfile::{Builder, NamedTempFile};

use crate::error::Error;

// helper to resolve path (such as path for runc binary, pid files, etc. )
pub fn abs_path_buf<P>(path: P) -> Result<PathBuf, Error>
where
    P: AsRef<Path>,
{
    let abs = std::path::absolute(path).map_err(Error::InvalidPath)?;
    let mut normalized = PathBuf::new();
    for component in abs.components() {
        match component {
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::CurDir => {}
            c => normalized.push(c),
        }
    }
    Ok(normalized)
}

fn path_to_string(path: impl AsRef<Path>) -> Result<String, Error> {
    path.as_ref()
        .to_str()
        .map(|v| v.to_string())
        .ok_or_else(|| {
            Error::InvalidPath(std::io::Error::other(format!(
                "invalid UTF-8 string: {}",
                path.as_ref().to_string_lossy()
            )))
        })
}

pub fn abs_string<P>(path: P) -> Result<String, Error>
where
    P: AsRef<Path>,
{
    path_to_string(abs_path_buf(path)?)
}

/// Returns a temp dir. If the environment variable "XDG_RUNTIME_DIR" is set, return its value.
/// Otherwise if `std::env::temp_dir()` failed, return current dir or return the temp dir depended on OS.
fn xdg_runtime_dir() -> String {
    env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| abs_string(env::temp_dir()).unwrap_or_else(|_| ".".to_string()))
}

/// Write the serialized `value` to a temp file, returning the file handle and its path.
///
/// The returned [NamedTempFile] owns the file: dropping it removes the file. Callers
/// must keep it alive for as long as `runc` needs to read the path. This is shared by
/// the sync and async clients — the write is a few KB to `$XDG_RUNTIME_DIR`, and doing
/// it synchronously is what makes RAII cleanup possible in async code, where there is
/// no async `Drop`.
pub fn write_value_to_temp_file<T: Serialize>(value: &T) -> Result<(NamedTempFile, String), Error> {
    let mut temp_file = Builder::new()
        .prefix("runc-process-")
        .suffix(".json")
        .tempfile_in(xdg_runtime_dir())
        .map_err(Error::SpecFileCreationFailed)?;
    let f = temp_file.as_file_mut();
    let spec_json = serde_json::to_string(value).map_err(Error::JsonDeserializationFailed)?;
    f.write_all(spec_json.as_bytes())
        .map_err(Error::SpecFileCreationFailed)?;
    f.flush().map_err(Error::SpecFileCreationFailed)?;
    let path = path_to_string(temp_file.path())?;
    Ok((temp_file, path))
}

/// Build a `pre_exec` hook that restores the `THP_DISABLED` value the parent exported.
///
/// `PR_SET_THP_DISABLE` sets a flag on the *calling* process's mm, and the shim
/// deliberately disables THP for itself to save RSS. So the restore has to happen in
/// the child — see `containerd-shim-runc-v2`'s `start_shim`, which stashes the original
/// value in `THP_DISABLED` precisely so the child can put it back before `execve`.
///
/// The environment is read here, in the parent. Everything the returned closure does
/// runs post-`fork` in a single-threaded child, where only async-signal-safe calls are
/// legal: `env::var` takes std's `ENV_LOCK` and `log::debug!` allocates and takes the
/// logger mutex, either of which can deadlock the child if another thread held it at
/// the moment of the fork. `prctl` is a bare syscall and is safe there.
///
/// Returns `None` when the knob is unset, so the caller can skip registering a hook at
/// all — any `pre_exec` forces `fork` + `execve` instead of the `posix_spawn` fast path.
#[cfg(target_os = "linux")]
pub(crate) fn restore_thp() -> Option<impl FnMut() -> std::io::Result<()> + Send + Sync + 'static> {
    let disabled: bool = env::var("THP_DISABLED").ok()?.parse().ok()?;
    Some(move || {
        // SAFETY: async-signal-safe — a single `prctl`, no allocation and no locks.
        unsafe {
            libc::prctl(
                libc::PR_SET_THP_DISABLE,
                if disabled { 1u64 } else { 0u64 },
                0,
                0,
                0,
            )
        };
        Ok(())
    })
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

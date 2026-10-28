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

use containerd_shim_protos::{api::Mount, shim::oci::Options};
use libc::mode_t;
use oci_spec::runtime::Spec;
use tokio::task::spawn_blocking;

use crate::{
    error::{Error, Result},
    util::{JsonOptions, CONFIG_FILE_NAME, OPTIONS_FILE_NAME, RUNTIME_FILE_NAME},
};

pub async fn asyncify<F, T>(f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    spawn_blocking(f)
        .await
        .map_err(other_error!("failed to spawn blocking task"))?
}

pub async fn read_file_to_str(path: impl AsRef<Path>) -> Result<String> {
    tokio::fs::read_to_string(&path)
        .await
        .map_err(io_error!(e, "read {}", path.as_ref().display()))
}

pub async fn write_str_to_file(filename: impl AsRef<Path>, s: impl AsRef<str>) -> Result<()> {
    let filename = filename.as_ref().to_owned();
    let contents = s.as_ref().to_owned();
    asyncify(move || crate::util::write_str_atomic(&filename, &contents)).await
}

pub async fn read_pid_from_file(pid_path: &Path) -> Result<i32> {
    let pid_str = read_file_to_str(pid_path).await?;
    let pid = pid_str.parse::<i32>()?;
    Ok(pid)
}

pub async fn read_spec(bundle: impl AsRef<Path>) -> Result<Spec> {
    let path = bundle.as_ref().join(CONFIG_FILE_NAME);
    let content = read_file_to_str(&path).await?;
    serde_json::from_str::<Spec>(content.as_str()).map_err(other_error!("read spec"))
}

// read_options reads the option information from the path.
// When the file does not exist, read_options returns nil without an error.
pub async fn read_options(bundle: impl AsRef<Path>) -> Result<Options> {
    let path = bundle.as_ref().join(OPTIONS_FILE_NAME);
    if !path.exists() {
        return Ok(Options::default());
    }
    let opts_str = read_file_to_str(path).await?;
    let opts =
        serde_json::from_str::<JsonOptions>(&opts_str).map_err(other_error!("read options"))?;
    Ok(opts.into())
}

pub async fn read_runtime(bundle: impl AsRef<Path>) -> Result<String> {
    read_file_to_str(bundle.as_ref().join(RUNTIME_FILE_NAME)).await
}

pub async fn write_options(bundle: impl AsRef<Path>, opt: &Options) -> Result<()> {
    let json_opt = JsonOptions::from(opt.to_owned());
    let opts_str = serde_json::to_string(&json_opt)?;
    let path = bundle.as_ref().join(OPTIONS_FILE_NAME);
    write_str_to_file(path.as_path(), opts_str.as_str()).await
}

pub async fn write_runtime(bundle: impl AsRef<Path>, binary_name: &str) -> Result<()> {
    write_str_to_file(bundle.as_ref().join(RUNTIME_FILE_NAME), binary_name).await
}

pub async fn mount_rootfs(m: &Mount, target: impl AsRef<Path>) -> Result<()> {
    let mount_type = m.type_.to_string();
    let source = m.source.to_string();
    let options = m.options.to_vec();
    let rootfs = target.as_ref().to_owned();
    asyncify(move || -> Result<()> {
        let mount_type = Some(mount_type.as_str()).filter(|s| !s.is_empty());
        let source = Some(source.as_str()).filter(|s| !s.is_empty());
        crate::mount::mount_rootfs(mount_type, source, options.as_slice(), &rootfs)
    })
    .await
}

pub async fn mkdir(path: impl AsRef<Path>, mode: mode_t) -> Result<()> {
    // mode_t is u16 on macOS, while DirBuilder takes u32.
    #[allow(clippy::unnecessary_cast)]
    let mode = mode as u32;
    let path = path.as_ref();
    match tokio::fs::DirBuilder::new().mode(mode).create(path).await {
        Ok(()) => Ok(()),
        Err(e)
            if e.kind() == std::io::ErrorKind::AlreadyExists
                && tokio::fs::metadata(path).await.is_ok_and(|m| m.is_dir()) =>
        {
            Ok(())
        }
        Err(e) => Err(Error::IoError {
            context: format!("mkdir {}", path.display()),
            err: e,
        }),
    }
}

#[cfg(test)]
mod tests {
    use crate::util::{read_file_to_str, write_str_to_file};

    #[cfg(unix)]
    #[tokio::test]
    async fn mkdir_accepts_directories_but_rejects_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("child");
        super::mkdir(&path, 0o700).await.unwrap();
        super::mkdir(&path, 0o755).await.unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let file = dir.path().join("file");
        std::fs::write(&file, "untouched").unwrap();
        assert!(super::mkdir(&file, 0o700).await.is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "untouched");
    }

    #[tokio::test]
    async fn test_read_write_str() {
        let tmpdir = tempfile::tempdir().unwrap();
        let tmp_file = tmpdir.path().join("test");
        let test_str = "this is a test";
        write_str_to_file(&tmp_file, test_str).await.unwrap();
        let read_str = read_file_to_str(&tmp_file).await.unwrap();
        assert_eq!(read_str, test_str);
    }
}

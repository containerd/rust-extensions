#![cfg(not(windows))]
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

use std::{
    collections::HashMap,
    env,
    ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, Not},
    path::Path,
};

use lazy_static::lazy_static;
use log::error;
#[cfg(target_os = "linux")]
use nix::mount::{mount, MsFlags};
#[cfg(target_os = "linux")]
use nix::unistd::{fork, ForkResult};

use crate::error::{Error, Result};
#[cfg(not(feature = "async"))]
use crate::monitor::{monitor_subscribe, wait_pid, Topic};

#[cfg(target_os = "linux")]
struct Flag {
    clear: bool,
    flags: MsFlags,
}

const OVERLAY_LOWERDIR_PREFIX: &str = "lowerdir=";

#[cfg(target_os = "linux")]
lazy_static! {
    static ref MOUNT_FLAGS: HashMap<&'static str, Flag> = {
        let mut mf = HashMap::new();
        let zero: MsFlags = MsFlags::from_bits(0).unwrap();
        mf.insert(
            "async",
            Flag {
                clear: true,
                flags: MsFlags::MS_SYNCHRONOUS,
            },
        );
        mf.insert(
            "atime",
            Flag {
                clear: true,
                flags: MsFlags::MS_NOATIME,
            },
        );
        mf.insert(
            "bind",
            Flag {
                clear: false,
                flags: MsFlags::MS_BIND,
            },
        );
        mf.insert(
            "defaults",
            Flag {
                clear: false,
                flags: zero,
            },
        );
        mf.insert(
            "dev",
            Flag {
                clear: true,
                flags: MsFlags::MS_NODEV,
            },
        );
        mf.insert(
            "diratime",
            Flag {
                clear: true,
                flags: MsFlags::MS_NODIRATIME,
            },
        );
        mf.insert(
            "dirsync",
            Flag {
                clear: false,
                flags: MsFlags::MS_DIRSYNC,
            },
        );
        mf.insert(
            "exec",
            Flag {
                clear: true,
                flags: MsFlags::MS_NOEXEC,
            },
        );
        mf.insert(
            "mand",
            Flag {
                clear: false,
                flags: MsFlags::MS_MANDLOCK,
            },
        );
        mf.insert(
            "noatime",
            Flag {
                clear: false,
                flags: MsFlags::MS_NOATIME,
            },
        );
        mf.insert(
            "nodev",
            Flag {
                clear: false,
                flags: MsFlags::MS_NODEV,
            },
        );
        mf.insert(
            "nodiratime",
            Flag {
                clear: false,
                flags: MsFlags::MS_NODIRATIME,
            },
        );
        mf.insert(
            "noexec",
            Flag {
                clear: false,
                flags: MsFlags::MS_NOEXEC,
            },
        );
        mf.insert(
            "nomand",
            Flag {
                clear: true,
                flags: MsFlags::MS_MANDLOCK,
            },
        );
        mf.insert(
            "norelatime",
            Flag {
                clear: true,
                flags: MsFlags::MS_RELATIME,
            },
        );
        mf.insert(
            "nostrictatime",
            Flag {
                clear: true,
                flags: MsFlags::MS_STRICTATIME,
            },
        );
        mf.insert(
            "nosuid",
            Flag {
                clear: false,
                flags: MsFlags::MS_NOSUID,
            },
        );
        mf.insert(
            "rbind",
            Flag {
                clear: false,
                flags: MsFlags::MS_BIND | MsFlags::MS_REC,
            },
        );
        mf.insert(
            "relatime",
            Flag {
                clear: false,
                flags: MsFlags::MS_RELATIME,
            },
        );
        mf.insert(
            "remount",
            Flag {
                clear: false,
                flags: MsFlags::MS_REMOUNT,
            },
        );
        mf.insert(
            "ro",
            Flag {
                clear: false,
                flags: MsFlags::MS_RDONLY,
            },
        );
        mf.insert(
            "rw",
            Flag {
                clear: true,
                flags: MsFlags::MS_RDONLY,
            },
        );
        mf.insert(
            "strictatime",
            Flag {
                clear: false,
                flags: MsFlags::MS_STRICTATIME,
            },
        );
        mf.insert(
            "suid",
            Flag {
                clear: true,
                flags: MsFlags::MS_NOSUID,
            },
        );
        mf.insert(
            "sync",
            Flag {
                clear: false,
                flags: MsFlags::MS_SYNCHRONOUS,
            },
        );
        mf
    };
    static ref PROPAGATION_TYPES: MsFlags = MsFlags::MS_SHARED
        .bitor(MsFlags::MS_PRIVATE)
        .bitor(MsFlags::MS_SLAVE)
        .bitor(MsFlags::MS_UNBINDABLE);
    static ref MS_PROPAGATION: MsFlags = PROPAGATION_TYPES
        .bitor(MsFlags::MS_REC)
        .bitor(MsFlags::MS_SILENT);
    static ref MS_BIND_RO: MsFlags = MsFlags::MS_BIND.bitor(MsFlags::MS_RDONLY);
}

fn options_size(options: &[String]) -> usize {
    options.iter().fold(0, |sum, x| sum + x.len())
}

fn longest_common_prefix(dirs: &[String]) -> &str {
    if dirs.is_empty() {
        return "";
    }

    let first_dir = &dirs[0];

    for (i, byte) in first_dir.as_bytes().iter().enumerate() {
        for dir in dirs {
            if dir.as_bytes().get(i) != Some(byte) {
                let mut end = i;
                // guaranteed not to underflow since is_char_boundary(0) is always true
                while !first_dir.is_char_boundary(end) {
                    end -= 1;
                }

                return &first_dir[0..end];
            }
        }
    }

    first_dir
}

// NOTE: the snapshot id is based on digits.
// in order to avoid to get snapshots/x, should be back to parent dir.
// however, there is assumption that the common dir is ${root}/io.containerd.v1.overlayfs/snapshots.
#[cfg(target_os = "linux")]
fn trim_flawed_dir(s: &str) -> String {
    match s.ends_with('/') {
        true => s.to_owned(),
        false => {
            // Iterate in reverse order to find the last '/', then return string in correct order
            s.chars()
                .rev()
                .skip_while(|x| *x != '/')
                .collect::<Vec<char>>()
                .iter()
                .rev()
                .collect::<String>()
        }
    }
}

#[cfg(target_os = "linux")]
#[derive(Default)]
struct LowerdirCompactor {
    options: Vec<String>,
    lowerdirs: Option<Vec<String>>,
    lowerdir_prefix: Option<String>,
}

#[cfg(target_os = "linux")]
impl LowerdirCompactor {
    fn new(options: &[String]) -> Self {
        Self {
            options: options.to_vec(),
            ..Self::default()
        }
    }

    fn lowerdirs(&mut self) -> &mut Self {
        self.lowerdirs = Some(
            self.options
                .iter()
                .filter(|x| x.starts_with(OVERLAY_LOWERDIR_PREFIX))
                .map(|x| x.strip_prefix(OVERLAY_LOWERDIR_PREFIX).unwrap_or(x))
                .flat_map(|x| x.split(':'))
                .map(str::to_string)
                .collect(),
        );
        self
    }

    fn lowerdir_prefix(&mut self) -> &mut Self {
        self.lowerdir_prefix = self
            .lowerdirs
            .as_ref()
            .filter(|x| x.len() > 1)
            .map(|x| longest_common_prefix(x))
            .map(trim_flawed_dir)
            .filter(|x| !x.is_empty() && x != "/");
        self
    }

    fn compact(&mut self) -> (Option<String>, Vec<String>) {
        self.lowerdirs().lowerdir_prefix();
        if let Some(chdir) = &self.lowerdir_prefix {
            let lowerdir_str = self
                .lowerdirs
                .as_ref()
                .unwrap_or(&Vec::new())
                .iter()
                .map(|x| x.strip_prefix(chdir).unwrap_or(x))
                .collect::<Vec<&str>>()
                .join(":");
            let replace = |x: &str| -> String {
                if x.starts_with(OVERLAY_LOWERDIR_PREFIX) {
                    format!("{}{}", OVERLAY_LOWERDIR_PREFIX, lowerdir_str)
                } else {
                    x.to_string()
                }
            };
            (
                self.lowerdir_prefix.clone(),
                self.options
                    .iter()
                    .map(|x| replace(x))
                    .collect::<Vec<String>>(),
            )
        } else {
            (None, self.options.to_vec())
        }
    }
}

enum MountExitCode {
    NixUnknownErr,
    ChdirErr,
    Success,
    NixOtherErr(i32),
}

impl From<i32> for MountExitCode {
    fn from(code: i32) -> Self {
        match code {
            -2 => MountExitCode::NixUnknownErr,
            -1 => MountExitCode::ChdirErr,
            0 => MountExitCode::Success,
            _ => MountExitCode::NixOtherErr(code),
        }
    }
}

impl From<MountExitCode> for i32 {
    fn from(code: MountExitCode) -> Self {
        match code {
            MountExitCode::NixUnknownErr => -2,
            MountExitCode::ChdirErr => -1,
            MountExitCode::Success => 0,
            MountExitCode::NixOtherErr(errno) => errno,
        }
    }
}

impl From<nix::errno::Errno> for MountExitCode {
    fn from(err: nix::errno::Errno) -> Self {
        match err {
            nix::errno::Errno::UnknownErrno => MountExitCode::NixUnknownErr,
            _ => MountExitCode::NixOtherErr(err as i32),
        }
    }
}

impl From<MountExitCode> for nix::errno::Errno {
    fn from(code: MountExitCode) -> Self {
        match code {
            MountExitCode::NixOtherErr(errno) => nix::errno::Errno::from_i32(errno),
            _ => nix::errno::Errno::UnknownErrno,
        }
    }
}

impl From<MountExitCode> for Result<()> {
    fn from(code: MountExitCode) -> Self {
        match code {
            MountExitCode::NixUnknownErr => Err(other!(
                "mount process exit unexpectedly, exit code: {}",
                nix::errno::Errno::from(code)
            )),
            MountExitCode::ChdirErr => Err(other!("mount process exit unexpectedly: chdir failed")),
            MountExitCode::Success => Ok(()),
            MountExitCode::NixOtherErr(errno) => Err(other!(
                "mount process exit unexpectedly, exit code: {}",
                nix::errno::Errno::from_i32(errno)
            )),
        }
    }
}

#[cfg(not(feature = "async"))]
#[cfg(target_os = "linux")]
pub fn mount_rootfs(
    fs_type: Option<&str>,
    source: Option<&str>,
    options: &[String],
    target: impl AsRef<Path>,
) -> Result<()> {
    //TODO add helper to mount fuse
    let max_size = page_size::get();
    // avoid hitting one page limit of mount argument buffer
    //
    // NOTE: 512 id a buffer during pagesize check.
    let (chdir, options) =
        if fs_type.unwrap_or("") == "overlay" && options_size(options) >= max_size - 512 {
            LowerdirCompactor::new(options).compact()
        } else {
            (None, options.to_vec())
        };

    let mut flags: MsFlags = MsFlags::from_bits(0).unwrap();
    let mut data = Vec::new();
    options.iter().for_each(|x| {
        if let Some(f) = MOUNT_FLAGS.get(x.as_str()) {
            if f.clear {
                flags.bitand_assign(f.flags.not());
            } else {
                flags.bitor_assign(f.flags)
            }
        } else {
            data.push(x.as_str())
        }
    });

    let opt = data.join(",");
    if opt.len() > max_size {
        return Err(other!("mount option is too long"));
    }

    let data = if !data.is_empty() {
        Some(opt.as_str())
    } else {
        None
    };

    let s = monitor_subscribe(Topic::All)?;
    match unsafe { fork() } {
        Ok(ForkResult::Parent { child, .. }) => {
            let code: MountExitCode = wait_pid(i32::from(child), s).into();
            code.into()
        }
        Ok(ForkResult::Child) => {
            if let Some(workdir) = chdir {
                env::set_current_dir(Path::new(&workdir)).unwrap_or_else(|_| {
                    unsafe { libc::_exit(i32::from(MountExitCode::ChdirErr)) };
                });
            }
            // mount with non-propagation first, or remount with changed data
            let oflags = flags.bitand(PROPAGATION_TYPES.not());
            let zero: MsFlags = MsFlags::from_bits(0).unwrap();
            if flags.bitand(MsFlags::MS_REMOUNT).eq(&zero) || data.is_some() {
                mount(source, target.as_ref(), fs_type, oflags, data).unwrap_or_else(|err| {
                    error!(
                        "Mount {:?} to {} failed: {}",
                        source,
                        target.as_ref().display(),
                        err
                    );
                    let code: MountExitCode = err.into();
                    unsafe { libc::_exit(code.into()) };
                });
            }
            // change the propagation type
            if flags.bitand(*PROPAGATION_TYPES).ne(&zero) {
                mount::<str, Path, str, str>(
                    None,
                    target.as_ref(),
                    None,
                    flags.bitand(*MS_PROPAGATION),
                    None,
                )
                .unwrap_or_else(|err| {
                    error!(
                        "Change {} mount propagation faied: {}",
                        target.as_ref().display(),
                        err
                    );
                    let code: MountExitCode = err.into();
                    unsafe { libc::_exit(code.into()) };
                });
            }
            if oflags.bitand(*MS_BIND_RO).eq(&MS_BIND_RO) {
                mount::<str, Path, str, str>(
                    None,
                    target.as_ref(),
                    None,
                    oflags.bitor(MsFlags::MS_REMOUNT),
                    None,
                )
                .unwrap_or_else(|err| {
                    error!(
                        "Change {} read-only failed: {}",
                        target.as_ref().display(),
                        err
                    );
                    let code: MountExitCode = err.into();
                    unsafe { libc::_exit(code.into()) };
                });
            }
            unsafe { libc::_exit(i32::from(MountExitCode::Success)) };
        }
        Err(_) => Err(other!("fork mount process failed")),
    }
}

#[cfg(feature = "async")]
#[cfg(target_os = "linux")]
pub fn mount_rootfs(
    fs_type: Option<&str>,
    source: Option<&str>,
    options: &[String],
    target: impl AsRef<Path>,
) -> Result<()> {
    //TODO add helper to mount fuse
    //TODO compactLowerdirOption for overlay
    let mut flags: MsFlags = MsFlags::from_bits(0).unwrap();
    let mut data = Vec::new();
    options.iter().for_each(|x| {
        if let Some(f) = MOUNT_FLAGS.get(x.as_str()) {
            if f.clear {
                flags.bitand_assign(f.flags.not());
            } else {
                flags.bitor_assign(f.flags)
            }
        } else {
            data.push(x.as_str())
        }
    });
    let opt = data.join(",");

    let data = if !data.is_empty() {
        Some(opt.as_str())
    } else {
        None
    };

    // mount with non-propagation first, or remount with changed data
    let oflags = flags.bitand(PROPAGATION_TYPES.not());
    let zero: MsFlags = MsFlags::from_bits(0).unwrap();
    if flags.bitand(MsFlags::MS_REMOUNT).eq(&zero) || data.is_some() {
        mount(source, target.as_ref(), fs_type, oflags, data).map_err(mount_error!(
            e,
            "Mount {:?} to {}",
            source,
            target.as_ref().display()
        ))?;
    }

    // change the propagation type
    if flags.bitand(*PROPAGATION_TYPES).ne(&zero) {
        mount::<str, Path, str, str>(None, target.as_ref(), None, *MS_PROPAGATION, None).map_err(
            mount_error!(e, "Change {} mount propagation", target.as_ref().display()),
        )?;
    }

    if oflags.bitand(*MS_BIND_RO).eq(&MS_BIND_RO) {
        mount::<str, Path, str, str>(
            None,
            target.as_ref(),
            None,
            oflags.bitor(MsFlags::MS_REMOUNT),
            None,
        )
        .map_err(mount_error!(
            e,
            "Change {} read-only",
            target.as_ref().display()
        ))?;
    }

    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn mount_rootfs(
    fs_type: Option<&str>,
    source: Option<&str>,
    options: &[String],
    target: impl AsRef<Path>,
) -> Result<()> {
    Err(Error::Unimplemented("start".to_string()))
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use super::*;

    #[test]
    fn test_trim_flawed_dir() {
        let mut tcases: Vec<(&str, String)> = Vec::new();
        tcases.push(("/.foo-_bar/foo", "/.foo-_bar/".to_string()));
        tcases.push(("/.foo-_bar/foo/", "/.foo-_bar/foo/".to_string()));
        tcases.push(("/.foo-_bar/foo/bar", "/.foo-_bar/foo/".to_string()));
        tcases.push(("/.foo-_bar/foo/bar/", "/.foo-_bar/foo/bar/".to_string()));
        for (case, expected) in tcases {
            let res = trim_flawed_dir(case);
            assert_eq!(res, expected);
        }
    }

    #[test]
    fn test_longest_common_prefix() {
        let mut tcases: Vec<(Vec<String>, String)> = Vec::new();
        tcases.push((vec![], "".to_string()));
        tcases.push((vec!["foo".to_string()], "foo".to_string()));
        tcases.push((vec!["foo".to_string(), "bar".to_string()], "".to_string()));
        tcases.push((
            vec!["foo".to_string(), "foo".to_string()],
            "foo".to_string(),
        ));
        tcases.push((
            vec!["foo".to_string(), "foobar".to_string()],
            "foo".to_string(),
        ));
        tcases.push((
            vec!["foo".to_string(), "".to_string(), "foobar".to_string()],
            "".to_string(),
        ));
        for (case, expected) in tcases {
            let res = longest_common_prefix(&case);
            assert_eq!(res, expected);
        }
    }

    #[test]
    fn test_compact_lowerdir_option() {
        let mut tcases: Vec<(Vec<String>, Option<String>, Vec<String>)> = Vec::new();
        tcases.push((
            vec!["workdir=a".to_string()],
            None,
            vec!["workdir=a".to_string()],
        ));
        tcases.push((
            vec!["workdir=a".to_string(), "lowerdir=b".to_string()],
            None,
            vec!["workdir=a".to_string(), "lowerdir=b".to_string()],
        ));
        tcases.push((
            vec!["lowerdir=/snapshots/1/fs:/snapshots/10/fs".to_string()],
            Some("/snapshots/".to_string()),
            vec!["lowerdir=1/fs:10/fs".to_string()],
        ));
        tcases.push((
            vec![
                "workdir=a".to_string(),
                "lowerdir=/snapshots/1/fs:/snapshots/10/fs".to_string(),
            ],
            Some("/snapshots/".to_string()),
            vec!["workdir=a".to_string(), "lowerdir=1/fs:10/fs".to_string()],
        ));
        tcases.push((
            vec!["lowerdir=/snapshots/1/fs:/snapshots/10/fs:/snapshots/2/fs".to_string()],
            Some("/snapshots/".to_string()),
            vec!["lowerdir=1/fs:10/fs:2/fs".to_string()],
        ));
        tcases.push((
            vec![
                "workdir=a".to_string(),
                "lowerdir=/snapshots/1/fs:/snapshots/10/fs:/snapshots/2/fs".to_string(),
            ],
            Some("/snapshots/".to_string()),
            vec![
                "workdir=a".to_string(),
                "lowerdir=1/fs:10/fs:2/fs".to_string(),
            ],
        ));
        tcases.push((
            vec!["lowerdir=/snapshots/1/fs:/other_snapshots/1/fs".to_string()],
            None,
            vec!["lowerdir=/snapshots/1/fs:/other_snapshots/1/fs".to_string()],
        ));
        tcases.push((
            vec![
                "workdir=a".to_string(),
                "lowerdir=/snapshots/1/fs:/other_snapshots/1/fs".to_string(),
            ],
            None,
            vec![
                "workdir=a".to_string(),
                "lowerdir=/snapshots/1/fs:/other_snapshots/1/fs".to_string(),
            ],
        ));
        for (case, expected_chdir, expected_options) in tcases {
            let (chdir, options) = LowerdirCompactor::new(&case).compact();
            assert_eq!(chdir, expected_chdir);
            assert_eq!(options, expected_options);
        }
    }
}

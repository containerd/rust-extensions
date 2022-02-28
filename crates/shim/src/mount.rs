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

use std::collections::HashMap;
use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, Not};
use std::path::Path;

use lazy_static::lazy_static;
#[cfg(target_os = "linux")]
use nix::mount::{mount, MsFlags};

use crate::error::{Error, Result};

#[cfg(target_os = "linux")]
struct Flag {
    clear: bool,
    flags: MsFlags,
}

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
    if flags.bitand(MsFlags::MS_REMOUNT).eq(&zero) || data != None {
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

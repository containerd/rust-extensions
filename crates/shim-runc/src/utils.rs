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

#![allow(dead_code)]

use std::collections::HashMap;
use std::io;
use std::path::Path;

use nix::libc::c_ulong;
use nix::mount::MsFlags;
use runc::error::Error;
use sys_mount::{Mount, MountFlags, SupportedFilesystems};

use crate::process::config::MountConfig;

/// A mount helper, similar to Go version.
pub struct MountUtil {
    /// Type specifies the host-specific of the mount.
    pub mount_type: String,
    /// Source specifies where to mount from. Depending on the host system, this can be a source path or device.
    pub source: String,
    /// Options contains zero or more fstab-style mount options. Typically, these are platform specific.
    pub options: Vec<String>,
    pub mount: Mount,
}

// impl MountUtil {
//     pub fn new<P>(&self, target: P) -> io::Result<Mount>
//     where
//         P: AsRef<Path>,
//     {
//         let fs = SupportedFilesystems::new()?;
//         Some(Mount::new(&self.source, target, &fs, MountFlags::empty(), None))
//     }
// }

// mount propagation types
const PTYPES: c_ulong = MsFlags::MS_SHARED.bits()
    | MsFlags::MS_PRIVATE.bits()
    | MsFlags::MS_SLAVE.bits()
    | MsFlags::MS_UNBINDABLE.bits();

const BROFLAGS: c_ulong = MsFlags::MS_BIND.bits() | MsFlags::MS_RDONLY.bits();

pub fn mount<T>(mnt: MountConfig, target: T) -> io::Result<()>
where
    T: AsRef<Path>,
{
    // FIXME: appropriate filesystem from mnt.mount_type
    let _fs = SupportedFilesystems::new()?;
    // FIXME: here is ugly hack because it doesn't resolve the common lower directory
    // see https://github.com/containerd/containerd/blob/main/mount/mount_linux.go#L259
    // if mnt.mount_type = "overlay" && size >= PAGESIZE - 512 { chdir =.. }
    let _chdir = "";

    // also, appropriate flag should be set
    let (flags, data, loop_setup) = parse_mount_options(&mnt.options);

    // Ensure propagation type change flags aren't included in other calls.
    let oflags = flags & !PTYPES;

    if flags & MsFlags::MS_REMOUNT.bits() == 0 || !data.is_empty() {
        // Initial call applying all non-propagation flags for mount
        // or remount with changed data
        let source = &mnt.source;
        if loop_setup {
            unimplemented!()
        }

        // FIXME: appropriate mount point with chdir
        let f = MountFlags::from_bits_truncate(oflags as u64);
        let _ = sys_mount::Mount::new(source, &target, mnt.mount_type.as_str(), f, Some(&data))?;
    }

    if flags & PTYPES != 0 {
        // change the propagation type.
        let f = MountFlags::from_bits_truncate(
            flags & (PTYPES | MsFlags::MS_REC.bits() | MsFlags::MS_SILENT.bits()) as u64,
        );
        let _ = sys_mount::Mount::new("", &target, "", f, Some(&data))?;
    }

    if oflags & BROFLAGS == BROFLAGS {
        // remount the bind to apply read only.
        let f = MountFlags::from_bits_truncate((oflags | MsFlags::MS_REMOUNT.bits()) as u64);
        let _ = sys_mount::Mount::new("", &target, "", f, None)?;
    }
    Ok(())
}

fn parse_mount_options(opts: &[String]) -> (c_ulong, String, bool) {
    let flags = make_flag_table();
    // If the option does not exist in the flags table or the flag
    // is not supported on the platform,
    // then it is a data value for a specific fs type
    let mut flag = 0;
    let mut loop_setup = false;
    let mut data = vec![];
    for opt in opts {
        match flags.get(opt) {
            Some((true, f)) => flag &= !f,
            Some((false, f)) => flag |= f,
            _ => {
                if opt == "loop" {
                    loop_setup = true;
                } else {
                    data.push(opt.clone());
                }
            }
        }
    }
    (flag, data.join(","), loop_setup)
}

#[rustfmt::skip]
fn make_flag_table() -> HashMap<String, (bool, c_ulong)> {
    const TABLE: [(&str, (bool, c_ulong)); 25] = [
        ("async",           (true,  MsFlags::MS_SYNCHRONOUS.bits())),
        ("atime",           (true,  MsFlags::MS_NOATIME.bits())),
        ("bind",            (false, MsFlags::MS_BIND.bits())),
        ("defaults",        (false, 0)),
        ("dev",             (true,  MsFlags::MS_SYNCHRONOUS.bits())),
        ("diratime",        (true,  MsFlags::MS_SYNCHRONOUS.bits())),
        ("dirsync",         (false, MsFlags::MS_SYNCHRONOUS.bits())),
        ("exec",            (true,  MsFlags::MS_SYNCHRONOUS.bits())),
        ("mand",            (false, MsFlags::MS_SYNCHRONOUS.bits())),
        ("noatime",         (false, MsFlags::MS_SYNCHRONOUS.bits())),
        ("nodev",           (false, MsFlags::MS_SYNCHRONOUS.bits())),
        ("nodiratime",      (false, MsFlags::MS_SYNCHRONOUS.bits())),
        ("noexec",          (false, MsFlags::MS_SYNCHRONOUS.bits())),
        ("nomand",          (true,  MsFlags::MS_SYNCHRONOUS.bits())),
        ("norelatime",      (true,  MsFlags::MS_SYNCHRONOUS.bits())),
        ("nostrictatime",   (true,  MsFlags::MS_SYNCHRONOUS.bits())),
        ("nosuid",          (false, MsFlags::MS_SYNCHRONOUS.bits())),
        ("rbind",           (false, MsFlags::MS_SYNCHRONOUS.bits())),
        ("relatime",        (false, MsFlags::MS_SYNCHRONOUS.bits())),
        ("remount",         (false, MsFlags::MS_SYNCHRONOUS.bits())),
        ("ro",              (false, MsFlags::MS_SYNCHRONOUS.bits())),
        ("rw",              (true,  MsFlags::MS_SYNCHRONOUS.bits())),
        ("strictatime",     (false, MsFlags::MS_SYNCHRONOUS.bits())),
        ("suid",            (true, MsFlags::MS_SYNCHRONOUS.bits())),
        ("sync",            (false, MsFlags::MS_SYNCHRONOUS.bits())),
    ];
    TABLE.iter().map(|&(k, v)| (k.to_string(), v)).collect()
}

// pub fn unmount<P>(path: P) -> io::Result<()> {

// }
const DEFAULT_RUNC_ROOT: &str = "/run/containerd/runc";
const RUNC_NAME: &str = "runc";

// NOTE: checkpoint is not supported now, then skipping criu for args.
pub fn new_runc<R, P>(
    root: R,
    path: P,
    namespace: String,
    runtime: &str,
    systemd_cgroup: bool,
) -> Result<runc::Client, Error>
where
    R: AsRef<str>,
    P: AsRef<Path>,
{
    let root = Path::new(if root.as_ref().is_empty() {
        DEFAULT_RUNC_ROOT
    } else {
        root.as_ref()
    })
    .join(namespace);
    let log = path.as_ref().join("log.json");
    let runtime = if runtime.is_empty() {
        RUNC_NAME
    } else {
        runtime
    };

    runc::Config::new()
        .command(runtime)
        .log(log)
        .log_format_json()
        .root(root)
        .systemd_cgroup(systemd_cgroup)
        .build()
}

pub fn new_async_runc<R, P>(
    root: R,
    path: P,
    namespace: String,
    runtime: &str,
    systemd_cgroup: bool,
) -> Result<runc::AsyncClient, Error>
where
    R: AsRef<str>,
    P: AsRef<Path>,
{
    let root = Path::new(if root.as_ref().is_empty() {
        DEFAULT_RUNC_ROOT
    } else {
        root.as_ref()
    })
    .join(namespace);
    let log = path.as_ref().join("log.json");
    let runtime = if runtime.is_empty() {
        RUNC_NAME
    } else {
        runtime
    };

    runc::Config::new()
        .command(runtime)
        .log(log)
        .log_format_json()
        .root(root)
        .systemd_cgroup(systemd_cgroup)
        .build_async()
}

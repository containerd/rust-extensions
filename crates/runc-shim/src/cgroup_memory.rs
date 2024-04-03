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

#![cfg(target_os = "linux")]

use std::{
    collections::HashMap,
    mem::size_of,
    os::{
        fd::AsFd,
        unix::io::{AsRawFd, FromRawFd},
    },
    path::Path,
    sync::Arc,
};

use containerd_shim::{
    error::{Error, Result},
    io_error, other_error,
};
use nix::sys::{
    eventfd::{EfdFlags, EventFd},
    inotify,
};
use tokio::{
    fs::{self, read_to_string, File},
    io::AsyncReadExt,
    sync::mpsc::{self, Receiver},
};

pub const DEFAULT_CGROUPV2_PATH: &str = "/sys/fs/cgroup";

pub async fn get_path_from_cgorup(pid: u32) -> Result<String> {
    let proc_path = format!("/proc/{}/cgroup", pid);
    let path_string = read_to_string(&proc_path)
        .await
        .map_err(io_error!(e, "open {}.", &proc_path))?;

    let (_, path) = path_string
        .lines()
        .find(|line| line.contains("memory"))
        .ok_or(Error::Other("Memory line not found".into()))?
        .split_once(":memory:")
        .ok_or(Error::Other("Failed to parse memory line".into()))?;

    Ok(path.to_string())
}

pub async fn get_existing_cgroup_mem_path(pid_path: String) -> Result<(String, String)> {
    let (mut mount_root, mount_point) = get_path_from_mountinfo().await?;
    if mount_root == "/" {
        mount_root = String::from("");
    }
    let mount_root = pid_path.trim_start_matches(&mount_root).to_string();
    Ok((mount_root, mount_point))
}

async fn get_path_from_mountinfo() -> Result<(String, String)> {
    let mountinfo_path = "/proc/self/mountinfo";
    let mountinfo_string =
        read_to_string(mountinfo_path)
            .await
            .map_err(io_error!(e, "open {}.", mountinfo_path))?;

    let line = mountinfo_string
        .lines()
        .find(|line| line.contains("cgroup") && line.contains("memory"))
        .ok_or(Error::Other(
            "Lines containers cgroup and memory not found in mountinfo".into(),
        ))?;

    parse_memory_mountroot(line)
}

fn parse_memory_mountroot(line: &str) -> Result<(String, String)> {
    let mut columns = line.split_whitespace();
    let mount_root = columns.nth(3).ok_or(Error::Other(
        "Invalid input information about mountinfo".into(),
    ))?;
    let mount_point = columns.next().ok_or(Error::Other(
        "Invalid input information about mountinfo".into(),
    ))?;
    Ok((mount_root.to_string(), mount_point.to_string()))
}

pub async fn register_memory_event(
    key: &str,
    cg_dir: &Path,
    event_name: &str,
) -> Result<Receiver<String>> {
    let path = cg_dir.join(event_name);
    let event_file = fs::File::open(path.clone())
        .await
        .map_err(other_error!(e, "Error get path:"))?;

    let eventfd = EventFd::from_value_and_flags(0, EfdFlags::EFD_CLOEXEC)?;

    let event_control_path = cg_dir.join("cgroup.event_control");
    let data = format!("{} {}", eventfd.as_raw_fd(), event_file.as_raw_fd());
    fs::write(&event_control_path, data.clone())
        .await
        .map_err(other_error!(e, "Error write eventfd:"))?;

    let mut buf = [0u8; 8];

    let (sender, receiver) = mpsc::channel(128);
    let key = key.to_string();

    tokio::spawn(async move {
        let mut eventfd_file = unsafe { File::from_raw_fd(eventfd.as_raw_fd()) };
        loop {
            match eventfd_file.read(&mut buf).await {
                Ok(bytes_read) if bytes_read == 0 => return,
                Err(_) => return,
                _ => (),
            }
            if !Path::new(&event_control_path).exists() {
                return;
            }
            sender.send(key.clone()).await.unwrap();
        }
    });

    Ok(receiver)
}

fn memory_event_fd(path: &Path) -> Result<inotify::Inotify> {
    let instance = inotify::Inotify::init(inotify::InitFlags::empty())?;

    let fpath = path.join("memory.events");
    instance.add_watch(&fpath, inotify::AddWatchFlags::IN_MODIFY)?;

    let evpath = path.join("cgroup.events");
    instance.add_watch(&evpath, inotify::AddWatchFlags::IN_MODIFY)?;

    Ok(instance)
}

async fn parse_kv_file(cg_dir: &Path, file: &str) -> Result<HashMap<String, u32>> {
    let path = cg_dir.join(file);
    let mut map: HashMap<String, u32> = HashMap::new();

    let file_string = read_to_string(path.clone()).await.map_err(io_error!(
        e,
        "open {}.",
        path.to_string_lossy()
    ))?;

    for line in file_string.lines() {
        if let Some((key, val)) = line.split_once(" ") {
            let val = val.parse::<u32>()?;
            map.insert(key.to_string(), val);
        }
    }

    Ok(map)
}

pub async fn register_memory_event_v2(key: &str, cg_dir: &Path) -> Result<Receiver<String>> {
    let (sender, receiver) = mpsc::channel(128);
    let cg_dir = Arc::new(Box::from(cg_dir));
    let key = key.to_string();

    tokio::spawn(async move {
        let inotify = memory_event_fd(&cg_dir).unwrap();
        let mut eventfd_file = unsafe { File::from_raw_fd(inotify.as_fd().as_raw_fd()) };
        let mut buffer: [u8; 4096] = [0u8; 4096];

        let mut lastoom_map: HashMap<String, u32> = HashMap::new();
        loop {
            let nread = match eventfd_file.read(&mut buffer).await {
                Ok(nread) => nread,
                Err(_) => return,
            };
            if nread >= size_of::<libc::inotify_event>() {
                match parse_kv_file(&cg_dir, "memory.events").await {
                    Ok(mem_map) => {
                        let last_oom_kill = match lastoom_map.get(&key) {
                            Some(v) => v,
                            None => &0,
                        };

                        let oom_kill = match mem_map.get("oom_kill") {
                            Some(v) => v,
                            None => return,
                        };

                        if *oom_kill > *last_oom_kill {
                            sender.send(key.clone()).await.unwrap();
                        }
                        if *oom_kill > 0 {
                            lastoom_map.insert(key.to_string(), *oom_kill);
                        }
                    }
                    Err(_) => return,
                };

                let cg_map = match parse_kv_file(&cg_dir, "cgroup.events").await {
                    Ok(cg_map) => cg_map,
                    Err(_) => return,
                };
                match cg_map.get("populated") {
                    Some(v) if *v == 0 => return,
                    Some(_) => (),
                    None => return,
                };
            }
        }
    });

    Ok(receiver)
}

pub async fn get_path_from_cgorup_v2(pid: u32) -> Result<String> {
    let proc_path = format!("/proc/{}/cgroup", pid);
    let path_string = read_to_string(&proc_path)
        .await
        .map_err(io_error!(e, "open {}.", &proc_path))?;

    let (_, path) = path_string
        .lines()
        .nth(0)
        .ok_or(Error::Other(
            "Error happened while geting the path from cgroup of container process pid.".into(),
        ))?
        .split_once("::")
        .ok_or(Error::Other("Failed to parse memory line".into()))?;

    Ok(path.to_string())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use cgroups_rs::{
        hierarchies::{self, is_cgroup2_unified_mode},
        memory::MemController,
        Cgroup, CgroupPid,
    };
    use tokio::{fs::remove_file, io::AsyncWriteExt, process::Command};

    use crate::cgroup_memory;

    #[tokio::test]
    async fn test_cgroup_oom_monitor() {
        // Create a memory cgroup with limits on both memory and swap.
        let path = "cgroupv1_oom_monitor";
        let cg = Cgroup::new(hierarchies::auto(), path).unwrap();

        let mem_controller: &MemController = cg.controller_of().unwrap();
        mem_controller.set_limit(10 * 1024 * 1024).unwrap(); // 10M
        mem_controller.set_swappiness(0).unwrap();

        // Create a sh sub process, and let it wait for the stdinput.
        let mut child_process = Command::new("sh")
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();

        let pid = child_process.id().unwrap();

        // Add the sh subprocess to the cgroup.
        cg.add_task_by_tgid(CgroupPid::from(pid as u64)).unwrap();
        let mut rx: tokio::sync::mpsc::Receiver<String>;

        if !is_cgroup2_unified_mode() {
            // Set cgroupv1 oom monitor
            let path_from_cgorup = cgroup_memory::get_path_from_cgorup(pid).await.unwrap();
            let (mount_root, mount_point) =
                cgroup_memory::get_existing_cgroup_mem_path(path_from_cgorup)
                    .await
                    .unwrap();

            let mem_cgroup_path = mount_point + &mount_root;
            rx = cgroup_memory::register_memory_event(
                pid.to_string().as_str(),
                Path::new(&mem_cgroup_path),
                "memory.oom_control",
            )
            .await
            .unwrap();
        } else {
            // Set cgroupv2 oom monitor
            let path_from_cgorup = cgroup_memory::get_path_from_cgorup_v2(pid).await.unwrap();
            let mem_cgroup_path =
                cgroup_memory::DEFAULT_CGROUPV2_PATH.to_owned() + &path_from_cgorup;

            rx = cgroup_memory::register_memory_event_v2(
                pid.to_string().as_str(),
                Path::new(&mem_cgroup_path),
            )
            .await
            .unwrap();
        }
        // Exec the sh subprocess to a dd command that consumes more than 10M of memory.
        if let Some(mut stdin) = child_process.stdin.take() {
            stdin
                .write_all(b"exec dd if=/dev/zero of=/tmp/test_oom_monitor_file bs=11M count=1\n")
                .await
                .unwrap();
            stdin.flush().await.unwrap();
        }

        // Wait for the oom message.
        if let Some(item) = rx.recv().await {
            assert_eq!(pid.to_string(), item, "Receive error oom message");
        }
        // Clean.
        child_process.wait().await.unwrap();
        cg.delete().unwrap();
        remove_file("/tmp/test_oom_monitor_file").await.unwrap();
    }
}

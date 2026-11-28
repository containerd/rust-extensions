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
    fs::File,
    io,
    os::unix::{
        fs::FileExt,
        io::{AsFd, AsRawFd, RawFd},
    },
    path::{Component, Path, PathBuf},
};

use cgroups_rs::fs::hierarchies::UNIFIED_MOUNTPOINT;
use containerd_shim::{
    error::{Error, Result},
    io_error, other_error,
};
use log::warn;
use nix::sys::inotify::{AddWatchFlags, InitFlags, Inotify};
use tokio::{
    fs::{self, read_to_string},
    io::{unix::AsyncFd, AsyncReadExt, Interest},
    sync::mpsc::{self, Receiver},
};
use tokio_eventfd::EventFd;

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
        .map_err(other_error!("Error get path:"))?;
    let mut eventfd = EventFd::new(0, false).map_err(other_error!("Error create eventfd:"))?;
    let event_control_path = cg_dir.join("cgroup.event_control");
    let data = format!("{} {}", eventfd.as_raw_fd(), event_file.as_raw_fd());
    fs::write(&event_control_path, data.clone())
        .await
        .map_err(other_error!("Error write eventfd:"))?;

    let mut buf = [0u8; 8];

    let (sender, receiver) = mpsc::channel(128);
    let key = key.to_string();

    tokio::spawn(async move {
        loop {
            match eventfd.read(&mut buf).await {
                Ok(0) => return,
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

/// Returns the cgroup v2 path (relative to the unified hierarchy root) from the
/// contents of `/proc/<pid>/cgroup`, i.e. the `<path>` of its `0::<path>` line.
///
/// A cgroup outside the reader's cgroup namespace shows up as `/../<path>`,
/// which isn't under our mount of the unified hierarchy, and a removed one
/// gets a ` (deleted)` suffix; both are rejected.
///
/// `containerd_shim::cgroup::get_cgroups_v2_path_by_pid` isn't used because it
/// canonicalizes the joined path, which resolves `/../` before it can be
/// rejected.
fn parse_cgroup2_path(content: &str) -> Result<&str> {
    let path = content
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .ok_or_else(|| Error::Other("cgroup v2 line (0::) not found".into()))?;
    let valid = path.starts_with('/')
        && !path.ends_with(" (deleted)")
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)));
    if !valid {
        return Err(Error::Other(format!("invalid cgroup v2 path {:?}", path)));
    }
    Ok(path)
}

/// Returns the directory of the cgroup v2 group that `pid` belongs to.
pub async fn get_cgroup2_path(pid: u32) -> Result<PathBuf> {
    let proc_path = format!("/proc/{}/cgroup", pid);
    let content = read_to_string(&proc_path)
        .await
        .map_err(io_error!(e, "open {}.", &proc_path))?;
    let path = parse_cgroup2_path(&content)?;
    Ok(Path::new(UNIFIED_MOUNTPOINT).join(path.trim_start_matches('/')))
}

/// Returns the value of `key` in a flat-keyed cgroup v2 file such as
/// `memory.events` or `cgroup.events` (one `<key> <value>` pair per line).
fn parse_flat_keyed(content: &str, key: &str) -> Option<u64> {
    content.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        match (fields.next(), fields.next(), fields.next()) {
            (Some(k), Some(v), None) if k == key => v.parse().ok(),
            _ => None,
        }
    })
}

/// Reads `key` from an open flat-keyed cgroup file, from its start. Reading
/// through the fd opened when the watch started, rather than by path, ties the
/// watcher to that cgroup: once it is removed the read fails with `ENODEV`,
/// even if a new cgroup has been created at the same path.
///
/// These are small in-memory kernfs files, so the read doesn't block.
fn read_flat_keyed(file: &File, key: &str) -> io::Result<u64> {
    let mut buf = [0u8; 4096];
    let mut len = 0;
    while len < buf.len() {
        let n = file.read_at(&mut buf[len..], len as u64)?;
        if n == 0 {
            break;
        }
        len += n;
    }
    let content = String::from_utf8_lossy(&buf[..len]);
    parse_flat_keyed(&content, key)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("{} not found", key)))
}

/// Whether `e` means the cgroup is gone, which is how a watch normally ends.
fn cgroup_gone(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::NotFound || e.raw_os_error() == Some(libc::ENODEV)
}

/// `AsyncFd` needs `AsRawFd`, which nix's `Inotify` doesn't implement.
struct InotifyFd(Inotify);

impl AsRawFd for InotifyFd {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_fd().as_raw_fd()
    }
}

/// Waits for inotify events. Returns `Ok(false)` if a watch was removed
/// (`IN_IGNORED`, e.g. because the cgroup filesystem was unmounted), meaning
/// there is nothing more to wait for.
async fn wait_inotify(fd: &AsyncFd<InotifyFd>) -> io::Result<bool> {
    loop {
        let mut guard = fd.readable().await?;
        match guard.try_io(|fd| fd.get_ref().0.read_events().map_err(io::Error::from)) {
            Ok(events) => {
                return Ok(!events?
                    .iter()
                    .any(|e| e.mask.contains(AddWatchFlags::IN_IGNORED)));
            }
            Err(_would_block) => continue,
        }
    }
}

/// Watches a cgroup v2 group for OOM kills: sends `key` on the returned channel
/// each time the `oom_kill` counter in `<cg_dir>/memory.events` increases. Kills
/// counted before this call don't produce an event.
///
/// The watcher task (and its inotify fd) ends once the cgroup has no processes
/// left (`populated 0` in `cgroup.events`), after reporting any kill counted
/// by then, or once the files can't be read any more. Removing a cgroup doesn't
/// produce an inotify event, but a cgroup can only be removed once it is empty,
/// and becoming empty modifies `cgroup.events`.
pub async fn register_memory_event_v2(key: &str, cg_dir: &Path) -> Result<Receiver<String>> {
    let memory_events_path = cg_dir.join("memory.events");
    let cgroup_events_path = cg_dir.join("cgroup.events");

    let inotify = Inotify::init(InitFlags::IN_NONBLOCK | InitFlags::IN_CLOEXEC)
        .map_err(other_error!("inotify_init1"))?;
    for path in [&memory_events_path, &cgroup_events_path] {
        inotify
            .add_watch(path, AddWatchFlags::IN_MODIFY)
            .map_err(other_error!(format!(
                "inotify_add_watch {}",
                path.display()
            )))?;
    }
    // SAFETY: `InotifyFd` owns the inotify fd (through `Inotify`'s `OwnedFd`) and
    // always returns that same fd, so it stays open for the `AsyncFd`'s lifetime.
    let inotify =
        unsafe { AsyncFd::register_with_interest(InotifyFd(inotify), Interest::READABLE) }
            .map_err(other_error!("register inotify fd"))?;

    let open = |path: &Path| File::open(path).map_err(io_error!(e, "open {}.", path.display()));
    let memory_events = open(&memory_events_path)?;
    let cgroup_events = open(&cgroup_events_path)?;

    // The watches are in place before this read, so a kill after it is not missed.
    let mut oom_kills = read_flat_keyed(&memory_events, "oom_kill").map_err(io_error!(
        e,
        "read {}.",
        memory_events_path.display()
    ))?;

    let (sender, receiver) = mpsc::channel(128);
    let key = key.to_string();

    tokio::spawn(async move {
        let result: io::Result<()> = async {
            loop {
                // Read `populated` before `oom_kill`. The kernel counts an OOM
                // kill before it sends SIGKILL, so once `populated` reads 0 the
                // read of `oom_kill` that follows includes every kill.
                let populated = read_flat_keyed(&cgroup_events, "populated")? != 0;
                let n = read_flat_keyed(&memory_events, "oom_kill")?;
                if n > oom_kills {
                    oom_kills = n;
                    if sender.send(key.clone()).await.is_err() {
                        return Ok(());
                    }
                }
                if !populated || !wait_inotify(&inotify).await? {
                    return Ok(());
                }
            }
        }
        .await;
        if let Err(e) = result
            && !cgroup_gone(&e)
        {
            warn!("OOM watch of {} stopped: {}", key, e);
        }
    });

    Ok(receiver)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::{os::unix::process::ExitStatusExt, path::Path, time::Duration};

    use cgroups_rs::{
        fs::{
            hierarchies::{self, is_cgroup2_unified_mode},
            memory::MemController,
            Cgroup,
        },
        CgroupPid,
    };
    use tokio::{
        fs::remove_file,
        io::AsyncWriteExt,
        process::{Child, Command},
        time::timeout,
    };

    use crate::cgroup_memory;

    #[tokio::test]
    async fn test_cgroupv1_oom_monitor() {
        if !is_cgroup2_unified_mode() {
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

            // Set oom monitor
            let path_from_cgorup = cgroup_memory::get_path_from_cgorup(pid).await.unwrap();
            let (mount_root, mount_point) =
                cgroup_memory::get_existing_cgroup_mem_path(path_from_cgorup)
                    .await
                    .unwrap();

            let mem_cgroup_path = mount_point + &mount_root;
            let mut rx = cgroup_memory::register_memory_event(
                pid.to_string().as_str(),
                Path::new(&mem_cgroup_path),
                "memory.oom_control",
            )
            .await
            .unwrap();

            // Exec the sh subprocess to a dd command that consumes more than 10M of memory.
            if let Some(mut stdin) = child_process.stdin.take() {
                stdin
                    .write_all(
                        b"exec dd if=/dev/zero of=/tmp/test_oom_monitor_file bs=11M count=1\n",
                    )
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

    #[test]
    fn test_parse_cgroup2_path() {
        let content = "12:memory:/ignored\n0::/kubepods.slice/cri-containerd-abc.scope\n";
        assert_eq!(
            cgroup_memory::parse_cgroup2_path(content).unwrap(),
            "/kubepods.slice/cri-containerd-abc.scope"
        );
        assert_eq!(cgroup_memory::parse_cgroup2_path("0::/\n").unwrap(), "/");
        // cgroup v1 only.
        assert!(cgroup_memory::parse_cgroup2_path("4:memory:/foo\n").is_err());
        // Not absolute.
        assert!(cgroup_memory::parse_cgroup2_path("0::foo\n").is_err());
        assert!(cgroup_memory::parse_cgroup2_path("0::../foo\n").is_err());
        // Outside our cgroup namespace, as the kernel shows it.
        assert!(cgroup_memory::parse_cgroup2_path("0::/../foo\n").is_err());
        assert!(cgroup_memory::parse_cgroup2_path("0::/a/../../foo\n").is_err());
        // Removed.
        assert!(cgroup_memory::parse_cgroup2_path("0::/foo (deleted)\n").is_err());
    }

    #[test]
    fn test_parse_flat_keyed() {
        let memory_events = "low 0\nhigh 0\nmax 35\noom 2\noom_kill 1\noom_group_kill 0\n";
        assert_eq!(
            cgroup_memory::parse_flat_keyed(memory_events, "oom_kill"),
            Some(1)
        );
        assert_eq!(
            cgroup_memory::parse_flat_keyed(memory_events, "oom"),
            Some(2)
        );
        assert_eq!(
            cgroup_memory::parse_flat_keyed(memory_events, "missing"),
            None
        );
        assert_eq!(
            cgroup_memory::parse_flat_keyed("populated 0\nfrozen 0\n", "populated"),
            Some(0)
        );
        assert_eq!(
            cgroup_memory::parse_flat_keyed("oom_kill x\n", "oom_kill"),
            None
        );
        assert_eq!(
            cgroup_memory::parse_flat_keyed("oom_kill 1 2\n", "oom_kill"),
            None
        );
    }

    /// Creates the cgroup v2 group `name` with a 10M memory limit and no swap.
    /// Returns `None`, so the caller skips its test, when not running as root,
    /// or if swap can't be limited (no swap accounting) while the host has
    /// swap: the process meant to be OOM-killed could then be swapped out.
    pub(crate) fn oom_test_cgroup(name: &str) -> Option<Cgroup> {
        if !nix::unistd::geteuid().is_root() {
            eprintln!("skipping: creating a cgroup needs root");
            return None;
        }
        let cg = Cgroup::new(hierarchies::auto(), name).unwrap();
        let mem_controller: &MemController = cg.controller_of().unwrap();
        // 10M
        mem_controller.set_limit(10 * 1024 * 1024).unwrap();
        if mem_controller.set_memswap_limit(0).is_err() {
            let swaps = std::fs::read_to_string("/proc/swaps").unwrap_or_default();
            if swaps.lines().count() > 1 {
                eprintln!("skipping: swap is on but memory.swap.max can't be set");
                cg.delete().unwrap();
                return None;
            }
        }
        Some(cg)
    }

    /// Starts `sh` in `cg`, reading commands from the returned child's stdin.
    pub(crate) fn spawn_sh_in(cg: &Cgroup) -> Child {
        let child = Command::new("sh")
            .stdin(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        cg.add_task_by_tgid(CgroupPid::from(child.id().unwrap() as u64))
            .unwrap();
        child
    }

    /// Makes `sh` (from [`spawn_sh_in`]) exec a process that allocates more than
    /// its cgroup's 10M limit, and checks that it was OOM-killed.
    pub(crate) async fn exec_oom(sh: &mut Child) {
        let mut stdin = sh.stdin.take().unwrap();
        stdin
            .write_all(b"exec dd if=/dev/zero of=/dev/null bs=11M count=1\n")
            .await
            .unwrap();
        stdin.flush().await.unwrap();
        let status = sh.wait().await.unwrap();
        assert_eq!(status.signal(), Some(libc::SIGKILL), "dd wasn't OOM-killed");
    }

    #[tokio::test]
    async fn test_cgroupv2_oom_monitor() {
        if !is_cgroup2_unified_mode() {
            return;
        }
        let key = "cgroupv2_oom_monitor";
        let Some(cg) = oom_test_cgroup(key) else {
            return;
        };

        // Keeps the cgroup populated, standing in for the container's init.
        let mut init = spawn_sh_in(&cg);

        // A kill from before the watch starts doesn't produce an event.
        exec_oom(&mut spawn_sh_in(&cg)).await;

        let path = cgroup_memory::get_cgroup2_path(init.id().unwrap())
            .await
            .unwrap();
        let mut rx = cgroup_memory::register_memory_event_v2(key, &path)
            .await
            .unwrap();

        exec_oom(&mut spawn_sh_in(&cg)).await;
        let item = timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("no OOM event")
            .expect("watcher ended without an OOM event");
        assert_eq!(item, key);

        // Once the cgroup is empty the watcher ends, closing the channel, with no
        // more events (in particular none for the kill before the watch started).
        drop(init.stdin.take());
        init.wait().await.unwrap();
        let next = timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("watcher still running after the cgroup became empty");
        assert_eq!(next, None);

        cg.delete().unwrap();
    }
}

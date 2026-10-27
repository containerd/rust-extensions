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
    error::Error as StdError,
    fs,
    path::{Path, PathBuf},
};

use cgroups_rs::{
    fs::{
        cgroup::get_cgroups_relative_paths_by_pid, error::Result as CgResult, hierarchies, Cgroup,
        MaxValue, Subsystem,
    },
    CgroupPid,
};
use containerd_shim_protos::cgroups::metrics::{
    CPUStat, CPUUsage, MemoryEntry, MemoryStat, Metrics, PidsStat, Throttle,
};
use oci_spec::runtime::LinuxResources;

use crate::error::{Error, Result};

// OOM_SCORE_ADJ_MAX is from https://github.com/torvalds/linux/blob/master/include/uapi/linux/oom.h#L10
const OOM_SCORE_ADJ_MAX: i64 = 1000;

/// Adds the shim process to `shim_cgroup` (the runtime option of the same name,
/// when not empty) and sets its OOM score above its parent's.
#[cfg_attr(feature = "tracing", tracing::instrument(level = "Info"))]
pub fn set_cgroup_and_oom_score(pid: u32, shim_cgroup: &str) -> Result<()> {
    if pid == 0 {
        return Ok(());
    }

    if !shim_cgroup.is_empty() {
        add_task_to_cgroup(shim_cgroup, pid)?;
    }

    // set oom score
    adjust_oom_score(pid)
}

/// Add a process to the given relative cgroup path
#[cfg_attr(feature = "tracing", tracing::instrument(level = "Info"))]
pub fn add_task_to_cgroup(path: &str, pid: u32) -> Result<()> {
    let h = hierarchies::auto();
    // use relative path here, need to trim prefix '/'
    let path = path.trim_start_matches('/');

    Cgroup::load(h, path)
        .add_task_by_tgid(CgroupPid::from(pid as u64))
        .map_err(other_error!("add task to cgroup"))
}

/// Sets the OOM score for the process to the parents OOM score + 1
/// to ensure that they parent has a lower score than the shim
#[cfg_attr(feature = "tracing", tracing::instrument(level = "Info"))]
pub fn adjust_oom_score(pid: u32) -> Result<()> {
    let score = read_process_oom_score(std::os::unix::process::parent_id())?;
    if score < OOM_SCORE_ADJ_MAX {
        write_process_oom_score(pid, score + 1)?;
    }
    Ok(())
}

#[cfg_attr(feature = "tracing", tracing::instrument(level = "info"))]
fn read_process_oom_score(pid: u32) -> Result<i64> {
    let content = fs::read_to_string(format!("/proc/{}/oom_score_adj", pid))
        .map_err(io_error!(e, "read oom score"))?;
    let score = content
        .trim()
        .parse::<i64>()
        .map_err(other_error!("parse oom score"))?;
    Ok(score)
}

#[cfg_attr(feature = "tracing", tracing::instrument(level = "info"))]
fn write_process_oom_score(pid: u32, score: i64) -> Result<()> {
    fs::write(format!("/proc/{}/oom_score_adj", pid), score.to_string())
        .map_err(io_error!(e, "write oom score"))
}

/// Collect process cgroup stats, return only necessary parts of it
#[cfg_attr(feature = "tracing", tracing::instrument(level = "info"))]
pub fn collect_metrics(cgroup: &Cgroup) -> Result<Metrics> {
    let mut metrics = Metrics::new();

    // to make it easy, fill the necessary metrics only.
    for sub_system in Cgroup::subsystems(cgroup) {
        match sub_system {
            Subsystem::Cpu(cpu_ctr) => {
                let mut cpu_usage = CPUUsage::new();
                let mut throttle = Throttle::new();
                let stat = cpu_ctr.cpu().stat;
                for line in stat.lines() {
                    let parts = line.split(' ').collect::<Vec<&str>>();
                    if parts.len() != 2 {
                        Err(Error::Other(format!("invalid cpu stat line: {}", line)))?;
                    }

                    // https://github.com/opencontainers/runc/blob/dbe8434359ca35af1c1e10df42b1f4391c1e1010/libcontainer/cgroups/fs2/cpu.go#L70
                    match parts[0] {
                        "usage_usec" => {
                            cpu_usage.set_total(parts[1].parse::<u64>().unwrap());
                        }
                        "user_usec" => {
                            cpu_usage.set_user(parts[1].parse::<u64>().unwrap());
                        }
                        "system_usec" => {
                            cpu_usage.set_kernel(parts[1].parse::<u64>().unwrap());
                        }
                        "nr_periods" => {
                            throttle.set_periods(parts[1].parse::<u64>().unwrap());
                        }
                        "nr_throttled" => {
                            throttle.set_throttled_periods(parts[1].parse::<u64>().unwrap());
                        }
                        "throttled_usec" => {
                            throttle.set_throttled_time(parts[1].parse::<u64>().unwrap());
                        }
                        _ => {}
                    }
                }
                let mut cpu_stats = CPUStat::new();
                cpu_stats.set_throttling(throttle);
                cpu_stats.set_usage(cpu_usage);
                metrics.set_cpu(cpu_stats);
            }
            Subsystem::Mem(mem_ctr) => {
                let mem = mem_ctr.memory_stat();
                let mut mem_entry = MemoryEntry::new();
                mem_entry.set_usage(mem.usage_in_bytes);
                let mut mem_stat = MemoryStat::new();
                mem_stat.set_usage(mem_entry);
                mem_stat.set_total_inactive_file(mem.stat.total_inactive_file);
                metrics.set_memory(mem_stat);
            }
            Subsystem::Pid(pid_ctr) => {
                // ignore cgroup NotFound error
                let ignore_err = |cr: CgResult<u64>| -> CgResult<u64> {
                    cr.or_else(|e| if is_not_found(&e) { Ok(0) } else { Err(e) })
                };

                let mut pid_stats = PidsStat::new();
                pid_stats.set_current(
                    ignore_err(pid_ctr.get_pid_current())
                        .map_err(other_error!("get current pid"))?,
                );

                pid_stats.set_limit(
                    ignore_err(pid_ctr.get_pid_max().map(|val| match val {
                        // See https://github.com/opencontainers/runc/blob/dbe8434359ca35af1c1e10df42b1f4391c1e1010/libcontainer/cgroups/fs/pids.go#L55
                        MaxValue::Max => 0,
                        MaxValue::Value(val) => val as u64,
                    }))
                    .map_err(other_error!("get pid limit"))?,
                );
                metrics.set_pids(pid_stats)
            }
            _ => {}
        }
    }
    Ok(metrics)
}

// get_cgroup will return either cgroup v1 or v2 depending on system configuration
#[cfg_attr(feature = "tracing", tracing::instrument(level = "info"))]
pub fn get_cgroup(pid: u32) -> Result<Cgroup> {
    let hierarchies = hierarchies::auto();
    let cgroup = if hierarchies.v2() {
        let path = get_cgroups_v2_path_by_pid(pid)?;
        Cgroup::load(hierarchies, path)
    } else {
        // get container main process cgroup
        let path =
            get_cgroups_relative_paths_by_pid(pid).map_err(other_error!("get process cgroup"))?;
        Cgroup::load_with_relative_paths(hierarchies::auto(), Path::new("."), path)
    };
    Ok(cgroup)
}

/// Get the cgroups v2 path given a PID
#[cfg_attr(feature = "tracing", tracing::instrument(level = "info"))]
pub fn get_cgroups_v2_path_by_pid(pid: u32) -> Result<PathBuf> {
    // todo: should upstream to cgroups-rs
    let path = format!("/proc/{}/cgroup", pid);
    let content = fs::read_to_string(path).map_err(io_error!(e, "read cgroup"))?;
    let content = content.lines().next().unwrap_or("");

    let Ok(path) = parse_cgroups_v2_path(content)?.canonicalize() else {
        return Err(Error::Other("cgroup path not found".to_string()));
    };
    Ok(path)
}

// https://github.com/opencontainers/runc/blob/1950892f69597aa844cbf000fbdf77610dda3a44/libcontainer/cgroups/fs2/defaultpath.go#L83
#[cfg_attr(feature = "tracing", tracing::instrument(level = "info"))]
fn parse_cgroups_v2_path(content: &str) -> Result<PathBuf> {
    // the entry for cgroup v2 is always in the format like `0::$PATH`
    // where 0 is the hierarchy ID, the controller name is omitted in cgroup v2
    // and $PATH is the cgroup path
    // see https://docs.kernel.org/admin-guide/cgroup-v2.html
    let Some(path) = content.strip_prefix("0::") else {
        return Err(Error::Other(format!("invalid cgroup path: {}", content)));
    };

    let path = path.trim_start_matches('/');

    Ok(PathBuf::from(format!("/sys/fs/cgroup/{}", path)))
}

/// Whether a cgroup operation failed because the file is not there.
fn is_not_found(err: &cgroups_rs::fs::error::Error) -> bool {
    err.source()
        .and_then(<dyn StdError>::downcast_ref::<std::io::Error>)
        .is_some_and(|source| source.kind() == std::io::ErrorKind::NotFound)
}

/// Converts an OCI swap limit, which counts memory plus swap, into what cgroup
/// v2's `memory.swap.max` takes, which is swap alone, the way runc does. `None`
/// means the limit is unset and nothing should be written.
fn swap_for_v2(swap: i64, limit: Option<i64>) -> Result<Option<i64>> {
    match (swap, limit.unwrap_or(0)) {
        (0, -1) => Ok(Some(-1)),
        (0, _) => Ok(None),
        (-1, _) | (_, -1) => Ok(Some(swap)),
        (_, 0) => Err(other!("unable to set swap limit without memory limit")),
        (_, limit) if limit < 0 => Err(other!("invalid memory limit {limit}")),
        (_, limit) if swap < limit => Err(other!(
            "memory+swap limit {swap} should be >= memory limit {limit}"
        )),
        (_, limit) => Ok(Some(swap - limit)),
    }
}

/// Update process cgroup limits
#[cfg_attr(feature = "tracing", tracing::instrument(level = "info"))]
pub fn update_resources(cgroup: &Cgroup, resources: &LinuxResources) -> Result<()> {
    for sub_system in Cgroup::subsystems(cgroup) {
        match sub_system {
            Subsystem::Pid(pid_ctr) => {
                // set maximum number of PIDs
                if let Some(pids) = resources.pids() {
                    pid_ctr
                        .set_pid_max(MaxValue::Value(pids.limit()))
                        .map_err(other_error!("set pid max"))?;
                }
            }
            Subsystem::Mem(mem_ctr) => {
                if let Some(memory) = resources.memory() {
                    // As in runc, a zero limit means unset.
                    let limit = memory.limit().filter(|&limit| limit != 0);
                    let swap = if cgroup.v2() {
                        // An omitted swap limit is unset too, so that an unlimited memory
                        // limit lifts the swap limit along with it. A swap limit sent
                        // without a memory limit is converted against the one in place.
                        let swap = memory.swap().unwrap_or(0);
                        let limit = match limit {
                            None if swap > 0 => Some(mem_ctr.memory_stat().limit_in_bytes),
                            limit => limit,
                        };
                        swap_for_v2(swap, limit)?
                    } else {
                        memory.swap().filter(|&swap| swap != 0)
                    };
                    let set_limit = || -> Result<()> {
                        if let Some(limit) = limit {
                            mem_ctr
                                .set_limit(limit)
                                .map_err(other_error!("set mem limit"))?;
                        }
                        Ok(())
                    };
                    let set_swap = || -> Result<()> {
                        if let Some(swap) = swap {
                            let written = mem_ctr.set_memswap_limit(swap);
                            // A host without swap accounting has no memory.swap.max. Like
                            // runc, accept that when the request amounts to no swap at all
                            // or to no limit on it.
                            if cgroup.v2()
                                && matches!(swap, 0 | -1)
                                && written.as_ref().is_err_and(is_not_found)
                            {
                                return Ok(());
                            }
                            written.map_err(other_error!("set memsw limit"))?;
                        }
                        Ok(())
                    };
                    // The order matters. On cgroup v2 swap goes first, as in runc, so that
                    // a tighter memory limit can reclaim into it instead of killing. On v1
                    // the memsw limit must stay above the memory limit, so it goes first
                    // only when it is being raised along with the memory limit.
                    let swap_first = cgroup.v2()
                        || matches!((limit, swap), (Some(_), Some(swap))
                            if swap == -1 || mem_ctr.memory_stat().limit_in_bytes < swap);
                    if swap_first {
                        set_swap()?;
                        set_limit()?;
                    } else {
                        set_limit()?;
                        set_swap()?;
                    }
                }
            }
            Subsystem::CpuSet(cpuset_ctr) => {
                if let Some(cpu) = resources.cpu() {
                    // set CPUs to use within the cpuset
                    if let Some(cpus) = cpu.cpus() {
                        cpuset_ctr
                            .set_cpus(cpus)
                            .map_err(other_error!("set CPU sets"))?;
                    }

                    // set list of memory nodes in the cpuset
                    if let Some(mems) = cpu.mems() {
                        cpuset_ctr
                            .set_mems(mems)
                            .map_err(other_error!("set CPU memes"))?;
                    }
                }
            }
            Subsystem::Cpu(cpu_ctr) => {
                if let Some(cpu) = resources.cpu() {
                    // set CPU shares
                    if let Some(shares) = cpu.shares() {
                        cpu_ctr
                            .set_shares(shares)
                            .map_err(other_error!("set CPU share"))?;
                    }

                    // set CPU hardcap limit
                    if let Some(quota) = cpu.quota() {
                        cpu_ctr
                            .set_cfs_quota(quota)
                            .map_err(other_error!("set CPU quota"))?;
                    }

                    // set CPU hardcap period
                    if let Some(period) = cpu.period() {
                        cpu_ctr
                            .set_cfs_period(period)
                            .map_err(other_error!("set CPU period"))?;
                    }
                }
            }
            Subsystem::HugeTlb(ht_ctr) => {
                // set the limit if "pagesize" hugetlb usage
                if let Some(hp_limits) = resources.hugepage_limits() {
                    for limit in hp_limits {
                        ht_ctr
                            .set_limit_in_bytes(limit.page_size().as_str(), limit.limit() as u64)
                            .map_err(other_error!("set huge page limit"))?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{error::Error as _, path::PathBuf};

    use cgroups_rs::{
        fs::{error::ErrorKind, hierarchies, Cgroup},
        CgroupPid,
    };

    use super::{parse_cgroups_v2_path, swap_for_v2};
    use crate::cgroup::{
        add_task_to_cgroup, adjust_oom_score, read_process_oom_score, OOM_SCORE_ADJ_MAX,
    };

    // Same table as runc's ConvertMemorySwapToCgroupV2Value.
    #[test]
    fn swap_for_v2_takes_swap_alone() {
        const MB: i64 = 1 << 20;
        assert_eq!(swap_for_v2(0, Some(128 * MB)).unwrap(), None);
        assert_eq!(swap_for_v2(0, None).unwrap(), None);
        assert_eq!(swap_for_v2(0, Some(-1)).unwrap(), Some(-1));
        assert_eq!(swap_for_v2(-1, Some(128 * MB)).unwrap(), Some(-1));
        assert_eq!(swap_for_v2(-1, None).unwrap(), Some(-1));
        assert_eq!(swap_for_v2(-1, Some(-1)).unwrap(), Some(-1));
        assert_eq!(swap_for_v2(256 * MB, Some(-1)).unwrap(), Some(256 * MB));
        assert_eq!(
            swap_for_v2(256 * MB, Some(128 * MB)).unwrap(),
            Some(128 * MB)
        );
        assert_eq!(swap_for_v2(256 * MB, Some(256 * MB)).unwrap(), Some(0));
        assert!(swap_for_v2(256 * MB, None).is_err());
        assert!(swap_for_v2(128 * MB, Some(256 * MB)).is_err());
        assert!(swap_for_v2(128 * MB, Some(-2)).is_err());
    }

    fn is_cgroup_permission_error(err: &cgroups_rs::fs::error::Error) -> bool {
        *err.kind() == ErrorKind::FsError
            && err
                .source()
                .and_then(|source| source.downcast_ref::<std::io::Error>())
                .is_some_and(|source| source.kind() == std::io::ErrorKind::PermissionDenied)
    }

    // The subject is a child process rather than the test harness itself, so
    // that nothing the harness does meanwhile ends up in the cgroup and keeps
    // it from being deleted afterwards.
    #[test]
    fn test_add_cgroup() {
        let path = "runc_shim_test_cgroup";
        let h = hierarchies::auto();

        // create cgroup path first
        let cg = match Cgroup::new(h, path) {
            Ok(cg) => cg,
            Err(err) if is_cgroup_permission_error(&err) => {
                eprintln!("skipping test_add_cgroup: {err}");
                return;
            }
            Err(err) => panic!("failed to create cgroup for test: {err}"),
        };

        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let added = add_task_to_cgroup(path, child.id());
        let in_cgroup = cg.tasks().contains(&CgroupPid::from(child.id() as u64));
        child.kill().unwrap();
        child.wait().unwrap();
        match added {
            Ok(()) => assert!(in_cgroup),
            Err(err) if crate::error::is_permission_error(&err) => {
                let _ = cg.delete();
                eprintln!("skipping test_add_cgroup: {err}");
                return;
            }
            Err(err) => panic!("failed to add task to cgroup for test: {err}"),
        }

        // The reaped child has left the cgroup, which can take a moment to show.
        let mut attempts = 0;
        while let Err(err) = cg.delete() {
            assert!(attempts < 50, "failed to delete the test cgroup: {err}");
            attempts += 1;
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn test_adjust_oom_score() {
        let pid = std::process::id();
        let score = read_process_oom_score(pid).unwrap();

        adjust_oom_score(pid).unwrap();
        let new = read_process_oom_score(pid).unwrap();
        if score < OOM_SCORE_ADJ_MAX {
            assert_eq!(new, score + 1)
        } else {
            assert_eq!(new, OOM_SCORE_ADJ_MAX)
        }
    }

    #[test]
    fn test_parse_cgroups_v2_path() {
        let path = "0::/user.slice/user-1000.slice/session-2.scope";
        assert_eq!(
            parse_cgroups_v2_path(path).unwrap(),
            PathBuf::from("/sys/fs/cgroup/user.slice/user-1000.slice/session-2.scope")
        );
    }

    #[test]
    fn test_parse_cgroups_v2_path_empty() {
        let path = "0::";
        assert_eq!(
            parse_cgroups_v2_path(path).unwrap(),
            PathBuf::from("/sys/fs/cgroup/")
        );
    }

    #[test]
    fn test_parse_cgroups_v2_path_kube() {
        let path = "0::/kubepods-besteffort-pod8.slice:cri-containerd:8";
        assert_eq!(
            parse_cgroups_v2_path(path).unwrap(),
            PathBuf::from("/sys/fs/cgroup/kubepods-besteffort-pod8.slice:cri-containerd:8")
        );
    }
}

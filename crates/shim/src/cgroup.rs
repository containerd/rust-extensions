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
    error::Error as StdError,
    fs,
    path::{Path, PathBuf},
};

use cgroups_rs::{
    fs::{
        cgroup::get_cgroups_relative_paths_by_pid, error::Result as CgResult, hierarchies,
        pid::PidController, Cgroup, Controller, MaxValue, Subsystem,
    },
    CgroupPid,
};
use containerd_shim_protos::{
    cgroups::metrics::{
        CPUStat, CPUUsage, MemoryEntry, MemoryOomControl, MemoryStat, Metrics, PidsStat, Throttle,
    },
    cgroups_v2::metrics::{
        CPUStat as CPUStatV2, MemoryEvents, MemoryStat as MemoryStatV2, Metrics as MetricsV2,
        PidsStat as PidsStatV2,
    },
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
                let mut mem_stat = MemoryStat::new();
                mem_stat.set_usage(memory_entry(mem.usage_in_bytes));
                mem_stat.set_total_inactive_file(mem.stat.total_inactive_file);
                // The swap entry is v1's memsw counter: memory and swap together.
                mem_stat.set_kernel(memory_entry(mem_ctr.kmem_stat().usage_in_bytes));
                mem_stat.set_swap(memory_entry(mem_ctr.memswap().usage_in_bytes));
                metrics.set_memory(mem_stat);
                let mut oom_control = MemoryOomControl::new();
                oom_control.set_oom_kill(mem.oom_control.oom_kill);
                metrics.set_memory_oom_control(oom_control);
            }
            Subsystem::Pid(pid_ctr) => {
                let (current, limit) = pids(pid_ctr)?;
                let mut pid_stats = PidsStat::new();
                pid_stats.set_current(current);
                pid_stats.set_limit(limit);
                metrics.set_pids(pid_stats)
            }
            _ => {}
        }
    }
    Ok(metrics)
}

/// A proto setter for one cgroup file key.
type Setter<T> = fn(&mut T, u64);

/// Collects a cgroup v2 cgroup's metrics as containerd's v2 metrics type,
/// which containerd reads with v2 meanings: swap apart from memory, time in
/// microseconds.
///
/// TODO(cgroups-rs): the parts read by hand here are gaps in cgroups-rs
/// 0.5.1, to fill upstream so that this stays a mapping onto its stats.
/// The contribution backlog:
/// - v2 memory event counters: `memory_stat()` leaves `oom_control` at its
///   defaults and nothing exposes `memory.events`. Add a typed API for low,
///   high, max, oom, oom_kill and oom_group_kill.
/// - unlimited v2 swap limit: `memswap()` parses `memory.swap.max` as an
///   integer, so `max` becomes 0. Parse it as `MaxValue`, keeping unlimited
///   apart from zero.
/// - typed CPU statistics: `cpu().stat` is raw text, leaving parsing and
///   units to callers. Expose typed v1 and v2 CPU stats with explicit units,
///   throttling and burst counters included.
/// - fallible stats collection: several getters replace read and parse
///   errors with zeros or defaults. Add fallible variants that tell an
///   unavailable counter from a permission or parse failure, keeping the
///   existing APIs.
/// - typed v2 memory statistics, lower priority: the typed memory structure
///   is v1-shaped and v2 fields live in `stat.raw`, while `memory_stat()`
///   stores `memory.swap.current` in `swappiness`. Add a v2 structure for
///   anon, file, kernel_stack, slab and the rest.
pub fn collect_metrics_v2(cgroup: &Cgroup) -> Result<MetricsV2> {
    let mut metrics = MetricsV2::new();
    for sub_system in Cgroup::subsystems(cgroup) {
        match sub_system {
            Subsystem::Cpu(cpu_ctr) => {
                let cpu_stat = cpu_ctr.cpu();
                let stat = flat_keyed(&cpu_stat.stat);
                let mut cpu = CPUStatV2::new();
                let fields: [(&str, Setter<CPUStatV2>); 8] = [
                    ("usage_usec", CPUStatV2::set_usage_usec),
                    ("user_usec", CPUStatV2::set_user_usec),
                    ("system_usec", CPUStatV2::set_system_usec),
                    ("nr_periods", CPUStatV2::set_nr_periods),
                    ("nr_throttled", CPUStatV2::set_nr_throttled),
                    ("throttled_usec", CPUStatV2::set_throttled_usec),
                    ("nr_bursts", CPUStatV2::set_nr_bursts),
                    ("burst_usec", CPUStatV2::set_burst_usec),
                ];
                for (key, set) in fields {
                    set(&mut cpu, stat.get(key).copied().unwrap_or(0));
                }
                metrics.set_cpu(cpu);
            }
            Subsystem::Mem(mem_ctr) => {
                let mem = mem_ctr.memory_stat();
                let swap = mem_ctr.memswap();
                let stat = &mem.stat.raw;
                let mut memory = MemoryStatV2::new();
                let fields: [(&str, Setter<MemoryStatV2>); 31] = [
                    ("anon", MemoryStatV2::set_anon),
                    ("file", MemoryStatV2::set_file),
                    ("kernel_stack", MemoryStatV2::set_kernel_stack),
                    ("slab", MemoryStatV2::set_slab),
                    ("sock", MemoryStatV2::set_sock),
                    ("shmem", MemoryStatV2::set_shmem),
                    ("file_mapped", MemoryStatV2::set_file_mapped),
                    ("file_dirty", MemoryStatV2::set_file_dirty),
                    ("file_writeback", MemoryStatV2::set_file_writeback),
                    ("anon_thp", MemoryStatV2::set_anon_thp),
                    ("inactive_anon", MemoryStatV2::set_inactive_anon),
                    ("active_anon", MemoryStatV2::set_active_anon),
                    ("inactive_file", MemoryStatV2::set_inactive_file),
                    ("active_file", MemoryStatV2::set_active_file),
                    ("unevictable", MemoryStatV2::set_unevictable),
                    ("slab_reclaimable", MemoryStatV2::set_slab_reclaimable),
                    ("slab_unreclaimable", MemoryStatV2::set_slab_unreclaimable),
                    ("pgfault", MemoryStatV2::set_pgfault),
                    ("pgmajfault", MemoryStatV2::set_pgmajfault),
                    ("workingset_refault", MemoryStatV2::set_workingset_refault),
                    ("workingset_activate", MemoryStatV2::set_workingset_activate),
                    (
                        "workingset_nodereclaim",
                        MemoryStatV2::set_workingset_nodereclaim,
                    ),
                    ("pgrefill", MemoryStatV2::set_pgrefill),
                    ("pgscan", MemoryStatV2::set_pgscan),
                    ("pgsteal", MemoryStatV2::set_pgsteal),
                    ("pgactivate", MemoryStatV2::set_pgactivate),
                    ("pgdeactivate", MemoryStatV2::set_pgdeactivate),
                    ("pglazyfree", MemoryStatV2::set_pglazyfree),
                    ("pglazyfreed", MemoryStatV2::set_pglazyfreed),
                    ("thp_fault_alloc", MemoryStatV2::set_thp_fault_alloc),
                    ("thp_collapse_alloc", MemoryStatV2::set_thp_collapse_alloc),
                ];
                for (key, set) in fields {
                    set(&mut memory, stat.get(key).copied().unwrap_or(0));
                }
                memory.set_usage(mem.usage_in_bytes);
                memory.set_usage_limit(limit(mem.limit_in_bytes));
                memory.set_max_usage(mem.max_usage_in_bytes);
                memory.set_swap_usage(swap.usage_in_bytes);
                // TODO(cgroups-rs): memswap() reads an unlimited memory.swap.max
                // as 0, so until it parses `max` the file is read here.
                let swap_limit = match fs::read_to_string(mem_ctr.path().join("memory.swap.max")) {
                    Ok(value) if value.trim() == "max" => u64::MAX,
                    Ok(value) => value.trim().parse().unwrap_or(0),
                    Err(_) => 0,
                };
                memory.set_swap_limit(swap_limit);
                memory.set_swap_max_usage(swap.max_usage_in_bytes);
                metrics.set_memory(memory);

                let events = match fs::read_to_string(mem_ctr.path().join("memory.events")) {
                    Ok(events) => events,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                    Err(e) => return Err(e).map_err(io_error!(e, "read memory.events")),
                };
                let events = flat_keyed(&events);
                let mut memory_events = MemoryEvents::new();
                let fields: [(&str, Setter<MemoryEvents>); 6] = [
                    ("low", MemoryEvents::set_low),
                    ("high", MemoryEvents::set_high),
                    ("max", MemoryEvents::set_max),
                    ("oom", MemoryEvents::set_oom),
                    ("oom_kill", MemoryEvents::set_oom_kill),
                    ("oom_group_kill", MemoryEvents::set_oom_group_kill),
                ];
                for (key, set) in fields {
                    set(&mut memory_events, events.get(key).copied().unwrap_or(0));
                }
                metrics.set_memory_events(memory_events);
            }
            Subsystem::Pid(pid_ctr) => {
                let (current, limit) = pids(pid_ctr)?;
                let mut pid_stats = PidsStatV2::new();
                pid_stats.set_current(current);
                pid_stats.set_limit(limit);
                metrics.set_pids(pid_stats)
            }
            _ => {}
        }
    }
    Ok(metrics)
}

fn memory_entry(usage: u64) -> MemoryEntry {
    let mut entry = MemoryEntry::new();
    entry.set_usage(usage);
    entry
}

/// The `key value` lines of a flat-keyed cgroup file.
fn flat_keyed(content: &str) -> HashMap<&str, u64> {
    content
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(' ')?;
            Some((key, value.trim().parse().ok()?))
        })
        .collect()
}

/// A limit as cgroups-rs reports it, where a negative value means none.
fn limit(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// The current and maximum pid counts, 0 for either when the cgroup has gone.
fn pids(pid_ctr: &PidController) -> Result<(u64, u64)> {
    let ignore_not_found =
        |cr: CgResult<u64>| cr.or_else(|e| if is_not_found(&e) { Ok(0) } else { Err(e) });
    let current =
        ignore_not_found(pid_ctr.get_pid_current()).map_err(other_error!("get current pid"))?;
    let limit = ignore_not_found(pid_ctr.get_pid_max().map(|val| match val {
        // See https://github.com/opencontainers/runc/blob/dbe8434359ca35af1c1e10df42b1f4391c1e1010/libcontainer/cgroups/fs/pids.go#L55
        MaxValue::Max => 0,
        MaxValue::Value(val) => val as u64,
    }))
    .map_err(other_error!("get pid limit"))?;
    Ok((current, limit))
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

    use super::{collect_metrics_v2, parse_cgroups_v2_path, swap_for_v2};
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

    /// On cgroup v2 the metrics come as containerd's v2 type, read from the
    /// cgroup's own files. Needs root and a v2 host, like test_add_cgroup.
    #[test]
    fn collect_metrics_v2_reads_the_cgroup() {
        let h = hierarchies::auto();
        if !h.v2() {
            eprintln!("skipping collect_metrics_v2_reads_the_cgroup: not cgroup v2");
            return;
        }
        let path = "runc_shim_test_metrics";
        let cg = match Cgroup::new(h, path) {
            Ok(cg) => cg,
            Err(err) if is_cgroup_permission_error(&err) => {
                eprintln!("skipping collect_metrics_v2_reads_the_cgroup: {err}");
                return;
            }
            Err(err) => panic!("failed to create cgroup for test: {err}"),
        };

        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let added = add_task_to_cgroup(path, child.id());
        let metrics = collect_metrics_v2(&cg);
        child.kill().unwrap();
        child.wait().unwrap();
        match added {
            Ok(()) => {}
            Err(err) if crate::error::is_permission_error(&err) => {
                let _ = cg.delete();
                eprintln!("skipping collect_metrics_v2_reads_the_cgroup: {err}");
                return;
            }
            Err(err) => panic!("failed to add task to cgroup for test: {err}"),
        }
        let metrics = metrics.unwrap();
        assert_eq!(metrics.pids().current(), 1, "{metrics:?}");
        // A fresh cgroup has no limit; its usage stays charged to the cgroup
        // the child was born in, so it is not something to assert on.
        assert_eq!(metrics.memory().usage_limit(), u64::MAX, "{metrics:?}");
        let any = crate::util::convert_to_any(Box::new(metrics)).unwrap();
        assert!(
            any.type_url.ends_with("cgroups.v2.Metrics"),
            "{}",
            any.type_url
        );

        let mut attempts = 0;
        while let Err(err) = cg.delete() {
            assert!(attempts < 50, "failed to delete the test cgroup: {err}");
            attempts += 1;
            std::thread::sleep(std::time::Duration::from_millis(10));
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

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

use std::fs;
use std::io::Read;
use std::path::Path;

use cgroups_rs::cgroup::get_cgroups_relative_paths_by_pid;
use cgroups_rs::{hierarchies, Cgroup, CgroupPid, MaxValue, Subsystem};
use oci_spec::runtime::LinuxResources;

use containerd_shim_protos::cgroups::metrics::{
    CPUStat, CPUUsage, MemoryEntry, MemoryStat, Metrics,
};
use containerd_shim_protos::protobuf::well_known_types::Any;
use containerd_shim_protos::protobuf::Message;
use containerd_shim_protos::shim::oci::Options;

use crate::error::{Error, Result};

// OOM_SCORE_ADJ_MAX is from https://github.com/torvalds/linux/blob/master/include/uapi/linux/oom.h#L10
const OOM_SCORE_ADJ_MAX: i64 = 1000;

pub fn set_cgroup_and_oom_score(pid: u32) -> Result<()> {
    if pid == 0 {
        return Ok(());
    }

    // set cgroup
    let mut data: Vec<u8> = Vec::new();
    std::io::stdin()
        .read_to_end(&mut data)
        .map_err(io_error!(e, "read stdin"))?;

    if !data.is_empty() {
        let opts = Any::parse_from_bytes(&data)
            .and_then(|any| Options::parse_from_bytes(any.get_value()))?;

        if !opts.shim_cgroup.is_empty() {
            add_task_to_cgroup(opts.shim_cgroup.as_str(), pid)?;
        }
    }

    // set oom score
    adjust_oom_score(pid)
}

/// Add a process to the given relative cgroup path
pub fn add_task_to_cgroup(path: &str, pid: u32) -> Result<()> {
    let h = hierarchies::auto();
    // use relative path here, need to trim prefix '/'
    let path = path.trim_start_matches('/');

    Cgroup::load(h, path)
        .add_task(CgroupPid::from(pid as u64))
        .map_err(other_error!(e, "add task to cgroup"))
}

/// Sets the OOM score for the process to the parents OOM score + 1
/// to ensure that they parent has a lower score than the shim
pub fn adjust_oom_score(pid: u32) -> Result<()> {
    let score = read_process_oom_score(std::os::unix::process::parent_id())?;
    if score < OOM_SCORE_ADJ_MAX {
        write_process_oom_score(pid, score + 1)?;
    }
    Ok(())
}

fn read_process_oom_score(pid: u32) -> Result<i64> {
    let content = fs::read_to_string(format!("/proc/{}/oom_score_adj", pid))
        .map_err(io_error!(e, "read oom score"))?;
    let score = content
        .trim()
        .parse::<i64>()
        .map_err(other_error!(e, "parse oom score"))?;
    Ok(score)
}

fn write_process_oom_score(pid: u32, score: i64) -> Result<()> {
    fs::write(format!("/proc/{}/oom_score_adj", pid), score.to_string())
        .map_err(io_error!(e, "write oom score"))
}

/// Collect process cgroup stats, return only necessary parts of it
pub fn collect_metrics(pid: u32) -> Result<Metrics> {
    let mut metrics = Metrics::new();
    // get container main process cgroup
    let path =
        get_cgroups_relative_paths_by_pid(pid).map_err(other_error!(e, "get process cgroup"))?;
    let cgroup = Cgroup::load_with_relative_paths(hierarchies::auto(), Path::new("."), path);

    // to make it easy, fill the necessary metrics only.
    for sub_system in Cgroup::subsystems(&cgroup) {
        match sub_system {
            Subsystem::CpuAcct(cpuacct_ctr) => {
                let mut cpu_usage = CPUUsage::new();
                cpu_usage.set_total(cpuacct_ctr.cpuacct().usage);
                let mut cpu_stat = CPUStat::new();
                cpu_stat.set_usage(cpu_usage);
                metrics.set_cpu(cpu_stat);
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
            _ => {}
        }
    }
    Ok(metrics)
}

/// Update process cgroup limits
pub fn update_resources(pid: u32, resources: &LinuxResources) -> Result<()> {
    // get container main process cgroup
    let path =
        get_cgroups_relative_paths_by_pid(pid).map_err(other_error!(e, "get process cgroup"))?;
    let cgroup = Cgroup::load_with_relative_paths(hierarchies::auto(), Path::new("."), path);

    for sub_system in Cgroup::subsystems(&cgroup) {
        match sub_system {
            Subsystem::Pid(pid_ctr) => {
                // set maximum number of PIDs
                if let Some(pids) = resources.pids() {
                    pid_ctr
                        .set_pid_max(MaxValue::Value(pids.limit()))
                        .map_err(other_error!(e, "set pid max"))?;
                }
            }
            Subsystem::Mem(mem_ctr) => {
                if let Some(memory) = resources.memory() {
                    // set memory limit in bytes
                    if let Some(limit) = memory.limit() {
                        mem_ctr
                            .set_limit(limit)
                            .map_err(other_error!(e, "set mem limit"))?;
                    }

                    // set memory swap limit in bytes
                    if let Some(swap) = memory.swap() {
                        mem_ctr
                            .set_memswap_limit(swap)
                            .map_err(other_error!(e, "set memsw limit"))?;
                    }
                }
            }
            Subsystem::CpuSet(cpuset_ctr) => {
                if let Some(cpu) = resources.cpu() {
                    // set CPUs to use within the cpuset
                    if let Some(cpus) = cpu.cpus() {
                        cpuset_ctr
                            .set_cpus(cpus)
                            .map_err(other_error!(e, "set CPU sets"))?;
                    }

                    // set list of memory nodes in the cpuset
                    if let Some(mems) = cpu.mems() {
                        cpuset_ctr
                            .set_mems(mems)
                            .map_err(other_error!(e, "set CPU memes"))?;
                    }
                }
            }
            Subsystem::Cpu(cpu_ctr) => {
                if let Some(cpu) = resources.cpu() {
                    // set CPU shares
                    if let Some(shares) = cpu.shares() {
                        cpu_ctr
                            .set_shares(shares)
                            .map_err(other_error!(e, "set CPU share"))?;
                    }

                    // set CPU hardcap limit
                    if let Some(quota) = cpu.quota() {
                        cpu_ctr
                            .set_cfs_quota(quota)
                            .map_err(other_error!(e, "set CPU quota"))?;
                    }

                    // set CPU hardcap period
                    if let Some(period) = cpu.period() {
                        cpu_ctr
                            .set_cfs_period(period)
                            .map_err(other_error!(e, "set CPU period"))?;
                    }
                }
            }
            Subsystem::HugeTlb(ht_ctr) => {
                // set the limit if "pagesize" hugetlb usage
                if let Some(hp_limits) = resources.hugepage_limits() {
                    for limit in hp_limits {
                        ht_ctr
                            .set_limit_in_bytes(limit.page_size().as_str(), limit.limit() as u64)
                            .map_err(other_error!(e, "set huge page limit"))?;
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
    use cgroups_rs::{hierarchies, Cgroup, CgroupPid};

    use crate::cgroup::{
        add_task_to_cgroup, adjust_oom_score, read_process_oom_score, OOM_SCORE_ADJ_MAX,
    };

    #[test]
    fn test_add_cgroup() {
        let path = "runc_shim_test_cgroup";
        let h = hierarchies::auto();

        // create cgroup path first
        let cg = Cgroup::new(h, path);

        let pid = std::process::id();
        add_task_to_cgroup(path, pid).unwrap();
        let cg_id = CgroupPid::from(pid as u64);
        assert!(cg.tasks().contains(&cg_id));

        // remove cgroup as possible
        cg.remove_task(cg_id);
        cg.delete().unwrap()
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
}

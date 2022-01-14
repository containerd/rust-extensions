/*
   copyright the containerd authors.

   licensed under the apache license, version 2.0 (the "license");
   you may not use this file except in compliance with the license.
   you may obtain a copy of the license at

       http://www.apache.org/licenses/license-2.0

   unless required by applicable law or agreed to in writing, software
   distributed under the license is distributed on an "as is" basis,
   without warranties or conditions of any kind, either express or implied.
   see the license for the specific language governing permissions and
   limitations under the license.
*/

// Forked from https://github.com/pwFoo/rust-runc/blob/master/src/specs.rs
/*
 * Copyright 2020 fsyncd, Berlin, Germany.
 * Additional material, copyright of the containerd authors.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// The dimensions of a rectangle, used for console sizing
/// This name is different from Go Version, avoiding confilict with [`std::boxed::Box`]
#[derive(Debug, Serialize, Deserialize)]
pub struct BoxSize {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<usize>,
}

/// User and group information for the container's process
#[derive(Debug, Serialize, Deserialize)]
pub struct User {
    /// User id
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
    /// Group id
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gid: Option<u32>,
    /// Additional group ids for the container's process
    #[serde(rename = "additionalGids")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub additional_gids: Option<Vec<u32>>,
    /// Optional username
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

/// Whitelist of capabilities that are kept for a process
#[derive(Debug, Serialize, Deserialize)]
pub struct LinuxCapabilities {
    /// Bounding capabilities
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounding: Option<Vec<String>>,
    /// Effective capabilities
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective: Option<Vec<String>>,
    /// Capabilities preserved across execve
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inheritable: Option<Vec<String>>,
    /// Limiting superset for effective capabilities
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permitted: Option<Vec<String>>,
    /// Ambient capabilities that are kept
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ambient: Option<Vec<String>>,
}

/// Rlimit type and restrictions
#[derive(Debug, Serialize, Deserialize)]
pub struct POSIXRlimit {
    /// Type of limit to set
    #[serde(rename = "type")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit_type: Option<String>,
    /// Hard limit
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hard: Option<u64>,
    /// Soft limit
    #[serde(skip_serializing_if = "Option::is_none")]
    pub soft: Option<u64>,
}

/// Information required to start a specific process inside the container
#[derive(Debug, Serialize, Deserialize)]
pub struct Process {
    /// Interactive terminal
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal: Option<bool>,
    /// Size of the console
    #[serde(rename = "consoleSize")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub console_size: Option<BoxSize>,
    /// User information
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<User>,
    /// Binary and arguments for the process to execute
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    /// Command line for the process to execute on Windows
    #[serde(rename = "commandLine")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_line: Option<String>,
    /// Environment for the process
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<Vec<String>>,
    /// Current working directory, relative to the container root
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Capabilities that are kept for the process
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<LinuxCapabilities>,
    /// Rlimit options to apply to the process
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rlimits: Option<Vec<POSIXRlimit>>,
    /// Disable the process from gaining additional privileges
    #[serde(rename = "noNewPrivileges")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_new_privileges: Option<bool>,
    /// Apparmor profile for the container
    #[serde(rename = "apparmorProfile")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_armor_profile: Option<String>,
    /// Out of memory score for the container
    #[serde(rename = "oomScoreAdj")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oom_score_adj: Option<usize>,
    /// Selinux context that the container is run as
    #[serde(rename = "selinuxLabel")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selinux_label: Option<String>,
}

/// Device rule for the whitelist controller
#[derive(Debug, Serialize, Deserialize)]
pub struct LinuxDeviceCgroup {
    /// Allow or deny
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow: Option<bool>,
    /// Device type, block, char, etc
    #[serde(rename = "type")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_type: Option<String>,
    /// Device major number
    #[serde(skip_serializing_if = "Option::is_none")]
    pub major: Option<i64>,
    /// Device minor number
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minor: Option<i64>,
    /// Cgroup access permissions format, rwm
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access: Option<String>,
}

/// Linux cgroup 'memory' resource management
#[derive(Debug, Serialize, Deserialize)]
pub struct LinuxMemory {
    /// Memory limit (in bytes)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    /// Memory reservation or soft_limit (in bytes)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reservation: Option<i64>,
    /// Total memory limit (memory + swap)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub swap: Option<i64>,
    /// Kernel memory limit (in bytes)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernel: Option<i64>,
    /// Kernel memory limit for tcp (in bytes)
    #[serde(rename = "kernelTCP")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernel_tcp: Option<i64>,
    /// How aggressive the kernel will swap memory pages
    #[serde(skip_serializing_if = "Option::is_none")]
    pub swappiness: Option<u64>,
    /// Disable the OOM killer for out of memory conditions
    #[serde(rename = "disableOOMKiller")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disable_oom_killer: Option<bool>,
}

/// Linux cgroup 'cpu' resource management
#[derive(Debug, Serialize, Deserialize)]
pub struct LinuxCPU {
    /// CPU shares (relative weight (ratio) vs. other cgroups with cpu shares)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shares: Option<u64>,
    /// CPU hardcap limit (in usecs), allowed cpu time in a given period
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quota: Option<i64>,
    /// CPU period to be used for hardcapping (in usecs)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub period: Option<u64>,
    /// How much time realtime scheduling may use (in usecs)
    #[serde(rename = "realtimeRuntime")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub realtime_runtime: Option<i64>,
    /// CPU period to be used for realtime scheduling (in usecs)
    #[serde(rename = "realtimePeriod")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub realtime_period: Option<u64>,
    /// CPUs to use within the cpuset, default is to use any CPU available
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpus: Option<String>,
    /// List of memory nodes in the cpuset, default is to use any available memory node
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mems: Option<String>,
}

/// Linux cgroup 'pids' resource management (Linux 4.3)
#[derive(Debug, Serialize, Deserialize)]
pub struct LinuxPids {
    /// Maximum number of PIDs
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
}

/// `major:minor rate_per_second` pair for throttleDevice
#[derive(Debug, Serialize, Deserialize)]
pub struct LinuxThrottleDevice {
    /// Device major number
    #[serde(skip_serializing_if = "Option::is_none")]
    pub major: Option<i64>,
    /// Device minor number
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minor: Option<i64>,
    /// IO rate limit per cgroup per device
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate: Option<u64>,
}

/// `major:minor weight` pair for weightDevice
#[derive(Debug, Serialize, Deserialize)]
pub struct LinuxWeightDevice {
    /// Device major number
    #[serde(skip_serializing_if = "Option::is_none")]
    pub major: Option<i64>,
    /// Device minor number
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minor: Option<i64>,
    /// Bandwidth rate for the device
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight: Option<u16>,
    /// Bandwidth rate for the device while competing with the cgroup's child cgroups, CFQ scheduler only
    #[serde(rename = "leafWeight")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leaf_weight: Option<u16>,
}

/// Linux cgroup 'blkio' resource management
#[derive(Debug, Serialize, Deserialize)]
pub struct LinuxBlockIO {
    /// Per cgroup weight
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight: Option<u16>,
    /// Tasks weight in the given cgroup while competing with the cgroup's child cgroups, CFQ scheduler only
    #[serde(rename = "leafWeight")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leaf_weight: Option<u16>,
    /// Weight per cgroup per device, can override BlkioWeight
    #[serde(rename = "weightDevice")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight_device: Option<Vec<LinuxWeightDevice>>,
    /// IO read rate limit per cgroup per device, bytes per second
    #[serde(rename = "throttleReadBpsDevice")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub throttle_read_bps_device: Option<Vec<LinuxThrottleDevice>>,
    /// IO write rate limit per cgroup per device, bytes per second
    #[serde(rename = "throttleWriteBpsDevice")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub throttle_write_bps_device: Option<Vec<LinuxThrottleDevice>>,
    /// IO read rate limit per cgroup per device, IO per second
    #[serde(rename = "throttleReadIOPSDevice")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub throttle_read_iops_device: Option<Vec<LinuxThrottleDevice>>,
    /// IO write rate limit per cgroup per device, IO per second
    #[serde(rename = "throttleWriteIOPSDevice")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub throttle_write_iops_device: Option<Vec<LinuxThrottleDevice>>,
}

/// Kernel hugepage limits
#[derive(Debug, Serialize, Deserialize)]
pub struct LinuxHugepageLimit {
    /// Hugepage size, format: "<size><unit-prefix>B' (e.g. 64KB, 2MB, 1GB, etc.)
    #[serde(rename = "pageSize")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_size: Option<String>,
    /// Limit of "hugepagesize" hugetlb usage
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
}

/// Interface priority for network interfaces
#[derive(Debug, Serialize, Deserialize)]
pub struct LinuxInterfacePriority {
    /// Name of the network interface
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Priority for the interface
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<u32>,
}

/// Network identification and priority configuration
#[derive(Debug, Serialize, Deserialize)]
pub struct LinuxNetwork {
    /// Class identifier for container's network packets
    #[serde(rename = "classID")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub class_id: Option<u32>,
    /// Priority of network traffic for container
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priorities: Option<Vec<LinuxInterfacePriority>>,
}

/// Linux cgroup 'rdma' resource management (Linux 4.11)
#[derive(Debug, Serialize, Deserialize)]
pub struct LinuxRdma {
    /// Maximum number of HCA handles that can be opened
    #[serde(rename = "hcaHandles")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hca_handles: Option<u32>,
    /// Maximum number of HCA objects that can be created
    #[serde(rename = "hcaObjects")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hca_objects: Option<u32>,
}

/// Container runtime resource constraints
#[derive(Debug, Serialize, Deserialize)]
pub struct LinuxResources {
    /// Device whitelist
    #[serde(skip_serializing_if = "Option::is_none")]
    pub devices: Option<Vec<LinuxDeviceCgroup>>,
    /// Memory restriction configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<LinuxMemory>,
    /// CPU resource restriction configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu: Option<LinuxCPU>,
    /// Task resource restriction configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pids: Option<LinuxPids>,
    /// BlockIO restriction configuration
    #[serde(rename = "blockIO")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_io: Option<LinuxBlockIO>,
    /// Hugetlb limit
    #[serde(rename = "hugepageLimits")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hugepage_limits: Option<Vec<LinuxHugepageLimit>>,
    /// Network restriction configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<LinuxNetwork>,
    /// Rdma resource restriction configuration.
    /// Limits are a set of key value pairs that define RDMA resource limits,
    /// where the key is device name and value is resource limits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rdma: Option<HashMap<String, LinuxRdma>>,
}

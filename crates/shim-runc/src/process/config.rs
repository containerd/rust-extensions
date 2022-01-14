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

// NOTE: Go reference
// https://github.com/containerd/containerd/blob/main/pkg/process/types.go
// https://github.com/containerd/containerd/blob/main/pkg/stdio/stdio.go


use containerd_shim_protos as protos;

use protos::shim::mount::Mount;

use protobuf::well_known_types::Any;

#[derive(Debug, Clone, Default)]
pub struct MountConfig {
    pub mount_type: String,
    pub source: String,
    pub target: String,
    pub options: Vec<String>,
}

impl MountConfig {
    pub fn from_proto_mount(mnt: Mount) -> Self {
        Self {
            mount_type: mnt.field_type,
            source: mnt.source,
            target: mnt.target,
            options: mnt.options.to_vec(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CreateConfig {
    pub id: String,
    pub bundle: String,
    pub runtime: String,
    pub rootfs: Vec<MountConfig>,
    pub terminal: bool,
    pub stdin: String,
    pub stdout: String,
    pub stderr: String,
    // FIXME: checkout is not supported now
    // checkpoint: String,
    // parent_checkpoint: String,
    pub options: Option<Any>,
}

#[derive(Debug, Clone, Default)]
pub struct ExecConfig {
    id: String,
    bundle: String,
    rootfs: Vec<MountConfig>,
    terminal: bool,
    stdin: String,
    stdout: String,
    // FIXME: checkout is not supported now
    // checkpoint: String,
    // parent_checkpoint: String,
    options: Option<Any>,
}

// FIXME: checkpoint is not supported now
// pub struct ChecoutConfig {}

#[derive(Debug, Clone, Default)]
pub struct StdioConfig {
    pub stdin: String,
    pub stdout: String,
    pub stderr: String,
    pub terminal: bool,
}

impl StdioConfig {
    pub fn is_null(&self) -> bool {
        self.stdin == "" && self.stdout == "" && self.stderr == ""
    }
}

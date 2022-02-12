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

// NOTE: Go references
// https://github.com/containerd/containerd/blob/main/pkg/process/exec_state.go
// https://github.com/containerd/containerd/blob/main/pkg/process/init_state.go

#[derive(Debug, Clone, Copy)]
pub enum ProcessState {
    Unknown,
    Created,
    // CreatedCheckpoint,
    Running,
    Paused,
    Pausing,
    Stopped,
    Deleted,
}

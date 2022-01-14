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

// NOTE: Go references
// https://github.com/containerd/containerd/blob/main/pkg/process/exec_state.go
// https://github.com/containerd/containerd/blob/main/pkg/process/init_state.go

#![allow(unused)]

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

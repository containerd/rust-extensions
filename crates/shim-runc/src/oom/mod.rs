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

pub mod v1;
pub mod v2;

use std::io;
use std::sync::{Arc, Mutex};

use containerd_shim as shim;

use shim::RemotePublisher;

use cgroups_rs as cgroup;

use cgroup::Hierarchy;

pub trait Watcher: Send + Sync {
    fn run(&mut self, publisher: Arc<Mutex<RemotePublisher>>) -> io::Result<()>;
    fn add(&self, id: String, namespace: String, cg: Arc<dyn Hierarchy>) -> io::Result<()>;
}

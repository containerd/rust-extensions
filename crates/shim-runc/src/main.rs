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

/// Generated option structures.
#[rustfmt::skip]
pub mod options;

pub use containerd_shim as shim;
pub use containerd_shim_protos as protos;
pub use containerd_runc_rust as runc;

pub mod v2 {
    pub use crate::service::Service;
    pub use crate::options::oci::*;
}

mod container;
mod process;
mod service;
mod utils;

use crate::service::Service;

fn main() {
    // all arguments will be parsed inside "run" function.
    shim::run::<Service>("io.containerd.runc.v2");
}

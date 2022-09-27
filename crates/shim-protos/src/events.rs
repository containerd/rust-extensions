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

pub mod container {
    include!(concat!(env!("OUT_DIR"), "/events/container.rs"));
}

pub mod content {
    include!(concat!(env!("OUT_DIR"), "/events/content.rs"));
}

pub mod image {
    include!(concat!(env!("OUT_DIR"), "/events/image.rs"));
}

pub mod namespace {
    include!(concat!(env!("OUT_DIR"), "/events/namespace.rs"));
}

pub mod snapshot {
    include!(concat!(env!("OUT_DIR"), "/events/snapshot.rs"));
}

pub mod task {
    include!(concat!(env!("OUT_DIR"), "/events/task.rs"));
}

mod mount {
    pub use crate::types::mount::*;
}

mod gogo {
    pub use crate::types::gogo::*;
}

mod fieldpath {
    pub use crate::types::fieldpath::*;
}

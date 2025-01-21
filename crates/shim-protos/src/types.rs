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

pub mod empty {
    include!(concat!(env!("OUT_DIR"), "/types/empty.rs"));
}

pub mod gogo {
    include!(concat!(env!("OUT_DIR"), "/types/gogo.rs"));
}

pub mod mount {
    include!(concat!(env!("OUT_DIR"), "/types/mount.rs"));
}

pub mod task {
    include!(concat!(env!("OUT_DIR"), "/types/task.rs"));
}

pub mod fieldpath {
    include!(concat!(env!("OUT_DIR"), "/types/fieldpath.rs"));
}

pub mod introspection {
    include!(concat!(env!("OUT_DIR"), "/types/introspection.rs"));
}
#[cfg(feature = "sandbox")]
pub mod platform {
    include!(concat!(env!("OUT_DIR"), "/types/platform.rs"));
}

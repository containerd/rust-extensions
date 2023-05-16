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

pub mod sandbox {
    include!(concat!(env!("OUT_DIR"), "/sandbox/sandbox.rs"));
}

pub mod sandbox_ttrpc {
    include!(concat!(env!("OUT_DIR"), "/sandbox/sandbox_ttrpc.rs"));
}

#[cfg(feature = "async")]
pub mod sandbox_async {
    include!(concat!(env!("OUT_DIR"), "/sandbox_async/sandbox_ttrpc.rs"));
}

pub(crate) mod mount {
    pub use crate::types::mount::*;
}

pub(crate) mod platform {
    pub use crate::types::platform::*;
}

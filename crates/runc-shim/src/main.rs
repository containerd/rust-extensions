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

use containerd_shim::asynchronous::run;

mod common;
mod console;
mod container;
mod io;
mod processes;
mod runc;
mod service;
mod task;

use service::Service;

#[tokio::main]
async fn main() {
    run::<Service>("io.containerd.runc.v2-rs", None).await;
}

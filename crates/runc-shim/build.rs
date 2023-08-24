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

use std::{process::Command, str::from_utf8};

fn main() {
    let output = match Command::new("git").arg("rev-parse").arg("HEAD").output() {
        Ok(output) => output,
        Err(_) => {
            return;
        }
    };
    let mut hash = from_utf8(&output.stdout).unwrap().trim().to_string();

    let output_dirty = match Command::new("git").arg("diff").arg("--exit-code").output() {
        Ok(output) => output,
        Err(_) => {
            return;
        }
    };

    if !output_dirty.status.success() {
        hash.push_str(".m");
    }
    println!("cargo:rustc-env=CARGO_GIT_HASH={}", hash);
}

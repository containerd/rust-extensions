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

use std::env;
use std::fs;
use std::io;

const PROTO_FILES: &[&str] = &[
    "vendor/github.com/containerd/containerd/api/types/mount.proto",
    "vendor/github.com/containerd/containerd/api/services/snapshots/v1/snapshots.proto",
];

const FIXUP_MODULES: &[&str] = &["containerd.services.snapshots.v1"];

fn main() {
    tonic_build::configure()
        .build_server(true)
        .compile(PROTO_FILES, &["vendor/"])
        .expect("Failed to generate GRPC bindings");

    for module in FIXUP_MODULES {
        fixup_imports(module).expect("Failed to fixup module");
    }
}

// Original containerd's protobuf files contain Go style imports:
// import "github.com/containerd/containerd/api/types/mount.proto";
//
// Tonic produces invalid code for these imports:
// error[E0433]: failed to resolve: there are too many leading `super` keywords
//   --> /containerd-rust-extensions/target/debug/build/containerd-client-protos-0a328c0c63f60cd0/out/containerd.services.diff.v1.rs:47:52
//    |
// 47 |     pub diff: ::core::option::Option<super::super::super::types::Descriptor>,
//    |                                                    ^^^^^ there are too many leading `super` keywords
//
// This func fixes imports to crate level ones, like `crate::types::Mount`
fn fixup_imports(path: &str) -> Result<(), io::Error> {
    let out_dir = env::var("OUT_DIR").unwrap();
    let path = format!("{}/{}.rs", out_dir, path);

    let contents =
        fs::read_to_string(&path)?.replace("super::super::super::types", "crate::api::types");
    fs::write(path, contents)?;
    Ok(())
}

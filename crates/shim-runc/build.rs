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

use std::fs;
use std::path::Path;
use ttrpc_codegen::{Codegen, ProtobufCustomize};

fn main() {
    codegen(
        "src/options",
        &["vendor/github.com/containerd/containerd/runtime/v2/runc/options/oci.proto"],
    );
}

fn codegen(path: impl AsRef<Path>, inputs: impl IntoIterator<Item = impl AsRef<Path>>) {
    let path = path.as_ref();

    fs::create_dir_all(&path).unwrap();

    Codegen::new()
        .inputs(inputs)
        .include("vendor/")
        .rust_protobuf()
        .rust_protobuf_customize(ProtobufCustomize {
            gen_mod_rs: Some(true),
            ..Default::default()
        })
        .out_dir(path)
        .run()
        .expect("Failed to generate protos");
}

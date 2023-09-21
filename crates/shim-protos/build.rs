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

use std::{
    env, fs,
    fs::File,
    io::{BufRead, BufReader},
    path::PathBuf,
};

use ttrpc_codegen::{Codegen, Customize, ProtobufCustomize};

fn main() {
    genmodule(
        "types",
        None,
        &[
            "vendor/gogoproto/gogo.proto",
            "vendor/google/protobuf/empty.proto",
            "vendor/github.com/containerd/containerd/protobuf/plugin/fieldpath.proto",
            "vendor/github.com/containerd/containerd/api/types/descriptor.proto",
            "vendor/github.com/containerd/containerd/api/types/metrics.proto",
            "vendor/github.com/containerd/containerd/api/types/mount.proto",
            #[cfg(feature = "sandbox")]
            "vendor/github.com/containerd/containerd/api/types/sandbox.proto",
            "vendor/github.com/containerd/containerd/api/types/task/task.proto",
            #[cfg(feature = "sandbox")]
            "vendor/github.com/containerd/containerd/api/types/platform.proto",
        ],
        false,
    );

    genmodule(
        "cgroups",
        Some("cgroup1"),
        &["vendor/github.com/containerd/cgroups/cgroup1/stats/metrics.proto"],
        false,
    );

    genmodule(
        "cgroups",
        Some("cgroup2"),
        &["vendor/github.com/containerd/cgroups/cgroup2/stats/metrics.proto"],
        false,
    );

    genmodule(
        "stats",
        None,
        &["vendor/microsoft/hcsshim/cmd/containerd-shim-runhcs-v1/stats/stats.proto"],
        false,
    );

    genmodule(
        "events",
        None,
        &[
            "vendor/github.com/containerd/containerd/api/types/mount.proto",
            "vendor/github.com/containerd/containerd/api/events/container.proto",
            "vendor/github.com/containerd/containerd/api/events/content.proto",
            "vendor/github.com/containerd/containerd/api/events/image.proto",
            "vendor/github.com/containerd/containerd/api/events/namespace.proto",
            #[cfg(feature = "sandbox")]
            "vendor/github.com/containerd/containerd/api/events/sandbox.proto",
            "vendor/github.com/containerd/containerd/api/events/snapshot.proto",
            "vendor/github.com/containerd/containerd/api/events/task.proto",
        ],
        false,
    );

    genmodule(
        "shim",
        None,
        &[
            "vendor/github.com/containerd/containerd/runtime/v2/runc/options/oci.proto",
            "vendor/github.com/containerd/containerd/api/runtime/task/v2/shim.proto",
            "vendor/github.com/containerd/containerd/api/services/ttrpc/events/v1/events.proto",
        ],
        false,
    );

    #[cfg(feature = "async")]
    {
        genmodule(
            "shim_async",
            None,
            &[
                "vendor/github.com/containerd/containerd/api/runtime/task/v2/shim.proto",
                "vendor/github.com/containerd/containerd/api/services/ttrpc/events/v1/events.proto",
            ],
            true,
        );
    }

    #[cfg(feature = "sandbox")]
    {
        genmodule(
            "sandbox",
            None,
            &[
                "vendor/github.com/containerd/containerd/api/types/mount.proto",
                "vendor/github.com/containerd/containerd/api/types/platform.proto",
                "vendor/github.com/containerd/containerd/api/types/metrics.proto",
                "vendor/github.com/containerd/containerd/api/runtime/sandbox/v1/sandbox.proto",
            ],
            false,
        );

        #[cfg(feature = "async")]
        genmodule(
            "sandbox_async",
            None,
            &[
                "vendor/github.com/containerd/containerd/api/types/mount.proto",
                "vendor/github.com/containerd/containerd/api/types/platform.proto",
                "vendor/github.com/containerd/containerd/api/types/metrics.proto",
                "vendor/github.com/containerd/containerd/api/runtime/sandbox/v1/sandbox.proto",
            ],
            true,
        );
    }
}

fn genmodule(name: &str, version: Option<&str>, inputs: &[&str], async_all: bool) {
    let mut out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    out_path.push(name);
    if let Some(version) = version {
        out_path.push(version);
    }

    fs::create_dir_all(&out_path).unwrap();

    Codegen::new()
        .inputs(inputs)
        .include("vendor/")
        .rust_protobuf()
        .rust_protobuf_customize(
            ProtobufCustomize::default()
                .gen_mod_rs(true)
                .generate_accessors(true),
        )
        .customize(Customize {
            async_all,
            ..Default::default()
        })
        .out_dir(&out_path)
        .run()
        .expect("Failed to generate protos");

    // Find all *.rs files generated by TTRPC codegen
    let files = fs::read_dir(&out_path)
        .unwrap()
        .filter_map(|entry| {
            let entry = entry.unwrap();
            if !entry.file_type().unwrap().is_file() {
                None
            } else {
                Some(entry.path())
            }
        })
        .collect::<Vec<_>>();

    // `include!` doesn't handle files with attributes:
    // - https://github.com/rust-lang/rust/issues/18810
    // - https://github.com/rust-lang/rfcs/issues/752
    // Remove all lines that start with:
    // - #![allow(unknown_lints)]
    // - #![cfg_attr(rustfmt, rustfmt::skip)]
    //
    for path in files {
        let file = File::open(&path).unwrap();

        let joined = BufReader::new(file)
            .lines()
            .filter_map(|line| {
                let line = line.unwrap();
                if line.starts_with("#!") || line.starts_with("//!") {
                    None
                } else {
                    Some(line)
                }
            })
            .collect::<Vec<_>>()
            .join("\r\n");

        fs::write(&path, joined).unwrap();
    }
}

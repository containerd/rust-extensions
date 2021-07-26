use std::fs;
use std::io::prelude::*;
use std::path::Path;
use ttrpc_codegen::{Codegen, ProtobufCustomize};

fn main() {
    codegen(
        "src/events",
        &[
            "vendor/github.com/containerd/containerd/api/events/container.proto",
            "vendor/github.com/containerd/containerd/api/events/content.proto",
            "vendor/github.com/containerd/containerd/api/events/image.proto",
            "vendor/github.com/containerd/containerd/api/events/namespace.proto",
            "vendor/github.com/containerd/containerd/api/events/snapshot.proto",
            "vendor/github.com/containerd/containerd/api/events/task.proto",
            "vendor/github.com/containerd/containerd/api/types/mount.proto",
        ],
    );

    codegen(
        "src/shim",
        &[
            "vendor/github.com/containerd/containerd/runtime/v2/task/shim.proto",
            "vendor/github.com/containerd/containerd/api/types/mount.proto",
            "vendor/github.com/containerd/containerd/api/types/task/task.proto",
            "vendor/github.com/containerd/containerd/api/services/ttrpc/events/v1/events.proto",
            "vendor/google/protobuf/empty.proto",
        ],
    );

    // TODO: shim_ttrpc is not included in mod.rs, file a bug upstream.
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open("src/shim/mod.rs")
        .unwrap();
    writeln!(f, "pub mod shim_ttrpc;").unwrap();
    writeln!(f, "pub mod events_ttrpc;").unwrap();
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

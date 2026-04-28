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

use std::{env, fs, io};

const PROTO_FILES: &[&str] = &[
    // Types
    "types/descriptor.proto",
    "types/metrics.proto",
    "types/mount.proto",
    "types/platform.proto",
    "types/sandbox.proto",
    "types/task/task.proto",
    "types/transfer/imagestore.proto",
    "types/transfer/importexport.proto",
    "types/transfer/progress.proto",
    "types/transfer/registry.proto",
    "types/transfer/streaming.proto",
    // Services
    "services/containers/v1/containers.proto",
    "services/content/v1/content.proto",
    "services/diff/v1/diff.proto",
    "services/events/v1/events.proto",
    "services/images/v1/images.proto",
    "services/introspection/v1/introspection.proto",
    "services/leases/v1/leases.proto",
    "services/namespaces/v1/namespace.proto",
    "services/sandbox/v1/sandbox.proto",
    "services/snapshots/v1/snapshots.proto",
    "services/streaming/v1/streaming.proto",
    "services/tasks/v1/tasks.proto",
    "services/transfer/v1/transfer.proto",
    "services/version/v1/version.proto",
    // Events
    "events/container.proto",
    "events/content.proto",
    "events/image.proto",
    "events/namespace.proto",
    "events/snapshot.proto",
    "events/task.proto",
];

const FIXUP_MODULES: &[&str] = &[
    "containerd.services.diff.v1",
    "containerd.services.images.v1",
    "containerd.services.introspection.v1",
    "containerd.services.sandbox.v1",
    "containerd.services.snapshots.v1",
    "containerd.services.tasks.v1",
    "containerd.services.containers.v1",
    "containerd.services.content.v1",
    "containerd.services.events.v1",
];

fn main() {
    let mut config = tonic_prost_build::Config::new();
    config.protoc_arg("--experimental_allow_proto3_optional");
    config.enable_type_names();

    tonic_prost_build::configure()
        .build_server(false)
        .compile_with_config(
            config,
            PROTO_FILES,
            &["vendor/github.com/containerd/containerd/api/", "vendor/"],
        )
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

    let contents = fs::read_to_string(&path)?
        .replace("super::super::super::v1::types", "crate::types::v1") // for tasks service
        .replace("super::super::super::super::types", "crate::types")
        .replace("super::super::super::types", "crate::types")
        .replace("super::super::super::super::google", "crate::google")
        .replace(
            "/// 	filters\\[0\\] or filters\\[1\\] or ... or filters\\[n-1\\] or filters\\[n\\]",
            r#"
            /// ```notrust
            /// 	filters[0] or filters[1] or ... or filters[n-1] or filters[n]
            /// ```"#,
        );
    fs::write(path, contents)?;
    Ok(())
}

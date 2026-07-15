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

const TYPE_PROTO_FILES: &[&str] = &[
    // containerd.types
    "types/descriptor.proto",
    "types/event.proto",
    "types/fieldpath.proto",
    "types/introspection.proto",
    "types/metrics.proto",
    "types/mount.proto",
    "types/platform.proto",
    "types/sandbox.proto",
    // containerd.v1.types
    "types/task/task.proto",
    // containerd.types.transfer
    "types/transfer/container.proto",
    "types/transfer/imagestore.proto",
    "types/transfer/importexport.proto",
    "types/transfer/progress.proto",
    "types/transfer/registry.proto",
    "types/transfer/streaming.proto",
    // containerd.events
    "events/container.proto",
    "events/content.proto",
    "events/image.proto",
    "events/namespace.proto",
    "events/snapshot.proto",
    "events/task.proto",
    // google.rpc
    "google/rpc/status.proto",
];

const SERVICE_PROTO_FILES: &[&str] = &[
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
];

fn main() {
    let includes = &["vendor/github.com/containerd/containerd/api/", "vendor/"];

    let mut type_config = tonic_prost_build::Config::new();
    type_config.enable_type_names();

    tonic_prost_build::configure()
        .build_server(false)
        .compile_with_config(type_config, TYPE_PROTO_FILES, includes)
        .expect("Failed to generate type bindings");

    let mut svc_config = tonic_prost_build::Config::new();
    svc_config.enable_type_names();
    // Tab-indented `filters[0]` in proto comments becomes a Markdown code block
    // that rustdoc tries to compile as a doc-test.
    svc_config.disable_comments([
        ".containerd.services.containers.v1.ListContainersRequest.filters",
        ".containerd.services.content.v1.ListContentRequest.filters",
        ".containerd.services.images.v1.ListImagesRequest.filters",
        ".containerd.services.introspection.v1.PluginsRequest.filters",
        ".containerd.services.snapshots.v1.ListSnapshotsRequest.filters",
    ]);

    tonic_prost_build::configure()
        .build_server(false)
        .extern_path(".containerd.types", "crate::types")
        .extern_path(".containerd.v1.types", "crate::types::v1")
        .extern_path(".containerd.types.transfer", "crate::types::transfer")
        .extern_path(".google.rpc", "crate::google::rpc")
        .compile_with_config(svc_config, SERVICE_PROTO_FILES, includes)
        .expect("Failed to generate GRPC bindings");
}

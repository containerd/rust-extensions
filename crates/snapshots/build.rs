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

fn main() {
    let includes = &["vendor/github.com/containerd/containerd/api/", "vendor/"];

    tonic_prost_build::configure()
        .build_server(false)
        .compile_protos(&["types/mount.proto"], includes)
        .expect("Failed to generate type bindings");

    // Tab-indented `filters[0]` in proto comments becomes a Markdown code block
    // that rustdoc tries to compile as a doc-test.
    let mut svc_config = tonic_prost_build::Config::new();
    svc_config.disable_comments([".containerd.services.snapshots.v1.ListSnapshotsRequest.filters"]);
    tonic_prost_build::configure()
        .build_server(true)
        .extern_path(".containerd.types", "crate::api::types")
        .compile_with_config(
            svc_config,
            &["services/snapshots/v1/snapshots.proto"],
            includes,
        )
        .expect("Failed to generate GRPC bindings");
}

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

//! What the shim tells containerd about itself in reply to the `-info` action.

use std::process::Command;

use containerd_shim::{
    bootstrap::any_is,
    error::{Error, Result},
    io_error, other, other_error,
    protos::{
        protobuf::{well_known_types::any::Any, Message, MessageField},
        shim::oci::Options,
        types::introspection::{RuntimeInfo, RuntimeVersion},
    },
};
use oci_spec::runtime::Features;

use crate::common::DEFAULT_COMMAND;

/// Type URL containerd registers for the runtime features.
const FEATURES_TYPE_URL: &str =
    "types.containerd.io/opencontainers/runtime-spec/1/features/Features";

/// Annotation with which runc advertises checkpoint and restore support.
const CHECKPOINT_ANNOTATION: &str = "org.opencontainers.runc.checkpoint.enabled";

/// Describes the shim and, through `runc features`, the runtime it drives,
/// less what the shim itself does not support.
pub(crate) fn runtime_info(runtime_id: &str, options: Option<&Any>) -> Result<RuntimeInfo> {
    // Options of another type are not ours to read, as in containerd's own shim.
    let options = options.filter(|any| any_is::<Options>(any));
    let mut info = RuntimeInfo {
        name: runtime_id.to_string(),
        version: MessageField::some(RuntimeVersion {
            version: env!("CARGO_PKG_VERSION").to_string(),
            revision: env!("CARGO_GIT_HASH").to_string(),
            ..Default::default()
        }),
        options: MessageField::from_option(options.cloned()),
        ..Default::default()
    };
    let runc = options
        .map(|any| Options::parse_from_bytes(&any.value))
        .transpose()?;
    let binary = runc
        .as_ref()
        .map(|options| options.binary_name())
        .filter(|name| !name.is_empty())
        .unwrap_or(DEFAULT_COMMAND);
    // Features are informational: a runtime without `features`, such as an
    // older youki, still works, it only goes unvalidated.
    match runc_features(binary) {
        Ok(features) => {
            info.features = MessageField::some(Any {
                type_url: FEATURES_TYPE_URL.to_string(),
                value: serde_json::to_vec(&supported(features))?,
                ..Default::default()
            });
        }
        Err(err) => eprintln!("{runtime_id}: features of {binary} unavailable: {err}"),
    }
    Ok(info)
}

/// Runs `<binary> features` and parses its reply.
fn runc_features(binary: &str) -> Result<Features> {
    let path = which::which(binary).map_err(other_error!("look up runtime binary"))?;
    let output = Command::new(path)
        .arg("features")
        .output()
        .map_err(io_error!(e, "run {binary} features"))?;
    if !output.status.success() {
        return Err(other!("{binary} features exited with {}", output.status));
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

/// Removes from `features` what runc can do but this shim cannot.
fn supported(mut features: Features) -> Features {
    // The shim mounts the rootfs itself and cannot idmap it, which is what
    // containerd checks before it uses user namespaces with a runtime.
    if let Some(idmap) = features
        .linux_mut()
        .as_mut()
        .and_then(|linux| linux.mount_extensions_mut().as_mut())
        .and_then(|extensions| extensions.idmap_mut().as_mut())
    {
        idmap.set_enabled(Some(false));
    }
    // Checkpoint and restore are not implemented.
    if let Some(enabled) = features
        .annotations_mut()
        .as_mut()
        .and_then(|annotations| annotations.get_mut(CHECKPOINT_ANNOTATION))
    {
        *enabled = "false".to_string();
    }
    features
}

#[cfg(test)]
mod tests {
    use containerd_shim::util::convert_to_any;
    use oci_spec::runtime::LinuxNamespaceType;

    use super::*;

    // Trimmed from the output of runc 1.5.
    const RUNC_FEATURES: &str = r#"{
        "ociVersionMin": "1.0.0",
        "ociVersionMax": "1.3.0",
        "mountOptions": ["rbind", "ro", "rro"],
        "linux": {
            "namespaces": ["cgroup", "ipc", "mount", "network", "pid", "time", "user", "uts"],
            "mountExtensions": {"idmap": {"enabled": true}}
        },
        "annotations": {
            "org.opencontainers.runc.checkpoint.enabled": "true",
            "org.opencontainers.runc.version": "1.5.1"
        }
    }"#;

    #[test]
    fn unsupported_features_are_switched_off() {
        let features = supported(serde_json::from_str(RUNC_FEATURES).unwrap());

        let linux = features.linux().as_ref().unwrap();
        let idmap = linux
            .mount_extensions()
            .as_ref()
            .unwrap()
            .idmap()
            .as_ref()
            .unwrap();
        assert_eq!(idmap.enabled(), &Some(false));
        // User namespaces themselves work; only the idmapped rootfs does not.
        assert!(linux
            .namespaces()
            .as_ref()
            .unwrap()
            .contains(&LinuxNamespaceType::User));

        let annotations = features.annotations().as_ref().unwrap();
        assert_eq!(annotations[CHECKPOINT_ANNOTATION], "false");
        assert_eq!(annotations["org.opencontainers.runc.version"], "1.5.1");
        assert!(features
            .mount_options()
            .as_ref()
            .unwrap()
            .contains(&"rro".to_string()));
    }

    // Only runc options are echoed back and read; anything else would not
    // even decode as them.
    #[test]
    fn only_runc_options_are_read() {
        let options = Options {
            binary_name: "runc".to_string(),
            ..Default::default()
        };
        let any = convert_to_any(Box::new(options)).unwrap();
        let info = runtime_info("io.containerd.runc.v2-rs", Some(&any)).unwrap();
        assert_eq!(info.name, "io.containerd.runc.v2-rs");
        assert_eq!(
            info.options.as_ref().map(|echoed| echoed.type_url.as_str()),
            Some(any.type_url.as_str())
        );

        let other = Any {
            type_url: "runtimeoptions.v1.Options".to_string(),
            value: vec![0x0a, 0x01, 0x78],
            ..Default::default()
        };
        let info = runtime_info("io.containerd.runc.v2-rs", Some(&other)).unwrap();
        assert!(info.options.is_none());
    }
}

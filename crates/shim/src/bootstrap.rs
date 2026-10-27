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

//! Shim bootstrap protocol.
//!
//! Starting with containerd 2.3 the `start` action receives a protobuf encoded
//! [`BootstrapParams`] message on stdin and must reply with a [`BootstrapResult`]
//! on stdout. Older daemons pass the same information through command line flags,
//! environment variables and (optionally) the runtime options on stdin, and read
//! the reply as a plain address or as JSON carrying the task API version.
//!
//! This module detects which protocol containerd used and normalizes both into
//! [`BootstrapParams`], mirroring the compatibility layer in containerd's `pkg/shim`.
//! See <https://github.com/containerd/containerd/blob/main/docs/runtime-v2.md#bootstrap-protocol-23>.

use std::{
    cmp::Reverse,
    io::{IsTerminal, Read},
};

pub use containerd_shim_protos::bootstrap_api::{
    BootstrapParams, BootstrapResult, Extension, LogLevel,
};
use containerd_shim_protos::{
    protobuf::{well_known_types::any::Any, Enum, EnumOrUnknown, Message, MessageFull},
    TASK_API_VERSION,
};

use crate::{
    args::Flags,
    error::{Error, Result},
    StartOpts, TTRPC_ADDRESS,
};

/// Protocol the shim serves its task API over.
const SHIM_PROTOCOL: &str = "ttrpc";

/// Upper bound on the bootstrap payload read from stdin, same as containerd.
const MAX_INPUT_SIZE: u64 = 10 << 20;

const GRPC_ADDRESS: &str = "GRPC_ADDRESS";

/// How containerd invoked the `start` action, which dictates the reply format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Protocol {
    /// containerd 2.3+: [`BootstrapParams`] on stdin, [`BootstrapResult`] on stdout.
    Bootstrap,
    /// containerd 2.0 to 2.2: flags, environment and runtime options on stdin,
    /// a JSON reply on stdout. A bare address would be taken for a v2 shim.
    Legacy,
}

/// Values the legacy protocol passes through the environment.
#[derive(Default)]
struct LegacyEnv {
    ttrpc_address: String,
    grpc_address: String,
}

impl LegacyEnv {
    fn from_env() -> Self {
        Self {
            ttrpc_address: std::env::var(TTRPC_ADDRESS).unwrap_or_default(),
            grpc_address: std::env::var(GRPC_ADDRESS).unwrap_or_default(),
        }
    }
}

/// Reads the bootstrap payload from stdin and resolves the protocol containerd used.
pub(crate) fn read_start_params(flags: &Flags) -> Result<(StartOpts, Protocol)> {
    let input = read_stdin()?;
    let (params, protocol) = parse_start_params(&input, flags, LegacyEnv::from_env())?;
    Ok((params.into(), protocol))
}

/// Reads what containerd piped to the shim, capped at [`MAX_INPUT_SIZE`].
pub(crate) fn read_stdin() -> Result<Vec<u8>> {
    let stdin = std::io::stdin();
    // Nothing is piped in when the shim is launched by hand from a console.
    if stdin.is_terminal() {
        return Ok(Vec::new());
    }
    let mut input = Vec::new();
    stdin
        .lock()
        .take(MAX_INPUT_SIZE)
        .read_to_end(&mut input)
        .map_err(io_error!(e, "read bootstrap params from stdin"))?;
    Ok(input)
}

/// Normalizes whatever containerd passed to the `start` action into [`BootstrapParams`].
fn parse_start_params(
    input: &[u8],
    flags: &Flags,
    env: LegacyEnv,
) -> Result<(BootstrapParams, Protocol)> {
    let (mut params, protocol) = match parse_bootstrap_params(input, &flags.id, &flags.namespace)? {
        Some(params) => (params, Protocol::Bootstrap),
        None => (legacy_params(input, flags, env), Protocol::Legacy),
    };
    // The `-debug` flag is passed alongside both protocols.
    if flags.debug && params.log_level.value() > LogLevel::LOG_LEVEL_DEBUG.value() {
        params.log_level = EnumOrUnknown::new(LogLevel::LOG_LEVEL_DEBUG);
    }
    // The shim server needs it to publish events; fail here rather than in the
    // detached server, whose stderr nobody sees.
    if params.containerd_ttrpc_address.is_empty() {
        return Err(match protocol {
            Protocol::Bootstrap => other!("bootstrap params carry no containerd TTRPC address"),
            Protocol::Legacy => other!("{TTRPC_ADDRESS} environment variable is not set"),
        });
    }
    Ok((params, protocol))
}

/// Decodes `input` as [`BootstrapParams`], or returns `None` when containerd used
/// the legacy protocol.
fn parse_bootstrap_params(
    input: &[u8],
    id: &str,
    namespace: &str,
) -> Result<Option<BootstrapParams>> {
    if input.is_empty() {
        return Ok(None);
    }
    let Ok(params) = BootstrapParams::parse_from_bytes(input) else {
        return Ok(None);
    };
    if params.instance_id == id && params.namespace == namespace {
        return Ok(Some(params));
    }
    // Legacy runtime options (a protobuf `Any`) decode as `BootstrapParams` too:
    // the type URL lands in `instance_id`, the value in `namespace` and nothing
    // else is set. Anything beyond that is a real message meant for another shim.
    let identity_only = BootstrapParams {
        instance_id: params.instance_id.clone(),
        namespace: params.namespace.clone(),
        ..Default::default()
    };
    if params == identity_only {
        return Ok(None);
    }
    Err(other!(
        "bootstrap params for {}/{} do not match command line arguments {}/{}",
        params.namespace,
        params.instance_id,
        namespace,
        id
    ))
}

/// Builds [`BootstrapParams`] from the legacy flags, environment and stdin payload.
fn legacy_params(input: &[u8], flags: &Flags, env: LegacyEnv) -> BootstrapParams {
    let mut params = BootstrapParams {
        instance_id: flags.id.clone(),
        namespace: flags.namespace.clone(),
        containerd_ttrpc_address: env.ttrpc_address,
        containerd_grpc_address: flags.address.clone(),
        containerd_binary: flags.publish_binary.clone(),
        ..Default::default()
    };
    if params.containerd_grpc_address.is_empty() {
        params.containerd_grpc_address = env.grpc_address;
    }
    // Runtime options, if any, are passed as a protobuf `Any` on stdin.
    if let Ok(any) = Any::parse_from_bytes(input)
        && !any.type_url.is_empty()
    {
        params.extensions.push(Extension {
            value: Some(any).into(),
            ..Default::default()
        });
    }
    params
}

/// Encodes the reply to the `start` action in the format `protocol` expects.
pub(crate) fn encode_start_result(address: &str, protocol: Protocol) -> Result<Vec<u8>> {
    match protocol {
        Protocol::Legacy => Ok(serde_json::to_vec(&serde_json::json!({
            "version": TASK_API_VERSION,
            "address": address,
            "protocol": SHIM_PROTOCOL,
        }))?),
        Protocol::Bootstrap => {
            let result = BootstrapResult {
                version: TASK_API_VERSION,
                address: address.to_string(),
                protocol: SHIM_PROTOCOL.to_string(),
                ..Default::default()
            };
            Ok(result.write_to_bytes()?)
        }
    }
}

/// Whether `any` carries a message of type `M`.
///
/// containerd's `typeurl` writes bare message names (`containerd.runc.v1.Options`)
/// while protobuf's `Any::pack` prefixes them with a host (`type.googleapis.com/...`),
/// so only the last path segment is compared.
pub fn any_is<M: MessageFull>(any: &Any) -> bool {
    let name = any.type_url.rsplit('/').next().unwrap_or_default();
    !name.is_empty() && name == M::descriptor().full_name()
}

/// Maps a wire log level to the nearest known [`LogLevel`].
///
/// The enum is open: containerd may send values this crate does not know about.
/// Ties resolve to the less verbose level.
pub fn nearest_log_level(value: i32) -> LogLevel {
    LogLevel::VALUES
        .iter()
        .copied()
        .min_by_key(|level| (level.value().abs_diff(value), Reverse(level.value())))
        .unwrap_or_default()
}

impl From<BootstrapParams> for StartOpts {
    fn from(params: BootstrapParams) -> Self {
        let log_level = nearest_log_level(params.log_level.value());
        StartOpts {
            id: params.instance_id,
            publish_binary: params.containerd_binary,
            address: params.containerd_grpc_address,
            ttrpc_address: params.containerd_ttrpc_address,
            namespace: params.namespace,
            debug: log_level.value() <= LogLevel::LOG_LEVEL_DEBUG.value(),
            log_level,
            containerd_version: params.containerd_version,
            socket_dir: params.socket_dir.filter(|dir| !dir.is_empty()),
            extensions: params
                .extensions
                .into_iter()
                .filter_map(|ext| ext.value.into_option())
                .collect(),
        }
    }
}

impl StartOpts {
    /// Environment the shim server needs: containerd's TTRPC address for publishing
    /// events, which containerd 2.3+ passes through the bootstrap protocol rather
    /// than the environment. `None` when unknown, so the server keeps what it inherits.
    pub(crate) fn server_env(&self) -> Option<(&'static str, &str)> {
        (!self.ttrpc_address.is_empty()).then_some((TTRPC_ADDRESS, self.ttrpc_address.as_str()))
    }

    /// Looks up the extension of type `M` containerd passed along, e.g. the
    /// runtime options (`containerd.runc.v1.Options`).
    ///
    /// Returns `Ok(None)` when no such extension is present.
    pub fn extension<M: MessageFull>(&self) -> Result<Option<M>> {
        self.extensions
            .iter()
            .find(|any| any_is::<M>(any))
            .map(|any| M::parse_from_bytes(&any.value).map_err(Into::into))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use containerd_shim_protos::shim::oci::Options;

    use super::*;

    const ID: &str = "abc";
    const NAMESPACE: &str = "k8s.io";

    fn flags() -> Flags {
        Flags {
            id: ID.to_string(),
            namespace: NAMESPACE.to_string(),
            address: "/run/containerd/containerd.sock".to_string(),
            publish_binary: "/usr/bin/containerd".to_string(),
            ..Default::default()
        }
    }

    fn env() -> LegacyEnv {
        LegacyEnv {
            ttrpc_address: "/run/containerd/containerd.sock.ttrpc".to_string(),
            grpc_address: "/run/containerd/containerd.sock".to_string(),
        }
    }

    fn runc_options() -> Options {
        Options {
            shim_cgroup: "/shims".to_string(),
            binary_name: "crun".to_string(),
            ..Default::default()
        }
    }

    /// Runtime options the way containerd's `typeurl` encodes them: bare type name.
    fn legacy_options_any(opts: &Options) -> Any {
        let any = crate::util::convert_to_any(Box::new(opts.clone())).unwrap();
        assert_eq!(any.type_url, "containerd.runc.v1.Options");
        any
    }

    fn bootstrap_params() -> BootstrapParams {
        BootstrapParams {
            instance_id: ID.to_string(),
            namespace: NAMESPACE.to_string(),
            log_level: EnumOrUnknown::new(LogLevel::LOG_LEVEL_DEBUG),
            containerd_version: "v2.3.0".to_string(),
            containerd_ttrpc_address: "/run/containerd/containerd.sock.ttrpc".to_string(),
            containerd_grpc_address: "/run/containerd/containerd.sock".to_string(),
            containerd_binary: "/usr/local/bin/containerd".to_string(),
            socket_dir: Some("/run/containerd/s".to_string()),
            extensions: vec![Extension {
                value: Some(legacy_options_any(&runc_options())).into(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn bootstrap_params_are_used_as_is() {
        let expected = bootstrap_params();
        let input = expected.write_to_bytes().unwrap();

        let (params, protocol) = parse_start_params(&input, &flags(), env()).unwrap();

        assert_eq!(protocol, Protocol::Bootstrap);
        assert_eq!(params, expected);
    }

    #[test]
    fn bootstrap_params_for_another_shim_are_rejected() {
        let mut other = bootstrap_params();
        other.instance_id = "someone-else".to_string();
        let input = other.write_to_bytes().unwrap();

        let err = parse_start_params(&input, &flags(), env()).unwrap_err();

        assert!(
            err.to_string()
                .contains("do not match command line arguments"),
            "{err}"
        );
    }

    #[test]
    fn bootstrap_params_without_ttrpc_address_are_rejected() {
        let mut params = bootstrap_params();
        params.containerd_ttrpc_address = String::new();
        let input = params.write_to_bytes().unwrap();

        assert!(parse_start_params(&input, &flags(), env()).is_err());
    }

    #[test]
    fn legacy_without_ttrpc_address_env_is_rejected() {
        let err = parse_start_params(&[], &flags(), LegacyEnv::default()).unwrap_err();

        assert!(err.to_string().contains(TTRPC_ADDRESS), "{err}");
    }

    #[test]
    fn empty_stdin_is_legacy() {
        let (params, protocol) = parse_start_params(&[], &flags(), env()).unwrap();

        assert_eq!(protocol, Protocol::Legacy);
        assert_eq!(params.instance_id, ID);
        assert_eq!(params.namespace, NAMESPACE);
        assert_eq!(
            params.containerd_ttrpc_address,
            "/run/containerd/containerd.sock.ttrpc"
        );
        assert_eq!(
            params.containerd_grpc_address,
            "/run/containerd/containerd.sock"
        );
        assert_eq!(params.containerd_binary, "/usr/bin/containerd");
        assert_eq!(params.log_level.value(), LogLevel::LOG_LEVEL_INFO.value());
        assert!(params.extensions.is_empty());
        assert!(params.socket_dir.is_none());
    }

    #[test]
    fn legacy_debug_flag_maps_to_debug_level() {
        let mut flags = flags();
        flags.debug = true;

        let (params, _) = parse_start_params(&[], &flags, env()).unwrap();

        assert_eq!(params.log_level.value(), LogLevel::LOG_LEVEL_DEBUG.value());
        assert!(StartOpts::from(params).debug);
    }

    #[test]
    fn debug_flag_only_raises_the_bootstrap_log_level() {
        let mut flags = flags();
        flags.debug = true;

        for (wire, expected) in [
            (LogLevel::LOG_LEVEL_INFO, LogLevel::LOG_LEVEL_DEBUG),
            (LogLevel::LOG_LEVEL_TRACE, LogLevel::LOG_LEVEL_TRACE),
        ] {
            let mut params = bootstrap_params();
            params.log_level = EnumOrUnknown::new(wire);
            let input = params.write_to_bytes().unwrap();

            let (params, _) = parse_start_params(&input, &flags, env()).unwrap();

            assert_eq!(params.log_level.value(), expected.value());
        }
    }

    #[test]
    fn server_env_carries_the_ttrpc_address_when_known() {
        let opts = StartOpts::from(bootstrap_params());
        assert_eq!(
            opts.server_env(),
            Some((TTRPC_ADDRESS, "/run/containerd/containerd.sock.ttrpc"))
        );
        assert_eq!(StartOpts::default().server_env(), None);
    }

    #[test]
    fn legacy_grpc_address_falls_back_to_env() {
        let mut flags = flags();
        flags.address = String::new();

        let (params, _) = parse_start_params(&[], &flags, env()).unwrap();

        assert_eq!(
            params.containerd_grpc_address,
            "/run/containerd/containerd.sock"
        );
    }

    #[test]
    fn legacy_runtime_options_become_an_extension() {
        let input = legacy_options_any(&runc_options())
            .write_to_bytes()
            .unwrap();

        let (params, protocol) = parse_start_params(&input, &flags(), env()).unwrap();

        assert_eq!(protocol, Protocol::Legacy);
        assert_eq!(params.extensions.len(), 1);
        let opts = StartOpts::from(params)
            .extension::<Options>()
            .unwrap()
            .expect("runc options");
        assert_eq!(opts, runc_options());
    }

    // Default runc options encode to zero bytes, so the `Any` is just a type
    // URL: it decodes as `BootstrapParams { instance_id: <url> }` and must be
    // rejected by the identity check rather than treated as the new protocol.
    #[test]
    fn legacy_default_options_are_not_mistaken_for_bootstrap_params() {
        let input = legacy_options_any(&Options::default())
            .write_to_bytes()
            .unwrap();

        let (params, protocol) = parse_start_params(&input, &flags(), env()).unwrap();

        assert_eq!(protocol, Protocol::Legacy);
        assert_eq!(params.instance_id, ID);
        let opts = StartOpts::from(params)
            .extension::<Options>()
            .unwrap()
            .expect("runc options");
        assert_eq!(opts, Options::default());
    }

    #[test]
    fn legacy_garbage_on_stdin_is_ignored() {
        let (params, protocol) = parse_start_params(&[0xff, 0xfe, 0x01], &flags(), env()).unwrap();

        assert_eq!(protocol, Protocol::Legacy);
        assert!(params.extensions.is_empty());
    }

    #[test]
    fn start_opts_from_bootstrap_params() {
        let opts = StartOpts::from(bootstrap_params());

        assert_eq!(opts.id, ID);
        assert_eq!(opts.namespace, NAMESPACE);
        assert_eq!(opts.publish_binary, "/usr/local/bin/containerd");
        assert_eq!(opts.address, "/run/containerd/containerd.sock");
        assert_eq!(opts.ttrpc_address, "/run/containerd/containerd.sock.ttrpc");
        assert_eq!(opts.containerd_version, "v2.3.0");
        assert_eq!(opts.socket_dir.as_deref(), Some("/run/containerd/s"));
        assert_eq!(opts.log_level, LogLevel::LOG_LEVEL_DEBUG);
        assert!(opts.debug);
        assert_eq!(opts.extensions.len(), 1);
        assert_eq!(
            opts.extension::<Options>().unwrap().expect("runc options"),
            runc_options()
        );
        assert!(opts.extension::<BootstrapResult>().unwrap().is_none());
    }

    #[test]
    fn empty_socket_dir_is_none() {
        let mut params = bootstrap_params();
        params.socket_dir = Some(String::new());

        assert!(StartOpts::from(params).socket_dir.is_none());
    }

    #[test]
    fn info_level_is_not_debug() {
        let mut params = bootstrap_params();
        params.log_level = EnumOrUnknown::new(LogLevel::LOG_LEVEL_INFO);

        assert!(!StartOpts::from(params).debug);
    }

    #[test]
    fn extension_type_url_may_carry_a_prefix() {
        let any = Any::pack(&runc_options()).unwrap();
        assert!(any.type_url.starts_with("type.googleapis.com/"));
        assert!(any_is::<Options>(&any));
        assert!(!any_is::<BootstrapParams>(&any));
        assert!(!any_is::<Options>(&Any::default()));
    }

    #[test]
    fn unknown_log_levels_map_to_nearest() {
        assert_eq!(nearest_log_level(-8), LogLevel::LOG_LEVEL_TRACE);
        assert_eq!(nearest_log_level(-100), LogLevel::LOG_LEVEL_TRACE);
        assert_eq!(nearest_log_level(-5), LogLevel::LOG_LEVEL_DEBUG);
        assert_eq!(nearest_log_level(-3), LogLevel::LOG_LEVEL_DEBUG);
        // Ties resolve to the less verbose level.
        assert_eq!(nearest_log_level(-2), LogLevel::LOG_LEVEL_INFO);
        assert_eq!(nearest_log_level(1), LogLevel::LOG_LEVEL_INFO);
        assert_eq!(nearest_log_level(9), LogLevel::LOG_LEVEL_FATAL);
        assert_eq!(nearest_log_level(100), LogLevel::LOG_LEVEL_PANIC);
        assert_eq!(nearest_log_level(i32::MIN), LogLevel::LOG_LEVEL_TRACE);
        assert_eq!(nearest_log_level(i32::MAX), LogLevel::LOG_LEVEL_PANIC);
    }

    #[test]
    fn legacy_reply_is_json_with_the_version() {
        let reply = encode_start_result("unix:///run/containerd/s/abc", Protocol::Legacy).unwrap();
        let result: serde_json::Value = serde_json::from_slice(&reply).unwrap();
        assert_eq!(
            result,
            serde_json::json!({
                "version": 3,
                "address": "unix:///run/containerd/s/abc",
                "protocol": "ttrpc",
            })
        );
    }

    #[test]
    fn bootstrap_reply_is_a_result_message() {
        let reply =
            encode_start_result("unix:///run/containerd/s/abc", Protocol::Bootstrap).unwrap();

        let result = BootstrapResult::parse_from_bytes(&reply).unwrap();
        assert_eq!(result.version, 3);
        assert_eq!(result.address, "unix:///run/containerd/s/abc");
        assert_eq!(result.protocol, "ttrpc");
    }
}

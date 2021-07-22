use go_flag::{self, FlagError};
use std::ffi::OsStr;
use thiserror::Error;

/// Flags to be passed from containerd daemon to a shim binary.
/// Reflects https://github.com/containerd/containerd/blob/master/runtime/v2/shim/shim.go#L100
#[derive(Debug, Default)]
pub struct Flags {
    /// Enable debug output in logs.
    pub debug: bool,
    /// Namespace that owns the shim.
    pub namespace: String,
    /// Id of the task.
    pub id: String,
    /// Abstract socket path to serve.
    pub socket: String,
    /// Path to the bundle if not workdir.
    pub bundle: String,
    /// GRPC address back to main containerd.
    pub address: String,
    /// Path to publish binary (used for publishing events).
    pub publish_binary: String,
    /// Shim action (start / delete).
    /// See https://github.com/containerd/containerd/blob/master/runtime/v2/shim/shim.go#L191
    pub action: String,
}

#[derive(Debug, Error, PartialEq)]
pub enum Error {
    /// Either bad or unknown flag.
    #[error("Invalid arg: {0}")]
    InvalidArg(String),
    /// Required flag is missing.
    #[error("Missing arg: {0}")]
    MissingArg(String),
    /// Syntax error.
    #[error("Parse failed: {0}")]
    ParseError(String),
}

/// Parses command line arguments passed to the shim.
/// This func replicates https://github.com/containerd/containerd/blob/master/runtime/v2/shim/shim.go#L110
pub fn parse<S: AsRef<OsStr>>(args: &[S]) -> Result<Flags, Error> {
    let mut flags = Flags::default();

    let args: Vec<String> = go_flag::parse_args(args, |f| {
        f.add_flag("debug", &mut flags.debug);
        f.add_flag("namespace", &mut flags.namespace);
        f.add_flag("id", &mut flags.id);
        f.add_flag("socket", &mut flags.socket);
        f.add_flag("bundle", &mut flags.bundle);
        f.add_flag("address", &mut flags.address);
        f.add_flag("publish-binary", &mut flags.publish_binary);
    })
    .map_err(|e| match e {
        FlagError::BadFlag { flag } => Error::InvalidArg(flag),
        FlagError::UnknownFlag { name } => Error::InvalidArg(name),
        FlagError::ArgumentNeeded { name } => Error::MissingArg(name),
        FlagError::ParseError { error } => Error::ParseError(format!("{:?}", error)),
    })?;

    if let Some(action) = args.get(0) {
        flags.action = action.into();
    }

    if flags.namespace.is_empty() {
        return Err(Error::MissingArg(String::from(
            "Shim namespace cannot be empty",
        )));
    }

    Ok(flags)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_all() {
        let args = [
            "-debug",
            "-id",
            "123",
            "-namespace",
            "default",
            "-socket",
            "/path/to/socket",
            "-publish-binary",
            "/path/to/binary",
            "-bundle",
            "bundle",
            "-address",
            "address",
            "delete",
        ];

        let flags = parse(&args).unwrap();

        assert!(flags.debug);
        assert_eq!(flags.id, "123");
        assert_eq!(flags.namespace, "default");
        assert_eq!(flags.socket, "/path/to/socket");
        assert_eq!(flags.publish_binary, "/path/to/binary");
        assert_eq!(flags.bundle, "bundle");
        assert_eq!(flags.address, "address");
        assert_eq!(flags.action, "delete");
    }

    #[test]
    fn parse_flags() {
        let args = ["-id", "123", "-namespace", "default"];

        let flags = parse(&args).unwrap();

        assert!(!flags.debug);
        assert_eq!(flags.id, "123");
        assert_eq!(flags.namespace, "default");
        assert_eq!(flags.action, "");
    }

    #[test]
    fn parse_action() {
        let args = ["-namespace", "1", "start"];

        let flags = parse(&args).unwrap();
        assert_eq!(flags.action, "start");
        assert_eq!(flags.id, "");
    }

    #[test]
    fn no_namespace() {
        let empty: [String; 0] = [];
        let result = parse(&empty).err();
        assert_eq!(
            result,
            Some(Error::MissingArg(
                "Shim namespace cannot be empty".to_owned()
            ))
        )
    }
}

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

// Forked from https://github.com/pwFoo/rust-runc/blob/313e6ae5a79b54455b0a242a795c69adf035141a/src/lib.rs

/*
 * Copyright 2020 fsyncd, Berlin, Germany.
 * Additional material, copyright of the containerd authors.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use time::{serde::rfc3339, OffsetDateTime};

/// Information for runc container, as printed by `runc state` and
/// `runc list --format json`.
#[derive(Debug, Serialize, Deserialize)]
pub struct Container {
    pub id: String,
    /// Pid of the container's init process; 0 once the container is stopped.
    pub pid: usize,
    pub status: String,
    /// runc reads this from the `bundle` label it records at create time, and
    /// prints an empty string if the label is missing.
    pub bundle: String,
    pub rootfs: String,
    /// runc prints a Go `time.Time`, which is RFC 3339 with nanoseconds.
    #[serde(with = "rfc3339")]
    pub created: OffsetDateTime,
    /// runc leaves this out when the container has no annotations.
    #[serde(default)]
    pub annotations: HashMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fixtures are the output of runc 1.6.0 for three containers: one running
    // with annotations, one created without annotations, and one stopped.

    fn created(secs: i64, nanos: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp_nanos(secs as i128 * 1_000_000_000 + nanos as i128)
            .unwrap()
    }

    #[test]
    fn state_with_annotations() {
        let j = include_str!("../testdata/runc-1.6.0/state-withann.json");
        let c: Container = serde_json::from_str(j).unwrap();
        assert_eq!(c.id, "withann");
        assert_eq!(c.pid, 57);
        assert_eq!(c.status, "running");
        assert_eq!(c.bundle, "/b/withann");
        assert_eq!(c.rootfs, "/rootfs");
        assert_eq!(c.created, created(1795847682, 629512978));
        assert_eq!(c.annotations.len(), 2);
        assert_eq!(c.annotations["org.example.foo"], "bar");
        assert_eq!(
            c.annotations["io.kubernetes.cri.container-type"],
            "container"
        );
    }

    #[test]
    fn state_without_annotations() {
        let j = include_str!("../testdata/runc-1.6.0/state-noann.json");
        let c: Container = serde_json::from_str(j).unwrap();
        assert_eq!(c.id, "noann");
        assert_eq!(c.status, "created");
        assert_eq!(c.created, created(1795847682, 644569638));
        assert!(c.annotations.is_empty());
    }

    #[test]
    fn state_stopped() {
        let j = include_str!("../testdata/runc-1.6.0/state-stopped.json");
        let c: Container = serde_json::from_str(j).unwrap();
        assert_eq!(c.id, "stopped");
        assert_eq!(c.pid, 0);
        assert_eq!(c.status, "stopped");
    }

    #[test]
    fn list() {
        let j = include_str!("../testdata/runc-1.6.0/list.json");
        let list: Vec<Container> = serde_json::from_str(j).unwrap();
        let ids: Vec<_> = list.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["noann", "stopped", "withann"]);
        assert!(list[0].annotations.is_empty());
        assert_eq!(list[2].annotations["org.example.foo"], "bar");
        assert_eq!(list[2].created, created(1795847682, 629512978));
    }

    #[test]
    fn created_serializes_as_rfc3339() {
        let j = include_str!("../testdata/runc-1.6.0/state-withann.json");
        let c: Container = serde_json::from_str(j).unwrap();
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["created"], "2026-11-28T06:34:42.629512978Z");
    }
}

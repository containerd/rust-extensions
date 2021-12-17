//! Remote snapshotter library for containerd.

/// Generated GRPC apis.
pub mod api {
    /// Generated snapshots bindings.
    pub mod snapshots {
        pub mod v1 {
            tonic::include_proto!("containerd.services.snapshots.v1");
        }
    }

    /// Generated `containerd.types` types.
    pub mod types {
        tonic::include_proto!("containerd.types");
    }
}

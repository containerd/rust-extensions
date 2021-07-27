pub use tonic;

pub mod types {
    tonic::include_proto!("containerd.types");

    pub mod v1 {
        tonic::include_proto!("containerd.v1.types");
    }
}

pub mod google {
    pub mod rpc {
        tonic::include_proto!("google.rpc");
    }
}

pub mod services {
    pub mod v1 {
        tonic::include_proto!("containerd.services.containers.v1");
        tonic::include_proto!("containerd.services.content.v1");
        tonic::include_proto!("containerd.services.diff.v1");
        tonic::include_proto!("containerd.services.events.v1");
        tonic::include_proto!("containerd.services.images.v1");
        tonic::include_proto!("containerd.services.introspection.v1");
        tonic::include_proto!("containerd.services.leases.v1");
        tonic::include_proto!("containerd.services.namespaces.v1");

        // Snapshot's `Info` conflicts with Content's `Info`, so wrap it into a separate sub module.
        pub mod snapshots {
            tonic::include_proto!("containerd.services.snapshots.v1");
        }

        tonic::include_proto!("containerd.services.version.v1");
    }
}

pub mod events {
    tonic::include_proto!("containerd.events");
}

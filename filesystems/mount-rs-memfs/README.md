# mount-rs-memfs

The in-memory `FsDriver` implementation for mount-rs. It supports a volatile filesystem and snapshot serialization used by `mount-rs-persist`.

`mount-rs-core` defines the driver and storage contracts. This crate implements those contracts without adding a dependency from core back to the filesystem.

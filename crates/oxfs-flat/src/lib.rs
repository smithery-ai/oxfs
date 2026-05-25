use std::time::Duration;

pub mod fuse_impl;
pub mod inode;

pub use fuse_impl::FlatFuse;

/// Configuration for the flat FUSE filesystem.
pub struct FlatConfig {
    /// TTL for directory and stat caches.
    pub dir_ttl: Duration,
}

impl Default for FlatConfig {
    fn default() -> Self {
        Self {
            dir_ttl: Duration::from_secs(1),
        }
    }
}

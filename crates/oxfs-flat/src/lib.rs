use std::time::Duration;

pub mod cached_op;
pub mod fuse_impl;
pub mod inode;

pub use fuse_impl::FlatFuse;

/// Configuration for the flat FUSE filesystem.
pub struct FlatConfig {
    pub dir_ttl: Duration,
    pub writeback: bool,
}

impl Default for FlatConfig {
    fn default() -> Self {
        Self {
            dir_ttl: Duration::from_secs(1),
            writeback: false,
        }
    }
}

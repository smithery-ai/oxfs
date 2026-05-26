use std::time::Duration;

pub mod cache;
pub mod fuse;
pub mod inode;
pub mod vfs;

pub use fuse::FlatFuse;

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

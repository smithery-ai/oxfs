use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::RwLock;

/// Ephemeral in-memory bidirectional path <-> inode mapping.
/// Inode 1 is always root ("").
pub struct InodeTable {
    path_to_ino: RwLock<HashMap<String, u64>>,
    ino_to_path: RwLock<HashMap<u64, String>>,
    next_ino: AtomicU64,
}

impl Default for InodeTable {
    fn default() -> Self {
        Self::new()
    }
}

impl InodeTable {
    pub fn new() -> Self {
        let mut p2i = HashMap::new();
        let mut i2p = HashMap::new();
        p2i.insert(String::new(), 1);
        i2p.insert(1, String::new());

        Self {
            path_to_ino: RwLock::new(p2i),
            ino_to_path: RwLock::new(i2p),
            next_ino: AtomicU64::new(2),
        }
    }

    /// Returns existing inode or allocates a new one for the given path.
    pub fn allocate(&self, path: &str) -> u64 {
        // Fast path: read lock
        {
            let map = self.path_to_ino.read();
            if let Some(&ino) = map.get(path) {
                return ino;
            }
        }
        // Slow path: write lock
        let mut p2i = self.path_to_ino.write();
        // Double-check after acquiring write lock
        if let Some(&ino) = p2i.get(path) {
            return ino;
        }
        let ino = self.next_ino.fetch_add(1, Ordering::Relaxed);
        p2i.insert(path.to_string(), ino);
        self.ino_to_path.write().insert(ino, path.to_string());
        ino
    }

    /// Resolve an inode back to its path.
    pub fn resolve(&self, ino: u64) -> Option<String> {
        self.ino_to_path.read().get(&ino).cloned()
    }
}

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use crate::cache::CacheLayer;
use oxfs_meta::MetaEngine;

pub struct Prefetcher {
    state: Mutex<HashMap<u64, PrefetchState>>,
    ahead: u32,
}

struct PrefetchState {
    last_chunk: u32,
    sequential_count: u32,
}

impl Prefetcher {
    pub fn new(ahead: u32) -> Self {
        Self {
            state: Mutex::new(HashMap::new()),
            ahead,
        }
    }

    pub fn record_and_should_prefetch(&self, inode: u64, chunk_idx: u32) -> Option<Vec<u32>> {
        let mut state = self.state.lock();
        let entry = state.entry(inode).or_insert(PrefetchState {
            last_chunk: chunk_idx,
            sequential_count: 0,
        });

        let is_sequential = chunk_idx == entry.last_chunk + 1
            || (chunk_idx == entry.last_chunk && entry.sequential_count == 0);

        entry.last_chunk = chunk_idx;

        if is_sequential {
            entry.sequential_count += 1;
            if entry.sequential_count >= 2 {
                let prefetch: Vec<u32> = (1..=self.ahead)
                    .map(|i| chunk_idx + i)
                    .collect();
                return Some(prefetch);
            }
        } else {
            entry.sequential_count = 0;
        }

        None
    }

    pub fn remove(&self, inode: u64) {
        self.state.lock().remove(&inode);
    }
}

pub fn spawn_prefetch<M: MetaEngine + 'static, C: CacheLayer + 'static>(
    meta: &Arc<M>,
    cache: &Arc<C>,
    inode: u64,
    chunks_to_fetch: Vec<u32>,
) {
    let meta = Arc::clone(meta);
    let cache = Arc::clone(cache);
    tokio::spawn(async move {
        for chunk_idx in chunks_to_fetch {
            let slices = match meta.read_slices(inode, chunk_idx).await {
                Ok(s) if !s.is_empty() => s,
                _ => continue,
            };
            for slice in &slices {
                let _ = cache.read_slice(slice.id).await;
            }
        }
    });
}

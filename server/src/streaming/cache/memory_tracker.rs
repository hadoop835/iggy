extern crate sysinfo;

use crate::configs::resource_quota::MemoryResourceQuota;
use crate::configs::system::CacheConfig;
use iggy::utils::byte_size::IggyByteSize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use sysinfo::System;
use tracing::info;

static INSTANCE: OnceLock<Option<Arc<CacheMemoryTracker>>> = OnceLock::new();

#[derive(Debug)]
pub struct CacheMemoryTracker {
    used_memory_bytes: AtomicU64,
    limit_bytes: IggyByteSize,
}

type MessageSize = u64;

impl CacheMemoryTracker {
    pub fn initialize(config: &CacheConfig) -> Option<Arc<CacheMemoryTracker>> {
        INSTANCE
            .get_or_init(|| {
                if config.enabled {
                    Some(Arc::new(CacheMemoryTracker::new(config.size.clone())))
                } else {
                    info!("Cache memory tracker disabled");
                    None
                }
            })
            .clone()
    }

    pub fn get_instance() -> Option<Arc<CacheMemoryTracker>> {
        INSTANCE.get().cloned().flatten()
    }

    fn new(limit: MemoryResourceQuota) -> Self {
        let mut sys = System::new_all();
        sys.refresh_all();

        let total_memory_bytes = IggyByteSize::from(sys.total_memory());
        let free_memory = IggyByteSize::from(sys.free_memory());
        let free_memory_percentage =
            free_memory.as_bytes_u64() as f64 / total_memory_bytes.as_bytes_u64() as f64 * 100.0;
        let used_memory_bytes = AtomicU64::new(0);
        let limit_bytes = limit.into();

        info!(
            "Cache memory tracker started, cache: {}, total memory: {}, free memory: {}, free memory percentage: {:.2}%",
            limit_bytes.as_human_string(), total_memory_bytes.as_human_string(), free_memory, free_memory_percentage
        );

        CacheMemoryTracker {
            used_memory_bytes,
            limit_bytes,
        }
    }

    pub fn increment_used_memory(&self, message_size: MessageSize) {
        let mut current_cache_size_bytes = self.used_memory_bytes.load(Ordering::SeqCst);
        loop {
            let new_size = current_cache_size_bytes + message_size;
            match self.used_memory_bytes.compare_exchange_weak(
                current_cache_size_bytes,
                new_size,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(actual_current) => current_cache_size_bytes = actual_current,
            }
        }
    }

    pub fn decrement_used_memory(&self, message_size: MessageSize) {
        let mut current_cache_size_bytes = self.used_memory_bytes.load(Ordering::SeqCst);
        loop {
            let new_size = current_cache_size_bytes - message_size;
            match self.used_memory_bytes.compare_exchange_weak(
                current_cache_size_bytes,
                new_size,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return,
                Err(actual_current) => current_cache_size_bytes = actual_current,
            }
        }
    }

    pub fn usage_bytes(&self) -> IggyByteSize {
        IggyByteSize::from(self.used_memory_bytes.load(Ordering::SeqCst))
    }

    pub fn will_fit_into_cache(&self, requested_size: IggyByteSize) -> bool {
        IggyByteSize::from(self.used_memory_bytes.load(Ordering::SeqCst)) + requested_size
            <= self.limit_bytes
    }
}

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use cosmwasm_std::Checksum;
use cosmwasm_vm::{Backend, Cache, CacheOptions, Instance, InstanceOptions, Size};

use crate::api::EngineApi;
use crate::error::{EngineError, EngineResult};
use crate::querier::EngineQuerier;
use crate::storage::EngineStorage;

static NEXT_TEMP_CACHE_ID: AtomicU64 = AtomicU64::new(1);

type EngineVmCache = Cache<EngineApi, EngineStorage, EngineQuerier>;

#[derive(Clone, Debug)]
pub struct WasmCacheConfig {
    /// Persistent cache directory. When omitted, a process-local temporary directory is used.
    pub base_dir: Option<PathBuf>,
    /// Capacity of the unpinned in-memory compiled-module cache.
    pub memory_cache_size_mib: usize,
    /// Maximum linear memory available to one Wasm instance.
    pub instance_memory_limit_mib: usize,
    /// Keep newly uploaded contracts in the pinned in-memory cache.
    pub pin_on_upload: bool,
    /// CosmWasm capabilities exposed by this research runtime.
    pub available_capabilities: Vec<String>,
}

impl Default for WasmCacheConfig {
    fn default() -> Self {
        Self {
            base_dir: None,
            memory_cache_size_mib: 128,
            instance_memory_limit_mib: 64,
            pin_on_upload: true,
            available_capabilities: vec!["iterator".to_owned()],
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WasmCacheMetrics {
    pub hits_pinned_memory_cache: u32,
    pub hits_memory_cache: u32,
    pub hits_fs_cache: u32,
    pub misses: u32,
    pub elements_pinned_memory_cache: usize,
    pub elements_memory_cache: usize,
    pub size_pinned_memory_cache: usize,
    pub size_memory_cache: usize,
}

pub(crate) struct WasmModuleCache {
    cache: Option<EngineVmCache>,
    base_dir: PathBuf,
    cleanup_on_drop: bool,
    pin_on_upload: bool,
}

impl WasmModuleCache {
    pub fn new(config: &WasmCacheConfig) -> EngineResult<Self> {
        if config.memory_cache_size_mib == 0 {
            return Err(EngineError::InvalidConfiguration(
                "Wasm memory cache size must be greater than zero".to_owned(),
            ));
        }
        if config.instance_memory_limit_mib == 0 {
            return Err(EngineError::InvalidConfiguration(
                "Wasm instance memory limit must be greater than zero".to_owned(),
            ));
        }

        let (base_dir, cleanup_on_drop) = match &config.base_dir {
            Some(path) => (path.clone(), false),
            None => (temporary_cache_dir(), true),
        };
        if cleanup_on_drop && base_dir.exists() {
            fs::remove_dir_all(&base_dir).map_err(|error| {
                EngineError::InvalidConfiguration(format!(
                    "failed to clear temporary Wasm cache directory {}: {error}",
                    base_dir.display()
                ))
            })?;
        }
        let capabilities: HashSet<String> = config.available_capabilities.iter().cloned().collect();
        let options = CacheOptions::new(
            base_dir.clone(),
            capabilities,
            Size::mebi(config.memory_cache_size_mib),
            Size::mebi(config.instance_memory_limit_mib),
        );

        // SAFETY: For persistent directories, callers must protect the directory from tampering.
        // Temporary directories are unique to this process and created solely for this cache.
        let cache = unsafe { EngineVmCache::new(options) }.map_err(EngineError::from)?;

        Ok(Self {
            cache: Some(cache),
            base_dir,
            cleanup_on_drop,
            pin_on_upload: config.pin_on_upload,
        })
    }

    pub fn save_wasm(&self, wasm: &[u8]) -> EngineResult<Checksum> {
        let checksum = self.cache().store_code(wasm, true, true)?;
        if self.pin_on_upload {
            self.cache().pin(&checksum)?;
        }
        Ok(checksum)
    }

    pub fn get_instance(
        &self,
        checksum: &Checksum,
        backend: Backend<EngineApi, EngineStorage, EngineQuerier>,
        options: InstanceOptions,
    ) -> EngineResult<Instance<EngineApi, EngineStorage, EngineQuerier>> {
        self.cache()
            .get_instance(checksum, backend, options)
            .map_err(EngineError::from)
    }

    pub fn pin(&self, checksum: &Checksum) -> EngineResult<()> {
        self.cache().pin(checksum).map_err(EngineError::from)
    }

    pub fn unpin(&self, checksum: &Checksum) -> EngineResult<()> {
        self.cache().unpin(checksum).map_err(EngineError::from)
    }

    pub fn metrics(&self) -> WasmCacheMetrics {
        let metrics = self.cache().metrics();
        WasmCacheMetrics {
            hits_pinned_memory_cache: metrics.stats.hits_pinned_memory_cache,
            hits_memory_cache: metrics.stats.hits_memory_cache,
            hits_fs_cache: metrics.stats.hits_fs_cache,
            misses: metrics.stats.misses,
            elements_pinned_memory_cache: metrics.elements_pinned_memory_cache,
            elements_memory_cache: metrics.elements_memory_cache,
            size_pinned_memory_cache: metrics.size_pinned_memory_cache,
            size_memory_cache: metrics.size_memory_cache,
        }
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    fn cache(&self) -> &EngineVmCache {
        self.cache
            .as_ref()
            .expect("Wasm cache is present until WasmModuleCache::drop")
    }
}

impl Drop for WasmModuleCache {
    fn drop(&mut self) {
        drop(self.cache.take());
        if self.cleanup_on_drop {
            let _ = fs::remove_dir_all(&self.base_dir);
        }
    }
}

fn temporary_cache_dir() -> PathBuf {
    let id = NEXT_TEMP_CACHE_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("acg-cosmwasm-cache-{}-{id}", std::process::id()))
}

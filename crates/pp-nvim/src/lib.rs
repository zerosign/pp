//! `pp-nvim` — expose pp's index & search functions for Neovim.
//!
//! Two layers:
//!
//! - a pure-Rust API (rlib) mirroring the previous surface, for tests and Rust
//!   consumers;
//! - an FFI surface (`cdylib`, `libpp_nvim.so`) for Neovim: `pp_search`,
//!   `pp_string_free` and `pp_refresh`, called from `LuaJIT` via `ffi.load`.
//!
//! The picker and keymaps live on the plugin side (`plugin/init.lua`).

use std::path::PathBuf;

pub use pp::{Config, cache_paths, get_cache_path, get_config_path, load_config};

/// Result type re-exported for convenience.
pub type Result<T> = pp::Result<T>;

/// Rebuild the index from scratch; returns the number of indexed repositories.
///
/// # Errors
///
/// Returns an error when the cache directory or index path cannot be
/// resolved, or when the rebuild fails (see [`pp::reindex`]).
pub fn rebuild_index() -> Result<usize> {
    let config = pp::load_config();
    let (cache_dir, db_path) = pp::cache_paths()?;
    let repos = pp::reindex(&config, &cache_dir, &db_path)?;
    Ok(repos.len())
}

/// List all indexed repositories (auto-rebuilds the index if missing or stale).
///
/// # Errors
///
/// Returns an error when the cache paths cannot be resolved or a required
/// rebuild fails (see [`pp::get_repos`]).
pub fn list_repos() -> Result<Vec<String>> {
    let config = pp::load_config();
    let (cache_dir, db_path) = pp::cache_paths()?;
    pp::get_repos(&config, &cache_dir, &db_path, false)
}

/// List repositories with a live scan, bypassing the cache.
///
/// # Errors
///
/// Returns an error when the cache paths cannot be resolved or the scan fails.
pub fn fresh_repos() -> Result<Vec<String>> {
    let config = pp::load_config();
    let (cache_dir, db_path) = pp::cache_paths()?;
    pp::get_repos(&config, &cache_dir, &db_path, true)
}

/// Case-insensitive substring search over the cached repository list.
///
/// # Errors
///
/// Returns an error when the repository list cannot be loaded (see
/// [`pp::get_repos`]).
pub fn search_repos(query: &str) -> Result<Vec<String>> {
    let config = pp::load_config();
    let (cache_dir, db_path) = pp::cache_paths()?;
    pp::search_repos(&config, &cache_dir, &db_path, query)
}

/// The index database path (useful for cache-busting or display on the plugin side).
///
/// # Errors
///
/// Returns an error when the cache directory cannot be resolved (see
/// [`pp::cache_paths`]).
pub fn index_db_path() -> Result<PathBuf> {
    Ok(pp::cache_paths()?.1)
}

/// FFI bridge for Neovim.
///
/// The repository list is cached in a process-global `RwLock` so per-keystroke
/// searches never re-read the index file. Every call compares the index file's
/// mtime against the cached one and reloads on change, so a reindex performed
/// by another process (e.g. `pp index` or `:PpIndex`) is picked up lazily.
pub mod ffi {
    use super::Result;
    use std::ffi::{CStr, CString, c_char};
    use std::fs;
    use std::sync::{OnceLock, PoisonError, RwLock};
    use std::time::SystemTime;

    struct Cache {
        repos: Vec<String>,
        /// mtime of `index.fst` when `repos` was loaded.
        db_mtime: Option<SystemTime>,
    }

    fn cache() -> &'static RwLock<Cache> {
        static CACHE: OnceLock<RwLock<Cache>> = OnceLock::new();

        CACHE.get_or_init(|| {
            RwLock::new(Cache {
                repos: Vec::new(),
                // Sentinel: guarantees a load on the first call even when the
                // index file does not exist yet.
                db_mtime: Some(SystemTime::UNIX_EPOCH),
            })
        })
    }

    fn read_cache() -> std::sync::RwLockReadGuard<'static, Cache> {
        cache().read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write_cache() -> std::sync::RwLockWriteGuard<'static, Cache> {
        cache().write().unwrap_or_else(PoisonError::into_inner)
    }

    fn index_mtime() -> Option<SystemTime> {
        let (_, db_path) = pp::cache_paths().ok()?;
        fs::metadata(db_path).ok()?.modified().ok()
    }

    fn load_repos() -> Result<Vec<String>> {
        let config = pp::load_config();
        let (cache_dir, db_path) = pp::cache_paths()?;
        pp::get_repos(&config, &cache_dir, &db_path, false)
    }

    /// Cached repository list, (re)loading it when the index file changed or
    /// first appeared since the last call.
    fn repos() -> Result<Vec<String>> {
        let current = index_mtime();
        if read_cache().db_mtime == current {
            return Ok(read_cache().repos.clone());
        }

        let loaded = load_repos()?;
        let mut guard = write_cache();
        let current = index_mtime();
        if guard.db_mtime != current {
            guard.repos = loaded;
            guard.db_mtime = current;
        }
        Ok(guard.repos.clone())
    }

    fn mode_from_int(mode: i32) -> pp::SearchMode {
        match mode {
            1 => pp::SearchMode::Prefix,
            2 => pp::SearchMode::Fuzzy,
            3 => pp::SearchMode::Subseq,
            _ => pp::SearchMode::Substring,
        }
    }

    /// Search the cached repository index.
    ///
    /// Returns a heap-allocated, NUL-terminated string holding the matching
    /// full paths (newline-separated; empty when nothing matches), or a null
    /// pointer on error. Release the result with [`pp_string_free`].
    ///
    /// `mode`: 0 substring (default), 1 prefix, 2 fuzzy, 3 subseq.
    ///
    /// # Safety
    ///
    /// `query` must point to a valid NUL-terminated string (or be null, which
    /// is treated as an error). `distance` only applies to fuzzy mode; `limit`
    /// caps results (`<= 0` for unlimited).
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pp_search(
        query: *const c_char,
        mode: i32,
        distance: u32,
        limit: i32,
    ) -> *mut c_char {
        if query.is_null() {
            return std::ptr::null_mut();
        }

        let result = (|| -> Result<String> {
            let query = unsafe { CStr::from_ptr(query) }
                .to_str()
                .map_err(|err| format!("invalid UTF-8 query: {err}"))?;

            let matches = pp::search_matches(
                &repos()?,
                query,
                mode_from_int(mode),
                distance,
                usize::try_from(limit).ok().filter(|n| *n > 0),
            );

            Ok(matches.join("\n"))
        })();

        match result {
            Ok(text) => CString::new(text).map_or(std::ptr::null_mut(), CString::into_raw),
            Err(_) => std::ptr::null_mut(),
        }
    }

    /// Release a string previously returned by [`pp_search`].
    ///
    /// # Safety
    ///
    /// `ptr` must be a pointer returned by `pp_search`, or null.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pp_string_free(ptr: *mut c_char) {
        if ptr.is_null() {
            return;
        }

        unsafe { drop(CString::from_raw(ptr)) };
    }

    /// Reload the cached repository list from disk immediately.
    ///
    /// Returns 1 on success, 0 on failure.
    #[unsafe(no_mangle)]
    pub extern "C" fn pp_refresh() -> i32 {
        match load_repos() {
            Ok(repos) => {
                let mut guard = write_cache();
                guard.repos = repos;
                guard.db_mtime = index_mtime();
                1
            }
            Err(_) => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{CStr, CString};

    /// Exercises the exported FFI entry points with real raw pointers against
    /// a small throwaway index. Single test because the FFI cache is
    /// process-global and reads XDG env vars on first use.
    #[test]
    fn ffi_search_roundtrip() {
        let dir = std::env::temp_dir().join(format!("pp-ffi-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cache = dir.join("cache");
        let db = cache.join("pp").join("index.fst");

        // One fake repository under a temp root.
        let repo_dir = dir.join("repos").join("alpha-project");
        std::fs::create_dir_all(&repo_dir).unwrap();
        let repo_path = repo_dir.to_string_lossy().into_owned();

        // Write the index directly with fst (rather than pp::reindex, which
        // uses rayon; rayon's thread spawns are not miri-compatible).
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let mut builder = fst::SetBuilder::new(Vec::<u8>::new()).unwrap();
        builder.insert(&repo_path).unwrap();
        std::fs::write(&db, builder.into_inner().unwrap()).unwrap();

        // SAFETY: the test process owns these env vars; no other thread reads
        // them before the FFI cache is initialised below.
        unsafe {
            std::env::set_var("XDG_CACHE_HOME", &cache);
            std::env::set_var("XDG_CONFIG_HOME", dir.join("config"));
        }

        // Round trip: raw pointer in -> NUL-terminated string out -> freed.
        let query = CString::new("alpha").unwrap();
        let ptr = unsafe { ffi::pp_search(query.as_ptr(), 0, 0, -1) };
        assert!(!ptr.is_null());
        let text = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap();
        assert!(text.contains("alpha-project"));
        unsafe { ffi::pp_string_free(ptr) };

        // Limit applies.
        let ptr = unsafe { ffi::pp_search(query.as_ptr(), 0, 0, 1) };
        let text = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap();
        assert_eq!(text.matches('\n').count(), 0);
        unsafe { ffi::pp_string_free(ptr) };

        // Null query -> null result (nothing to free).
        assert!(unsafe { ffi::pp_search(std::ptr::null(), 0, 0, -1) }.is_null());

        // Invalid UTF-8 -> null result (nothing to free).
        let bad = CString::new([0xff_u8, 0xfe]).unwrap();
        assert!(unsafe { ffi::pp_search(bad.as_ptr(), 0, 0, -1) }.is_null());

        // Null pointer is a no-op free.
        unsafe { ffi::pp_string_free(std::ptr::null_mut()) };

        // Refresh forces a reload and reports success.
        assert_eq!(ffi::pp_refresh(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}

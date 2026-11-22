//! `pp-nvim` — FFI surface (`cdylib`, `libpp_nvim.so`) exposing `pp`'s cache &
//! search functions to Neovim (`pp_search`, `pp_string_free`, `pp_refresh`),
//! called from `LuaJIT` via `ffi.load`.

pub use pp::{Config, Result, cache_paths, get_cache_path, get_config_path, load_config};

/// FFI bridge for Neovim.
///
/// The memmapped [`pp::Cache`] is held in a process-global `RwLock` so
/// per-keystroke searches borrow `&str`s zero-copy straight from the mapping
/// under a single read lock without cloning `String`s. Every call compares the
/// cache file's mtime against the cached one and reloads on change, so a
/// reindex performed by another process (`pp index` or `:PpIndex`) is picked up
/// automatically.
pub mod ffi {
    use super::Result;
    use std::ffi::{CStr, CString, c_char};
    use std::fs;
    use std::sync::{OnceLock, PoisonError, RwLock};
    use std::time::SystemTime;

    struct State {
        cache: Option<pp::Cache>,
        /// mtime of `pp.cache` when `cache` was mapped.
        db_mtime: Option<SystemTime>,
    }

    fn state() -> &'static RwLock<State> {
        static STATE: OnceLock<RwLock<State>> = OnceLock::new();

        STATE.get_or_init(|| {
            RwLock::new(State {
                cache: None,
                // Sentinel: guarantees a load on the first call even when the
                // index file does not exist yet.
                db_mtime: Some(SystemTime::UNIX_EPOCH),
            })
        })
    }

    fn read_state() -> std::sync::RwLockReadGuard<'static, State> {
        state().read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write_state() -> std::sync::RwLockWriteGuard<'static, State> {
        state().write().unwrap_or_else(PoisonError::into_inner)
    }

    fn index_mtime() -> Option<SystemTime> {
        let (_, db_path) = pp::cache_paths().ok()?;
        fs::metadata(db_path).ok()?.modified().ok()
    }

    fn open_cache() -> Result<pp::Cache> {
        let config = pp::load_config();
        let (cache_dir, db_path) = pp::cache_paths()?;
        pp::get_repos(&config, &cache_dir, &db_path, false)
    }

    /// Run `f` against the borrowed repository paths from the mapped cache,
    /// reloading the mapping only when `pp.cache`'s mtime changed.
    fn with_repos<T>(f: impl FnOnce(&[&str]) -> T) -> Result<T> {
        let current = index_mtime();
        {
            let guard = read_state();
            if guard.db_mtime == current
                && let Some(ref cache) = guard.cache
            {
                let paths = cache.paths();
                return Ok(f(&paths));
            }
        }

        let loaded = open_cache()?;
        let mut guard = write_state();
        guard.db_mtime = index_mtime();
        guard.cache = Some(loaded);
        let paths = guard.cache.as_ref().expect("just stored").paths();
        Ok(f(&paths))
    }

    fn mode_from_int(mode: i32) -> pp::SearchMode {
        match mode {
            1 => pp::SearchMode::Prefix,
            2 => pp::SearchMode::Fuzzy,
            3 => pp::SearchMode::Subseq,
            _ => pp::SearchMode::Substring,
        }
    }

    /// Search the cached repository list.
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

            let search_mode = mode_from_int(mode);
            let max_items = usize::try_from(limit).ok().filter(|n| *n > 0);
            with_repos(|paths| {
                pp::search_matches(paths, query, search_mode, distance, max_items).join("\n")
            })
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
        match open_cache() {
            Ok(cache) => {
                let mut guard = write_state();
                guard.cache = Some(cache);
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
        let db = cache.join("pp").join("pp.cache");

        // One fake repository under a temp root.
        let repo_dir = dir.join("repos").join("alpha-project");
        std::fs::create_dir_all(&repo_dir).unwrap();
        let repo_path = repo_dir.to_string_lossy().into_owned();

        // Write the cache directly (rather than pp::reindex) so the test owns
        // the exact repository list without a live scan.
        pp::write_cache(db.parent().unwrap(), &db, &[repo_path]).unwrap();

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

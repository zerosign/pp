//! `pp` — shared core for the pp repository locator.
//!
//! This crate holds only the *cache & search* logic: config loading, root
//! scanning, and the on-disk cache. The interactive UI (fuzzy picker) lives in
//! `pp-cli`, and nvim-facing wrappers live in `pp-nvim`.
//!
//! # Cache format
//!
//! The cache is an [`rkyv`]-serialized `RepoCache` — a magic tag, a format
//! version, and a `Vec<String>` of repository paths — written atomically via a
//! temp file + rename. It is **memmapped** on read and accessed zero-copy:
//! [`Cache`] hands out borrowed `&str`s straight out of the mapping, so no
//! per-path allocation or UTF-8 validation happens on the hot path. Because
//! the archive is immutable and swapped by rename, any number of readers
//! (CLI + nvim simultaneously) share it, and a rebuild never disturbs an
//! in-flight read.
//!
//! # Access modes
//!
//! - [`reindex`] — write path: scan roots and persist the cache.
//! - `Cache::open` — read path: mmap and validate the cached archive.
//! - [`get_repos`] — convenience accessor that auto-rebuilds a missing,
//!   stale, or unreadable cache.

use memmap2::Mmap;
use rkyv::string::ArchivedString;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// Result type used across the crate.
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Locator configuration, read from `$XDG_CONFIG_HOME/pp/config.toml`.
#[derive(Serialize, Deserialize, Debug)]
pub struct Config {
    /// Parent paths scanned for repositories.
    #[serde(default = "default_roots")]
    pub roots: Vec<String>,
    /// Directory names skipped while scanning.
    #[serde(default = "default_ignored_dirs")]
    pub ignored_dirs: Vec<String>,
    /// File/dir names that mark a directory as a repository.
    #[serde(default = "default_project_markers")]
    pub project_markers: Vec<String>,
    /// Maximum recursion depth when scanning.
    #[serde(default = "default_max_depth")]
    pub max_depth: usize,
    /// Rebuild the cache when it is older than this many days.
    #[serde(default = "default_index_ttl_days")]
    pub index_ttl_days: u64,
    /// Follow symlinked directories while scanning. Off by default: an
    /// outbound link can pull arbitrary trees into the scan, so indexing
    /// repositories reached through symlinks is an explicit opt-in.
    #[serde(default)]
    pub follow_symlinks: bool,
    /// Subtrees where a repository is not treated as a leaf: the scan keeps
    /// descending past a project marker, so nested repositories are
    /// indexed too. Empty by default: a detected repository always
    /// terminates the walk.
    #[serde(default)]
    pub nested_projects: Vec<String>,
}

fn default_max_depth() -> usize {
    6
}

fn default_index_ttl_days() -> u64 {
    7
}

fn default_roots() -> Vec<String> {
    vec!["~/Repositories".to_string()]
}

fn default_ignored_dirs() -> Vec<String> {
    vec![
        "node_modules".to_string(),
        ".cargo".to_string(),
        "target".to_string(),
        ".git".to_string(),
        "Library".to_string(),
        "Applications".to_string(),
        "System".to_string(),
        "Pictures".to_string(),
        "Music".to_string(),
        "Downloads".to_string(),
        "Desktop".to_string(),
        "Documents".to_string(),
        "Public".to_string(),
        "Templates".to_string(),
        "Videos".to_string(),
        "Cache".to_string(),
        ".cache".to_string(),
    ]
}

fn default_project_markers() -> Vec<String> {
    vec![
        ".git".to_string(),
        "go.mod".to_string(),
        "Cargo.toml".to_string(),
        "package.json".to_string(),
    ]
}

impl Default for Config {
    fn default() -> Self {
        Self {
            roots: default_roots(),
            ignored_dirs: default_ignored_dirs(),
            project_markers: default_project_markers(),
            max_depth: default_max_depth(),
            index_ttl_days: default_index_ttl_days(),
            follow_symlinks: false,
            nested_projects: Vec::new(),
        }
    }
}

/// Load the config from the XDG config dir, falling back to defaults.
///
/// Missing or unparsable config files fall back to [`Config::default`].
#[must_use]
pub fn load_config() -> Config {
    let Some(config_file) = get_config_path().map(|dir| dir.join("config.toml")) else {
        return Config::default();
    };
    match fs::read_to_string(&config_file) {
        Ok(content) => toml::from_str(&content).unwrap_or_else(|err| {
            eprintln!("Warning: invalid config {}: {err}", config_file.display());
            Config::default()
        }),
        Err(_) => Config::default(),
    }
}

/// XDG config directory for pp.
#[must_use]
pub fn get_config_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("com", "pp", "pp")
        .map(|proj| proj.config_dir().to_path_buf())
        .or_else(|| {
            directories::UserDirs::new()
                .map(|dirs| dirs.home_dir().join(".config").join("pp"))
        })
}

/// XDG cache directory for pp.
#[must_use]
pub fn get_cache_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("com", "pp", "pp")
        .map(|proj| proj.cache_dir().to_path_buf())
        .or_else(|| {
            directories::UserDirs::new()
                .map(|dirs| dirs.home_dir().join(".cache").join("pp"))
        })
}

/// Resolve the cache directory and the cache file path.
///
/// # Errors
///
/// Returns an error when the cache directory cannot be determined from the
/// environment.
pub fn cache_paths() -> Result<(PathBuf, PathBuf)> {
    let cache_dir = get_cache_path().ok_or("Could not determine cache directory")?;
    Ok((cache_dir.clone(), cache_dir.join("pp.cache")))
}

/// Magic tag at the start of every cache file.
const CACHE_MAGIC: [u8; 8] = *b"PPCACHE\0";

/// Cache format version; bump to invalidate caches written by older versions.
const CACHE_VERSION: u32 = 1;

/// The archived root of a cache file.
#[derive(rkyv::Archive, rkyv::Serialize)]
struct RepoCache {
    magic: [u8; 8],
    version: u32,
    repos: Vec<String>,
}

/// A read-only view of the repository list.
///
/// Normally this is a memmapped cache file; after a `--no-cache` live scan it
/// owns the list in memory instead. Either way [`Cache::paths`] borrows the
/// underlying strings, so a picker or search can consume them without copying.
pub struct Cache {
    inner: CacheInner,
}

enum CacheInner {
    Mapped(Mmap),
    Owned(Vec<String>),
}

impl Cache {
    /// Open a cache file, memmapping it for zero-copy reads.
    ///
    /// Validates the archive once (magic + version); later reads use the
    /// unchecked accessor against the immutable mapping.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be opened or mapped, is not a
    /// valid rkyv archive, or carries an unexpected magic/version.
    fn open(db_path: &Path) -> Result<Self> {
        let file = fs::File::open(db_path)?;
        // SAFETY: a cache file is only ever replaced by an atomic rename; pp
        // never mutates one in place, so this mapping cannot be truncated
        // underneath a live read.
        let map = unsafe { Mmap::map(&file)? };
        let archived = rkyv::access::<ArchivedRepoCache, rkyv::rancor::Error>(&map)
            .map_err(|err| format!("unreadable cache: {err}"))?;
        if archived.magic != CACHE_MAGIC || archived.version.to_native() != CACHE_VERSION {
            return Err("cache format mismatch".into());
        }
        Ok(Self {
            inner: CacheInner::Mapped(map),
        })
    }

    /// A cache backed by an in-memory list (used by `--no-cache`).
    fn owned(repos: Vec<String>) -> Self {
        Self {
            inner: CacheInner::Owned(repos),
        }
    }

    /// The archived root, re-derived from the mapping.
    ///
    /// [`Cache::open`] already ran the checked accessor and the mapping is
    /// never mutated, so re-reading it here is sound.
    fn archived(&self) -> &ArchivedRepoCache {
        let CacheInner::Mapped(map) = &self.inner else {
            unreachable!("archived() is only valid for mapped caches");
        };
        // SAFETY: validated in `open`; the underlying bytes are immutable.
        unsafe { rkyv::access_unchecked::<ArchivedRepoCache>(map) }
    }

    /// Number of repositories in the cache.
    #[must_use]
    pub fn len(&self) -> usize {
        match &self.inner {
            CacheInner::Mapped(_) => self.archived().repos.len(),
            CacheInner::Owned(repos) => repos.len(),
        }
    }

    /// Whether the cache holds no repositories.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Borrow every repository path, in stored (sorted) order.
    ///
    /// For a mapped cache the returned `&str`s point straight into the mmap;
    /// the `Vec` is a scratch buffer of pointers, not string copies.
    #[must_use]
    pub fn paths(&self) -> Vec<&str> {
        match &self.inner {
            CacheInner::Mapped(_) => self
                .archived()
                .repos
                .iter()
                .map(ArchivedString::as_str)
                .collect(),
            CacheInner::Owned(repos) => repos.iter().map(String::as_str).collect(),
        }
    }
}

/// Expand `~` and anchor relative paths to `home`.
///
/// Relative roots must not silently depend on the working directory of
/// whichever process happens to index (`pp index` in a shell vs `:PpIndex`
/// inside nvim would otherwise build different indexes from one config), so
/// they resolve against the home directory.
fn expand_path_with(path: &str, home: &Path) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/").or_else(|| (path == "~").then_some("")) {
        return home.join(rest);
    }
    let candidate = Path::new(path);
    if candidate.is_relative() {
        return home.join(candidate);
    }
    candidate.to_path_buf()
}

fn expand_path(path: &str) -> PathBuf {
    match directories::UserDirs::new() {
        Some(dirs) => expand_path_with(path, dirs.home_dir()),
        None => PathBuf::from(path),
    }
}

/// DFS worklist of directories awaiting a scan, with their root-relative depth.
type Pending = Vec<(PathBuf, usize)>;

/// Accumulator-shaped scan: repositories found under `root` are pushed into `out`.
///
/// `pending` is a reusable DFS stack shared across the whole scan (all
/// directories and all roots), so only `out` + `pending` allocate — both
/// amortized-grown — instead of building a `Vec` per visited directory.
///
/// A directory holding a project marker is a leaf: children seen before
/// the marker are discarded (`pending.truncate`) and the rest are skipped
/// (early `break`), mirroring the original recursive walk. Inside a
/// `nested_projects` subtree the marker is noted but iteration continues, so
/// every child is queued and nested repositories are found.
fn scan_directory(
    root: &Path,
    out: &mut Vec<PathBuf>,
    pending: &mut Pending,
    config: &Config,
    nested_projects: &[PathBuf],
) {
    let ignored_dirs = config.ignored_dirs.as_slice();
    let project_markers = config.project_markers.as_slice();
    let max_depth = config.max_depth;
    let follow_symlinks = config.follow_symlinks;

    pending.push((root.to_path_buf(), 0));

    while let Some((dir, depth)) = pending.pop() {
        if depth >= max_depth {
            continue;
        }

        if dir
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| ignored_dirs.iter().any(|ignored| ignored == name))
        {
            continue;
        }

        let Ok(read_dir) = fs::read_dir(&dir) else {
            continue;
        };

        let mark = pending.len();
        // Repos are normally leaves; only inside a `nested_projects` subtree
        // (at/below a listed prefix) does descent continue past a project
        // marker. A dir that is an ANCESTOR of a listed prefix is also
        // covered, since pruning it would make the prefix unreachable.
        let covered = nested_projects
            .iter()
            .any(|nested| dir.starts_with(nested) || nested.starts_with(&dir));
        let mut has_marker = false;
        for entry in read_dir.filter_map(std::result::Result::ok) {
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            if project_markers.iter().any(|marker| marker == &*name) {
                has_marker = true;
                if !covered {
                    break;
                }
                continue;
            }
            // entry.file_type() does not follow symlinks; fs::metadata does.
            // Following is opt-in (Config::follow_symlinks) because an outbound
            // link can pull arbitrary trees into the scan.
            let is_dir = if follow_symlinks {
                fs::metadata(entry.path()).is_ok_and(|meta| meta.is_dir())
            } else {
                entry.file_type().is_ok_and(|ft| ft.is_dir())
            };

            if is_dir && !ignored_dirs.iter().any(|ignored| ignored == &*name) {
                pending.push((entry.path(), depth + 1));
            }
        }

        if has_marker {
            if !covered {
                pending.truncate(mark);
            }
            out.push(dir);
        }
    }
}

/// Live-scan all configured roots for repositories (no cache involved).
#[must_use]
pub fn scan_repos(config: &Config) -> Vec<PathBuf> {
    let nested: Vec<PathBuf> = config.nested_projects.iter().map(|p| expand_path(p)).collect();
    let mut out = Vec::new();
    let mut pending = Vec::new();
    for root in config
        .roots
        .iter()
        .map(|root| expand_path(root))
        .filter(|root| root.exists())
    {
        scan_directory(&root, &mut out, &mut pending, config, &nested);
    }
    out
}

fn to_sorted_strings(repos: Vec<PathBuf>) -> Vec<String> {
    let mut strings: Vec<String> = repos
        .into_iter()
        .filter_map(|repo| repo.to_str().map(str::to_string))
        .collect();

    strings.sort();
    strings.dedup();
    strings
}

fn is_stale(db_path: &Path, config: &Config) -> bool {
    fs::metadata(db_path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age > Duration::from_secs(config.index_ttl_days * 86400))
}

/// Serialize repository paths to the cache file.
///
/// The list is sorted and de-duplicated before writing. The archive goes to a
/// per-process temp file and is atomically renamed into place, so concurrent
/// readers keep seeing the previous cache until the swap. A plain `Vec<String>`
/// is enough because rkyv needs no sorted-key structure — unlike the old `fst`
/// set, ordering is a display convenience, not a storage requirement.
///
/// # Errors
///
/// Returns an error when the cache directory cannot be created, the archive
/// cannot be serialized, or the temp file cannot be written or renamed.
pub fn write_cache(cache_dir: &Path, db_path: &Path, repos: &[String]) -> Result<()> {
    fs::create_dir_all(cache_dir)?;

    let file_name = db_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("pp.cache");
    let tmp_path = db_path.with_file_name(format!("{file_name}.{}.tmp", std::process::id()));

    let mut sorted = repos.to_vec();
    sorted.sort();
    sorted.dedup();

    let cache = RepoCache {
        magic: CACHE_MAGIC,
        version: CACHE_VERSION,
        repos: sorted,
    };
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&cache)?;

    fs::write(&tmp_path, &bytes[..])?;
    fs::rename(&tmp_path, db_path)?;
    Ok(())
}

/// Write path: scan roots and persist the cache.
///
/// Returns the sorted list of cached repositories. Only this cache file is
/// written; a stale `index.fst`/`index.db` from an older format is left in
/// place rather than deleted, so switching between versions stays reversible.
///
/// # Errors
///
/// Returns an error when the scan result cannot be written (see [`write_cache`]).
pub fn reindex(config: &Config, cache_dir: &Path, db_path: &Path) -> Result<Vec<String>> {
    let start = Instant::now();
    eprintln!("Scanning repositories to build cache...");

    let repo_strings = to_sorted_strings(scan_repos(config));
    write_cache(cache_dir, db_path, &repo_strings)?;

    eprintln!(
        "Cached {} repositories in {:.2?}.",
        repo_strings.len(),
        start.elapsed()
    );

    Ok(repo_strings)
}

/// The state of the on-disk cache; decides between serving from cache and
/// rebuilding in [`get_repos`].
enum CacheState {
    /// No cache file exists yet.
    Missing,
    /// The cache is older than the configured TTL.
    Stale,
    /// The cache exists but is not a valid archive for this version.
    Unreadable,
    /// The cache is current and was mapped successfully.
    Ready(Cache),
}

/// Classify the on-disk cache, mapping it when it looks usable.
fn cache_state(db_path: &Path, config: &Config) -> CacheState {
    if !db_path.exists() {
        return CacheState::Missing;
    }
    if is_stale(db_path, config) {
        return CacheState::Stale;
    }
    match Cache::open(db_path) {
        Ok(cache) => CacheState::Ready(cache),
        Err(_) => CacheState::Unreadable,
    }
}

/// Get the repository list as a [`Cache`] (borrowed paths, not copies).
///
/// - `no_cache` — always live-scan (`scan_repos`).
/// - otherwise the cache state decides: a missing, stale, or unreadable cache
///   is rebuilt via the write path; a current one is memmapped.
///
/// # Errors
///
/// Returns an error when a required rebuild fails (see [`reindex`]).
pub fn get_repos(
    config: &Config,
    cache_dir: &Path,
    db_path: &Path,
    no_cache: bool,
) -> Result<Cache> {
    if no_cache {
        return Ok(Cache::owned(to_sorted_strings(scan_repos(config))));
    }
    match cache_state(db_path, config) {
        CacheState::Ready(cache) => Ok(cache),
        CacheState::Missing => {
            reindex(config, cache_dir, db_path)?;
            Cache::open(db_path)
        }
        CacheState::Stale => {
            eprintln!("Cache is stale, rebuilding...");
            reindex(config, cache_dir, db_path)?;
            Cache::open(db_path)
        }
        CacheState::Unreadable => {
            eprintln!("Cache is unreadable, rebuilding...");
            reindex(config, cache_dir, db_path)?;
            Cache::open(db_path)
        }
    }
}

/// Search modes supported by [`search`]. All modes are case-insensitive.
///
/// Matching is done in memory over the borrowed repository list; the cache is
/// used purely as storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// Case-insensitive substring match over the full path.
    Substring,
    /// Case-insensitive prefix match over the basename.
    Prefix,
    /// Levenshtein (edit distance) match over the basename.
    Fuzzy,
    /// Subsequence match over the basename (e.g. `cfg` matches `config`).
    Subseq,
}

impl std::str::FromStr for SearchMode {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "substring" => Ok(Self::Substring),
            "prefix" => Ok(Self::Prefix),
            "fuzzy" => Ok(Self::Fuzzy),
            "subseq" | "subsequence" => Ok(Self::Subseq),
            other => Err(format!("unknown search mode: {other}")),
        }
    }
}

/// How closely a basename matches the query; used to rank results in the
/// non-fuzzy search modes (see [`SearchMode::score`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum BasenameRank {
    /// The basename equals the query.
    Exact,
    /// The basename starts with the query.
    Prefix,
    /// Any other match.
    Other,
}

impl From<BasenameRank> for u8 {
    fn from(rank: BasenameRank) -> Self {
        match rank {
            BasenameRank::Exact => 0,
            BasenameRank::Prefix => 1,
            BasenameRank::Other => 2,
        }
    }
}

impl SearchMode {
    /// The default number of edits allowed for [`SearchMode::Fuzzy`].
    pub const DEFAULT_FUZZY_DISTANCE: u32 = 2;

    /// Whether/how well `repo` matches `query` (already lowercased) under this
    /// mode. Returns `None` for a non-match, or a rank used to order results:
    /// for [`SearchMode::Fuzzy`] the rank is the edit distance; otherwise it
    /// is a [`BasenameRank`].
    fn score(self, repo: &str, query_lower: &str, distance: u32) -> Option<u8> {
        let base_lower = basename(repo).to_lowercase();
        match self {
            Self::Fuzzy => {
                let dist = edit_distance(&base_lower, query_lower);
                if dist > distance as usize {
                    return None;
                }
                return Some(u8::try_from(dist).unwrap_or(u8::MAX));
            }
            Self::Substring if !repo.to_lowercase().contains(query_lower) => return None,
            Self::Prefix if !base_lower.starts_with(query_lower) => return None,
            Self::Subseq if !is_subsequence(query_lower, &base_lower) => return None,
            _ => {}
        }
        let rank = match (base_lower == query_lower, base_lower.starts_with(query_lower)) {
            (true, _) => BasenameRank::Exact,
            (_, true) => BasenameRank::Prefix,
            _ => BasenameRank::Other,
        };
        Some(u8::from(rank))
    }
}

impl std::fmt::Display for SearchMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Substring => "substring",
            Self::Prefix => "prefix",
            Self::Fuzzy => "fuzzy",
            Self::Subseq => "subseq",
        };
        f.write_str(name)
    }
}

/// The last path component of `path`, or the whole string when there is none.
#[must_use]
pub fn basename(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
}

/// Edit distance between `a` and `b` (Levenshtein over Unicode scalar values).
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let m = b.len();

    let mut prev: Vec<usize> = (0..=m).collect();
    for (i, ac) in a.iter().enumerate() {
        let mut curr = vec![0usize; m + 1];
        curr[0] = i + 1;
        for (j, bc) in b.iter().enumerate() {
            let substitution = prev[j] + usize::from(ac != bc);
            curr[j + 1] = substitution.min(prev[j + 1] + 1).min(curr[j] + 1);
        }
        prev = curr;
    }
    prev[m]
}

/// Whether `needle` appears as a subsequence of `haystack`.
fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut it = haystack.chars();
    needle.chars().all(|c| it.any(|h| h == c))
}

/// Search an already-loaded repository list.
///
/// The core of [`search`] split out so callers that hold their own cached
/// [`Cache`] (notably the picker and the nvim FFI bridge) can search borrowed
/// paths without re-reading or re-allocating the list. Results are ranked
/// (fuzzy by edit distance; otherwise exact basename, basename prefix, then the
/// rest) and returned in sorted order.
#[must_use]
pub fn search_matches<'a>(
    repos: &[&'a str],
    query: &str,
    mode: SearchMode,
    distance: u32,
    limit: Option<usize>,
) -> Vec<&'a str> {
    let query_lower = query.to_lowercase();
    let mut matched: Vec<(u8, &'a str)> = repos
        .iter()
        .copied()
        .filter_map(|repo| mode.score(repo, &query_lower, distance).map(|score| (score, repo)))
        .collect();

    matched.sort_by_key(|item| item.0);
    matched
        .into_iter()
        .take(limit.unwrap_or(usize::MAX))
        .map(|(_, repo)| repo)
        .collect()
}

/// Search the cached repository list.
///
/// `distance` only applies to [`SearchMode::Fuzzy`]; `limit` caps the number
/// of returned results (`None` for unlimited).
///
/// This returns owned `String`s for callers that need to outlive the cache
/// (e.g. the nvim FFI bridge). Prefer [`search_matches`] with a borrowed
/// [`Cache::paths`] view when the cache stays in scope.
///
/// # Errors
///
/// Returns an error when the repository list cannot be loaded (see
/// [`get_repos`]).
pub fn search(
    config: &Config,
    cache_dir: &Path,
    db_path: &Path,
    query: &str,
    mode: SearchMode,
    distance: u32,
    limit: Option<usize>,
) -> Result<Vec<String>> {
    let cache = get_repos(config, cache_dir, db_path, false)?;
    Ok(search_matches(&cache.paths(), query, mode, distance, limit)
        .into_iter()
        .map(str::to_string)
        .collect())
}

/// Search the cached repository list for paths containing `query`
/// (case-insensitive substring; the default mode).
///
/// # Errors
///
/// Returns an error when the repository list cannot be loaded (see
/// [`get_repos`]).
pub fn search_repos(
    config: &Config,
    cache_dir: &Path,
    db_path: &Path,
    query: &str,
) -> Result<Vec<String>> {
    search(config, cache_dir, db_path, query, SearchMode::Substring, 0, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPOS: [&str; 6] = [
        "/home/u/Repos/config",
        "/home/u/Repos/config-manager",
        "/home/u/Repos/dotfiles",
        "/home/u/Repos/Dotfiles-Tools",
        "/home/u/Repos/pp",
        "/home/u/Repos/rust-pp",
    ];

    fn repos() -> Vec<&'static str> {
        REPOS.to_vec()
    }

    #[test]
    fn basename_returns_last_component() {
        assert_eq!(basename("/home/u/Repos/config"), "config");
        assert_eq!(basename("config"), "config");
    }

    #[test]
    fn substring_is_case_insensitive_over_full_path() {
        let results = search_matches(&repos(), "CONFIG", SearchMode::Substring, 0, None);
        assert_eq!(
            results,
            vec![
                "/home/u/Repos/config",
                "/home/u/Repos/config-manager",
            ]
        );
    }

    #[test]
    fn prefix_matches_basename_case_insensitively() {
        let results = search_matches(&repos(), "dot", SearchMode::Prefix, 0, None);
        assert_eq!(
            results,
            vec![
                "/home/u/Repos/dotfiles",
                "/home/u/Repos/Dotfiles-Tools",
            ]
        );
    }

    #[test]
    fn fuzzy_respects_distance() {
        let results = search_matches(&repos(), "pp", SearchMode::Fuzzy, 1, None);
        assert_eq!(results, vec!["/home/u/Repos/pp"]);
        assert_eq!(edit_distance("kitten", "sitten"), 1);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }

    #[test]
    fn fuzzy_ranks_by_edit_distance() {
        let results = search_matches(&repos(), "cfg", SearchMode::Fuzzy, 3, None);
        // "config" (3) and "pp" (3) tie on distance; stable sort keeps their
        // original relative order, so "config" comes first.
        assert_eq!(
            results,
            vec!["/home/u/Repos/config", "/home/u/Repos/pp"]
        );
    }

    #[test]
    fn subseq_matches_query_in_order() {
        let results = search_matches(&repos(), "cfg", SearchMode::Subseq, 0, None);
        assert_eq!(
            results,
            vec![
                "/home/u/Repos/config",
                "/home/u/Repos/config-manager",
            ]
        );
    }

    #[test]
    fn exact_basename_ranks_first() {
        let results = search_matches(&repos(), "pp", SearchMode::Substring, 0, None);
        assert_eq!(
            results,
            vec!["/home/u/Repos/pp", "/home/u/Repos/rust-pp"]
        );
    }

    #[test]
    fn limit_caps_results() {
        let results = search_matches(&repos(), "config", SearchMode::Substring, 0, Some(1));
        assert_eq!(results, vec!["/home/u/Repos/config"]);
    }

    #[test]
    fn empty_query_matches_everything_for_prefix() {
        let results = search_matches(&repos(), "", SearchMode::Prefix, 0, None);
        assert_eq!(results.len(), REPOS.len());
    }

    #[test]
    fn expand_path_anchors_relative_roots_to_home() {
        let home = Path::new("/home/u");
        assert_eq!(
            expand_path_with("~/Repos", home),
            PathBuf::from("/home/u/Repos")
        );
        assert_eq!(expand_path_with("~", home), PathBuf::from("/home/u"));
        // A relative root must not depend on the indexing process's cwd.
        assert_eq!(expand_path_with("code", home), PathBuf::from("/home/u/code"));
        assert_eq!(
            expand_path_with("/abs/path", home),
            PathBuf::from("/abs/path")
        );
    }

    /// Symlink fixture: `real-repo` (has .git) plus `link-repo` pointing at it.
    /// `tag` keeps parallel tests from sharing one temp dir.
    fn symlink_fixture(tag: &str) -> (PathBuf, Config) {
        let tmp =
            std::env::temp_dir().join(format!("pp-scan-symlink-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let real = tmp.join("real-repo");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join(".git"), "").unwrap();
        // The user scenario: a repository reached through a symlinked
        // directory inside a scanned root.
        std::os::unix::fs::symlink(&real, tmp.join("link-repo")).unwrap();
        (tmp, Config::default())
    }

    #[test]
    #[cfg(unix)]
    fn scan_skips_symlinks_by_default() {
        let (tmp, config) = symlink_fixture("skip");
        let mut found = Vec::new();
        scan_directory(
            &tmp,
            &mut found,
            &mut Vec::new(),
            &config,
            &[],
        );
        found.sort();

        let names: Vec<&str> = found.iter().filter_map(|p| p.file_name()?.to_str()).collect();
        assert_eq!(names, vec!["real-repo"]);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    #[cfg(unix)]
    fn scan_follows_symlinked_directories_when_enabled() {
        let (tmp, mut config) = symlink_fixture("follow");
        config.follow_symlinks = true;
        let mut found = Vec::new();
        scan_directory(
            &tmp,
            &mut found,
            &mut Vec::new(),
            &config,
            &[],
        );
        found.sort();

        let names: Vec<&str> = found.iter().filter_map(|p| p.file_name()?.to_str()).collect();
        assert_eq!(names, vec!["link-repo", "real-repo"]);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A directory that is itself a repository must be returned as a whole,
    /// and none of its children may appear in the results regardless of
    /// readdir order — the hit on a project marker has to discard
    /// (truncate) any children already pushed onto the pending stack.
    #[test]
    #[cfg(unix)]
    fn scan_marker_prunes_children_pushed_before_it() {
        let tmp = std::env::temp_dir().join(format!("pp-scan-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let parent = tmp.join("parent");
        std::fs::create_dir_all(parent.join("child-a/deeper")).unwrap();
        std::fs::create_dir_all(parent.join("child-b")).unwrap();
        // The project marker is a plain entry; readdir order decides
        // whether the children are pushed onto the pending stack before it
        // is seen.
        std::fs::write(parent.join(".git"), "").unwrap();
        // A sibling repo to prove the scan continues past the pruned subtree.
        let sibling = tmp.join("sibling");
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::write(sibling.join("Cargo.toml"), "").unwrap();

        let mut found = Vec::new();
        let config = Config {
            roots: vec![tmp.to_string_lossy().into_owned()],
            ..Config::default()
        };
        scan_directory(
            Path::new(&config.roots[0]),
            &mut found,
            &mut Vec::new(),
            &config,
            &[],
        );
        found.sort();

        let names: Vec<&str> = found.iter().filter_map(|p| p.file_name()?.to_str()).collect();
        assert_eq!(names, vec!["parent", "sibling"]);
        // No child of `parent` was ever emitted.
        assert!(
            found
                .iter()
                .all(|p| p == &parent || !p.starts_with(&parent))
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Inside a `nested_projects` subtree, a project-marker hit emits the
    /// directory but does NOT prune it, so nested repositories are found at
    /// any depth. Outside
    /// the subtree, repos stay leaves as before. Driven through `scan_repos`
    /// to also cover prefix expansion.
    #[test]
    #[cfg(unix)]
    fn scan_nested_projects_descends_inside_subtree() {
        let tmp = std::env::temp_dir().join(format!("pp-scan-nested-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let repo = tmp.join("repo");
        std::fs::create_dir_all(repo.join("inner")).unwrap();
        std::fs::create_dir_all(repo.join("plain/leaf")).unwrap();
        std::fs::write(repo.join(".git"), "").unwrap();
        std::fs::write(repo.join("inner/Cargo.toml"), "").unwrap();
        std::fs::write(repo.join("plain/leaf/package.json"), "").unwrap();
        // A repo outside the nested subtree keeps leaf semantics.
        let sibling = tmp.join("sibling");
        std::fs::create_dir_all(sibling.join("child")).unwrap();
        std::fs::write(sibling.join(".git"), "").unwrap();
        std::fs::write(sibling.join("child/go.mod"), "").unwrap();

        let config = Config {
            roots: vec![tmp.to_string_lossy().into_owned()],
            nested_projects: vec![repo.to_string_lossy().into_owned()],
            ..Config::default()
        };
        let mut found = scan_repos(&config);
        found.sort();

        let expected: Vec<PathBuf> = [
            repo.clone(),
            repo.join("inner"),
            repo.join("plain/leaf"),
            sibling,
        ]
        .to_vec();
        assert_eq!(found, expected);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A prefix deeper than an enclosing repo still works: the repo is an
    /// ancestor of the listed subtree, so the walk descends *past* its
    /// project marker (ancestor rule). Sibling repos off the path still
    /// prune.
    #[test]
    #[cfg(unix)]
    fn scan_nested_projects_reaches_through_repo_ancestor() {
        let tmp = std::env::temp_dir().join(format!("pp-scan-nested-anc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let repo = tmp.join("repo");
        let target = repo.join("deep/nested-target");
        std::fs::create_dir_all(repo.join("deep/nested-other/child")).unwrap();
        std::fs::create_dir_all(target.join("sub")).unwrap();
        std::fs::create_dir_all(repo.join("side/x")).unwrap();
        std::fs::write(repo.join(".git"), "").unwrap();
        std::fs::write(target.join("Cargo.toml"), "").unwrap();
        std::fs::write(target.join("sub/.git"), "").unwrap();
        std::fs::write(repo.join("deep/nested-other/package.json"), "").unwrap();
        std::fs::write(repo.join("deep/nested-other/child/.git"), "").unwrap();
        std::fs::write(repo.join("side/go.mod"), "").unwrap();
        std::fs::write(repo.join("side/x/Cargo.toml"), "").unwrap();

        let config = Config {
            roots: vec![tmp.to_string_lossy().into_owned()],
            nested_projects: vec![target.to_string_lossy().into_owned()],
            ..Config::default()
        };
        let mut found = scan_repos(&config);
        found.sort();

        let expected: Vec<PathBuf> = [
            repo.clone(),
            // Repos not covered by the prefix: emitted, but pruned as leaves.
            repo.join("deep/nested-other"),
            // Inside the listed subtree (and at the prefix itself): no pruning.
            repo.join("deep/nested-target"),
            target.join("sub"),
            repo.join("side"),
        ]
        .to_vec();
        assert_eq!(found, expected);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn cache_round_trips_through_rkyv() {
        let dir = std::env::temp_dir().join(format!("pp-cache-roundtrip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cache_dir = dir.join("cache");
        let db = cache_dir.join("pp.cache");
        let owned: Vec<String> = REPOS.iter().map(|s| (*s).to_string()).collect();

        write_cache(&cache_dir, &db, &owned).unwrap();
        let cache = Cache::open(&db).unwrap();

        let mut expected = owned.clone();
        expected.sort();
        assert_eq!(
            cache.paths(),
            expected.iter().map(String::as_str).collect::<Vec<_>>()
        );
        assert!(!cache.is_empty());

        // A current cache is served as-is by get_repos (no live scan needed),
        // even when the configured roots are empty.
        let config = Config {
            roots: Vec::new(),
            ..Config::default()
        };
        let served = get_repos(&config, &cache_dir, &db, false).unwrap();
        assert_eq!(served.len(), REPOS.len());

        // Corrupt bytes are rejected instead of being read as garbage.
        std::fs::write(&db, b"not an rkyv archive").unwrap();
        assert!(Cache::open(&db).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}

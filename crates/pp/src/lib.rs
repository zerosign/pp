//! `pp` — shared core for the pp repository locator.
//!
//! This crate holds only the *index & search* logic: config loading, root
//! scanning, and the cache. The interactive UI (skim picker) lives in
//! `pp-cli`, and nvim-facing wrappers live in `pp-nvim`.
//!
//! # Index format
//!
//! The cache is an immutable [`fst::Set`] of repository paths, built from
//! sorted input and swapped into place atomically (temp file + rename). It is
//! memmapped-free by design: the file is tiny, so reads just load it into
//! memory. Because the set is immutable, any number of readers (CLI + nvim
//! simultaneously) can access it without coordination, and a rebuild never
//! disturbs an in-flight read.
//!
//! # Access modes
//!
//! - [`reindex`] — write path: scan roots and persist the index.
//! - [`read_index`] — read path: load the cached set.
//! - [`get_repos`] — convenience accessor that auto-rebuilds a missing,
//!   stale, or unreadable index.

use fst::{Set, SetBuilder};
use rayon::prelude::*;
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

/// Resolve the cache directory and the index database path.
///
/// # Errors
///
/// Returns an error when the cache directory cannot be determined from the
/// environment.
pub fn cache_paths() -> Result<(PathBuf, PathBuf)> {
    let cache_dir = get_cache_path().ok_or("Could not determine cache directory")?;
    Ok((cache_dir.clone(), cache_dir.join("index.fst")))
}

fn expand_path(path: &str) -> PathBuf {
    path.strip_prefix("~/")
        .or_else(|| (path == "~").then_some(""))
        .and_then(|rest| directories::UserDirs::new().map(|dirs| dirs.home_dir().join(rest)))
        .unwrap_or_else(|| PathBuf::from(path))
}

fn scan_directory(
    path: &Path,
    ignored_dirs: &[String],
    project_markers: &[String],
    depth: usize,
    max_depth: usize,
) -> Vec<PathBuf> {
    if depth >= max_depth {
        return vec![];
    }
    if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| ignored_dirs.iter().any(|ignored| ignored == name))
    {
        return vec![];
    }

    let Ok(read_dir) = fs::read_dir(path) else {
        return vec![];
    };

    let mut subdirs = Vec::new();
    let mut has_marker = false;
    for entry in read_dir.filter_map(std::result::Result::ok) {
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if project_markers.iter().any(|marker| marker == &*name) {
            has_marker = true;
            break;
        }
        if entry
            .file_type()
            .is_ok_and(|file_type| file_type.is_dir())
            && !ignored_dirs.iter().any(|ignored| ignored == &*name)
        {
            subdirs.push(entry.path());
        }
    }

    if has_marker {
        return vec![path.to_path_buf()];
    }

    subdirs
        .into_par_iter()
        .flat_map(|entry| scan_directory(&entry, ignored_dirs, project_markers, depth + 1, max_depth))
        .collect()
}

/// Live-scan all configured roots for repositories (no cache involved).
#[must_use]
pub fn scan_repos(config: &Config) -> Vec<PathBuf> {
    config
        .roots
        .iter()
        .map(|root| expand_path(root))
        .filter(|root| root.exists())
        .flat_map(|root| scan_directory(&root, &config.ignored_dirs, &config.project_markers, 0, config.max_depth))
        .collect()
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

/// Write path: scan roots and persist the index as an immutable [`fst::Set`].
///
/// Returns the sorted list of indexed repositories. The set is written to a
/// per-process temp file and atomically renamed into place, so concurrent
/// readers keep seeing the previous index until the swap. A stale `index.db`
/// left over from the old redb-backed format is removed on success.
///
/// # Errors
///
/// Returns an error when the cache directory cannot be created, the fst cannot
/// be built, or the temp file cannot be written or renamed into place.
pub fn reindex(config: &Config, cache_dir: &Path, db_path: &Path) -> Result<Vec<String>> {
    let start = Instant::now();
    eprintln!("Scanning repositories to build index...");

    let repo_strings = to_sorted_strings(scan_repos(config));
    fs::create_dir_all(cache_dir)?;

    let file_name = db_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("index.fst");
    let tmp_path = db_path.with_file_name(format!("{file_name}.{}.tmp", std::process::id()));

    let mut builder = SetBuilder::new(Vec::<u8>::new())?;
    for repo in &repo_strings {
        builder.insert(repo)?;
    }
    fs::write(&tmp_path, builder.into_inner()?)?;
    fs::rename(&tmp_path, db_path)?;

    // Clean up the legacy redb-format cache if it is still around.
    let legacy_db = db_path.with_file_name("index.db");
    if legacy_db.exists() {
        let _ = fs::remove_file(legacy_db);
    }

    eprintln!(
        "Indexed {} repositories in {:.2?}.",
        repo_strings.len(),
        start.elapsed()
    );
    Ok(repo_strings)
}

/// Read path: load the cached repository set (already sorted & de-duplicated).
///
/// # Errors
///
/// Returns an error when the index file cannot be read or parsed as an fst.
pub fn read_index(db_path: &Path) -> Result<Vec<String>> {
    let set = Set::new(fs::read(db_path)?)?;
    Ok(set.stream().into_strs()?)
}

/// The state of the on-disk index; decides between serving from cache and
/// rebuilding in [`get_repos`].
enum IndexState {
    /// No index file exists yet.
    Missing,
    /// The index is older than the configured TTL.
    Stale,
    /// The index exists but cannot be parsed.
    Unreadable,
    /// The index is current and was loaded successfully.
    Ready(Vec<String>),
}

/// Classify the on-disk index, loading it when it looks usable.
fn index_state(db_path: &Path, config: &Config) -> IndexState {
    if !db_path.exists() {
        return IndexState::Missing;
    }
    if is_stale(db_path, config) {
        return IndexState::Stale;
    }
    match read_index(db_path) {
        Ok(repos) => IndexState::Ready(repos),
        Err(_) => IndexState::Unreadable,
    }
}

/// Get the sorted repository list.
///
/// - `no_cache` — always live-scan (`scan_repos`).
/// - otherwise the index state decides: a missing, stale, or unreadable index
///   is rebuilt via the write path; a current one is served from cache.
///
/// # Errors
///
/// Returns an error when a required rebuild fails (see [`reindex`]).
pub fn get_repos(
    config: &Config,
    cache_dir: &Path,
    db_path: &Path,
    no_cache: bool,
) -> Result<Vec<String>> {
    if no_cache {
        return Ok(to_sorted_strings(scan_repos(config)));
    }
    match index_state(db_path, config) {
        IndexState::Missing => reindex(config, cache_dir, db_path),
        IndexState::Stale => {
            eprintln!("Index is stale, rebuilding...");
            reindex(config, cache_dir, db_path)
        }
        IndexState::Unreadable => {
            eprintln!("Index is unreadable, rebuilding...");
            reindex(config, cache_dir, db_path)
        }
        IndexState::Ready(repos) => Ok(repos),
    }
}

/// Search modes supported by [`search`]. All modes are case-insensitive.
///
/// Matching is done in memory over the loaded repository list; the fst cache
/// is used purely as storage.
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
/// The core of [`search`] split out so callers that hold their own cached list
/// (notably the nvim FFI bridge) can search without re-reading the index.
/// Results are ranked (fuzzy by edit distance; otherwise exact basename,
/// basename prefix, then the rest) and returned in sorted order.
pub fn search_matches(
    repos: &[String],
    query: &str,
    mode: SearchMode,
    distance: u32,
    limit: Option<usize>,
) -> Vec<String> {
    let query_lower = query.to_lowercase();
    let mut matched: Vec<(u8, &str)> = repos
        .iter()
        .map(String::as_str)
        .filter_map(|repo| mode.score(repo, &query_lower, distance).map(|score| (score, repo)))
        .collect();
    matched.sort_by_key(|item| item.0);
    matched
        .into_iter()
        .take(limit.unwrap_or(usize::MAX))
        .map(|(_, repo)| repo.to_string())
        .collect()
}

/// Search the cached repository list.
///
/// `distance` only applies to [`SearchMode::Fuzzy`]; `limit` caps the number
/// of returned results (`None` for unlimited).
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
    Ok(search_matches(
        &get_repos(config, cache_dir, db_path, false)?,
        query,
        mode,
        distance,
        limit,
    ))
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

    fn repos() -> Vec<String> {
        REPOS.iter().map(|s| (*s).to_string()).collect()
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
}

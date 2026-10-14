//! `pp` — interactive repository picker (console + fuzzy-matcher) and CLI
//! subcommands.
//!
//! UI and IO live here; all cache/search logic is delegated to the `pp` lib
//! crate. Argument parsing uses `usage-rs` derives: the parser, help pages,
//! shell-completion scripts, and the emitted usage spec all come from the
//! same declaration.

use console::{Key, Term, style};
use fuzzy_matcher::{FuzzyMatcher, skim::SkimMatcherV2};
use std::io::{self, Write};
use usage::{Args, Cli, Subcommands, ValueEnum};

/// Fast repository locator
#[derive(Cli)]
#[usage(bin = "pp", version, about = "Fast repository locator", completion)]
struct Pp {
    /// Force a fresh scan, bypassing the cache
    #[usage(short, long, global)]
    no_cache: bool,

    #[usage(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommands)]
enum Command {
    /// Rebuild the repository cache
    Index,
    /// Print the plain list of repositories to stdout
    List,
    /// Search the repository cache
    Search(Search),
    /// Clear the repository cache
    Clear,
    /// Generate autocomplete script for specified shell
    Generate(Generate),
}

#[derive(Args)]
struct Search {
    /// Search mode
    #[usage(long, value_enum, default = "substring")]
    mode: SearchModeArg,
    /// Maximum edit distance for fuzzy search (fuzzy mode only)
    #[usage(long, default = "2")]
    distance: u32,
    /// Maximum number of results to print
    #[usage(long)]
    limit: Option<usize>,
    /// Search query
    query: String,
}

/// CLI spelling of [`pp::SearchMode`] (value enums cannot live in the lib).
#[derive(ValueEnum, Clone, Copy, Debug)]
enum SearchModeArg {
    Substring,
    Prefix,
    Fuzzy,
    Subseq,
}

impl From<SearchModeArg> for pp::SearchMode {
    fn from(mode: SearchModeArg) -> Self {
        match mode {
            SearchModeArg::Substring => Self::Substring,
            SearchModeArg::Prefix => Self::Prefix,
            SearchModeArg::Fuzzy => Self::Fuzzy,
            SearchModeArg::Subseq => Self::Subseq,
        }
    }
}

#[derive(Args)]
struct Generate {
    /// Shell to generate completions for
    #[usage(value_enum)]
    shell: Shell,
}

/// Shells with installable completion scripts.
#[derive(ValueEnum, Clone, Copy, Debug)]
enum Shell {
    Bash,
    Elvish,
    Fish,
    /// PowerShell — word stays "powershell" despite the variant name.
    #[usage(name = "powershell")]
    Pwsh,
    Zsh,
}

impl From<Shell> for usage::complete::Shell {
    fn from(shell: Shell) -> Self {
        match shell {
            Shell::Bash => Self::Bash,
            Shell::Elvish => Self::Elvish,
            Shell::Fish => Self::Fish,
            Shell::Pwsh => Self::PowerShell,
            Shell::Zsh => Self::Zsh,
        }
    }
}

/// The ranking key for `query` against `path`, or `None` when nothing matches.
///
/// Every component of `path` is matched independently, so `something` finds
/// `.../something/test/test` while no query can match *across* a separator —
/// which is what keeps a shared parent such as `.../Repositories` from flooding
/// the list. A query that itself contains `/` is matched against the whole
/// path, because there the separator is part of what the user typed.
///
/// The key orders matches: best component score first, then the component
/// closest to the basename (so a repo named `test` outranks a repo whose parent
/// directory is named `test` when both score equally).
fn score_path(matcher: &SkimMatcherV2, path: &str, query: &str) -> Option<(i64, usize)> {
    if query.contains('/') {
        return matcher.fuzzy_match(path, query).map(|score| (score, 0));
    }

    let components = path.split('/').filter(|part| !part.is_empty());
    let count = components.clone().count();
    components
        .enumerate()
        .filter_map(|(index, part)| {
            matcher
                .fuzzy_match(part, query)
                .map(|score| (score, count - 1 - index))
        })
        .max_by_key(|&(score, depth)| (score, std::cmp::Reverse(depth)))
}

/// Interactive fuzzy repository selector over borrowed paths.
///
/// Hand-rolled on `console` + `fuzzy-matcher` (rather than `dialoguer`) so the
/// candidate strings are borrowed straight out of the [`pp::Cache`] mapping:
/// nothing is cloned and the returned index maps exactly back into `items`.
/// Matching runs per path component (see [`score_path`]) so a query can name
/// any directory along the way, not just the repo's own name.
fn pick_repo(items: &[&str]) -> io::Result<Option<usize>> {
    let term = Term::stderr();
    let matcher = SkimMatcherV2::default();
    let mut query = String::new();
    let mut selected = 0usize;
    let mut filtered: Vec<(usize, (i64, usize))> = Vec::with_capacity(items.len());
    let mut drawn = 0usize;

    term.hide_cursor()?;
    let result = loop {
        filtered.clear();
        if query.is_empty() {
            filtered.extend((0..items.len()).map(|index| (index, (0, 0))));
        } else {
            filtered.extend(items.iter().enumerate().filter_map(|(index, item)| {
                score_path(&matcher, item, &query).map(|key| (index, key))
            }));
            filtered.sort_unstable_by_key(|&(index, (score, depth))| {
                (std::cmp::Reverse(score), depth, index)
            });
        }

        if filtered.is_empty() {
            selected = 0;
        } else if selected >= filtered.len() {
            selected = filtered.len() - 1;
        }

        // One row is the prompt, one is kept free for the final newline.
        let rows = (term.size().0 as usize).saturating_sub(2).max(1);
        let start = selected.saturating_sub(rows - 1);
        let end = (start + rows).min(filtered.len());

        if drawn > 0 {
            term.clear_last_lines(drawn)?;
        }
        term.write_line(&format!(
            "{} repo {}",
            style("?").green(),
            style(&query).cyan()
        ))?;
        drawn = 1;
        for (offset, &(index, _)) in filtered[start..end].iter().enumerate() {
            let marker = if start + offset == selected {
                style("❯").cyan()
            } else {
                style(" ")
            };
            term.write_line(&format!("{marker} {}", items[index]))?;
            drawn += 1;
        }
        term.flush()?;

        match term.read_key()? {
            Key::Enter if !filtered.is_empty() => break Some(filtered[selected].0),
            Key::Escape => break None,
            Key::ArrowUp if !filtered.is_empty() => {
                selected = selected.saturating_sub(1);
            }
            Key::ArrowDown if !filtered.is_empty() => {
                selected = (selected + 1).min(filtered.len() - 1);
            }
            Key::Backspace => {
                query.pop();
                selected = 0;
            }
            Key::Char(ch) if !ch.is_control() => {
                query.push(ch);
                selected = 0;
            }
            _ => {}
        }
    };

    if drawn > 0 {
        term.clear_last_lines(drawn)?;
    }
    term.show_cursor()?;
    Ok(result)
}

fn print_lines<'a>(lines: impl Iterator<Item = &'a str>) -> io::Result<()> {
    let stdout = io::stdout();
    let mut handle = io::BufWriter::new(stdout.lock());

    for line in lines {
        writeln!(handle, "{line}")?;
    }

    handle.flush()
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    // `parse()` renders help/version and clap-shaped diagnostics itself,
    // exiting the way clap does (2 on a parse failure).
    let pp = Pp::parse();

    match pp.command {
        Some(Command::Generate(generate)) => {
            let script = Pp::completion_script(generate.shell.into());
            io::stdout().write_all(script.as_bytes())?;
        }
        Some(Command::Clear) => {
            let (_, db_path) = pp::cache_paths()?;

            if db_path.exists() {
                std::fs::remove_file(&db_path)?;
                eprintln!("Cache cleared.");
            }
        }
        Some(Command::Index) => {
            let config = pp::load_config();
            let (cache_dir, db_path) = pp::cache_paths()?;

            pp::reindex(&config, &cache_dir, &db_path)?;
        }
        Some(Command::List) => {
            let config = pp::load_config();
            let (cache_dir, db_path) = pp::cache_paths()?;
            let cache = pp::get_repos(&config, &cache_dir, &db_path, pp.no_cache)?;

            print_lines(cache.paths().into_iter())?;
        }
        Some(Command::Search(search)) => {
            let config = pp::load_config();
            let (cache_dir, db_path) = pp::cache_paths()?;
            let cache = pp::get_repos(&config, &cache_dir, &db_path, pp.no_cache)?;

            let paths = cache.paths();
            let results = pp::search_matches(
                &paths,
                &search.query,
                search.mode.into(),
                search.distance,
                search.limit,
            );

            print_lines(results.into_iter())?;
        }
        None => {
            let config = pp::load_config();
            let (cache_dir, db_path) = pp::cache_paths()?;
            let cache = pp::get_repos(&config, &cache_dir, &db_path, pp.no_cache)?;

            if cache.is_empty() {
                eprintln!("No repositories found. Run `pp index` to build the cache.");
                return Ok(());
            }

            let items = cache.paths();
            if let Some(index) = pick_repo(&items)? {
                println!("{}", items[index]);
            }
        }
    }

    Ok(())
}

fn main() {
    if let Err(err) = run() {
        // Piping to `head`/`tail` closes stdout early; that is not an error.
        if err
            .downcast_ref::<io::Error>()
            .is_some_and(|io_err| io_err.kind() == io::ErrorKind::BrokenPipe)
        {
            return;
        }

        eprintln!("Error: {err}");

        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Score `query` against `path` with the picker's matcher.
    fn score(path: &str, query: &str) -> Option<(i64, usize)> {
        score_path(&SkimMatcherV2::default(), path, query)
    }

    #[test]
    fn matches_a_middle_path_component() {
        // The repo sits at /*/something/*/* — "something" is neither the root
        // nor the basename, and must still be reachable.
        let path = "/home/user/something/test/test";
        assert!(score(path, "some").is_some());
        assert!(score(path, "something").is_some());
        assert!(score(path, "test").is_some());
        assert!(score(path, "nope").is_none());
    }

    #[test]
    fn matches_incrementally() {
        let path = "/home/user/something/test/test";
        for query in [
            "s",
            "so",
            "som",
            "some",
            "somet",
            "someth",
            "somethi",
            "somethin",
            "something",
        ] {
            assert!(
                score(path, query).is_some(),
                "{query:?} should keep matching {path}"
            );
        }
        assert!(score(path, "somethingelse").is_none());
    }

    #[test]
    fn a_shared_prefix_cannot_match_across_separators() {
        let path = "/home/user/Repositories/alpha-store";
        let matcher = SkimMatcherV2::default();
        // A single string would happily match `ster` by hopping across `/` —
        // that is the flood the component split prevents.
        assert!(matcher.fuzzy_match(path, "ster").is_some());
        assert!(score(path, "ster").is_none());
    }

    #[test]
    fn basename_wins_a_score_tie() {
        let repo = score("/home/user/test", "test").unwrap();
        let parent = score("/home/user/test/api", "test").unwrap();
        assert_eq!(repo.0, parent.0);
        assert!(
            (std::cmp::Reverse(repo.0), repo.1) < (std::cmp::Reverse(parent.0), parent.1),
            "the repo named `test` ranks ahead of the repo *under* `test`"
        );
    }

    #[test]
    fn a_query_with_a_slash_matches_the_whole_path() {
        let path = "/home/user/something/test/test";
        assert!(score(path, "something/test").is_some());
        assert!(score(path, "user/something").is_some());
        assert!(score(path, "nope/test").is_none());
    }
}

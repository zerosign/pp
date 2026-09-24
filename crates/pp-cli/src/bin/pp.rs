//! `pp` — interactive repository picker (dialoguer) and CLI subcommands.
//!
//! UI and IO live here; all index/search logic is delegated to the `pp` lib
//! crate. Argument parsing uses `usage-rs` derives: the parser, help pages,
//! shell-completion scripts, and the emitted usage spec all come from the
//! same declaration.

use std::io::{self, Write};
use usage::{Args, Cli, Subcommands, ValueEnum};

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

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
    /// Rebuild the repository index
    Index,
    /// Print the plain list of repositories to stdout
    List,
    /// Search the repository index
    Search(Search),
    /// Clear the index database
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

/// Interactive fuzzy repository selector (`dialoguer`).
fn pick_repo(repos: &[String]) -> Option<String> {
    use dialoguer::{theme::ColorfulTheme, FuzzySelect};

    let selection = FuzzySelect::with_theme(&ColorfulTheme::default())
        .with_prompt("repo")
        .items(repos)
        .default(0)
        .interact_opt()
        .ok()??;

    repos.get(selection).cloned()
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
                eprintln!("Index cleared.");
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
            let repos = pp::get_repos(&config, &cache_dir, &db_path, pp.no_cache)?;

            print_lines(repos.iter().map(String::as_str))?;
        }
        Some(Command::Search(search)) => {
            let config = pp::load_config();
            let (cache_dir, db_path) = pp::cache_paths()?;

            let repos = pp::search(
                &config,
                &cache_dir,
                &db_path,
                &search.query,
                search.mode.into(),
                search.distance,
                search.limit,
            )?;

            print_lines(repos.iter().map(String::as_str))?;
        }
        None => {
            let config = pp::load_config();
            let (cache_dir, db_path) = pp::cache_paths()?;
            let repos = pp::get_repos(&config, &cache_dir, &db_path, pp.no_cache)?;

            if repos.is_empty() {
                eprintln!("No repositories found. Run `pp index` to build the index.");
                return Ok(());
            }

            if let Some(selected) = pick_repo(&repos) {
                println!("{selected}");
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

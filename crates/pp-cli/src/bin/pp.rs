//! `pp` — interactive repository picker (skim) and CLI subcommands.
//!
//! UI and IO live here; all index/search logic is delegated to the `pp` lib
//! crate.

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use std::io::{self, Write};

#[derive(Parser, Debug)]
#[command(author, version, about = "Fast repository locator", long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,

    /// Force a fresh scan, bypassing the cache
    #[arg(short, long, global = true)]
    no_cache: bool,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Rebuild the repository index
    Index,
    /// Print the plain list of repositories to stdout
    List,
    /// Search the repository index
    Search {
        /// Search mode
        #[arg(long, value_enum, default_value = "substring")]
        mode: SearchModeArg,
        /// Maximum edit distance for fuzzy search (fuzzy mode only)
        #[arg(long, default_value_t = pp::SearchMode::DEFAULT_FUZZY_DISTANCE)]
        distance: u32,
        /// Maximum number of results to print
        #[arg(long)]
        limit: Option<usize>,
        /// Search query
        query: String,
    },
    /// Clear the index database
    Clear,
    /// Generate autocomplete script for specified shell
    Generate {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: Shell,
    },
}

/// CLI spelling of [`pp::SearchMode`] (clap value enums cannot live in the lib).
#[derive(clap::ValueEnum, Clone, Copy, Debug)]
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

/// The interactive fuzzy picker (skim). Renders on stderr so it composes with
/// command substitution; the accepted selection is printed on stdout.
fn pick_repo(repos: Vec<String>) -> Option<String> {
    use skim::prelude::*;

    let options = SkimOptionsBuilder::default()
        .height("60%")
        .multi(false)
        .prompt("repo> ")
        .build()
        .ok()?;

    let output = Skim::run_items(options, repos).ok()?;
    
    if output.is_abort {
        return None;
    }

    output
        .selected_items
        .first()
        .map(|item| item.item.output().to_string())
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
    let args = Args::parse();

    match args.command {
        Some(Command::Generate { shell }) => {
            let mut cmd = Args::command();
            
            clap_complete::generate(shell, &mut cmd, "pp", &mut io::stdout());
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
            let repos = pp::get_repos(&config, &cache_dir, &db_path, args.no_cache)?;
            
            print_lines(repos.iter().map(String::as_str))?;
        }
        Some(Command::Search {
            mode,
            distance,
            limit,
            query,
        }) => {
            let config = pp::load_config();
            let (cache_dir, db_path) = pp::cache_paths()?;
            
            let repos = pp::search(
                &config,
                &cache_dir,
                &db_path,
                &query,
                mode.into(),
                distance,
                limit,
            )?;

            print_lines(repos.iter().map(String::as_str))?;
        }
        None => {
            let config = pp::load_config();
            let (cache_dir, db_path) = pp::cache_paths()?;
            let repos = pp::get_repos(&config, &cache_dir, &db_path, args.no_cache)?;
            
            if repos.is_empty() {
                eprintln!("No repositories found. Run `pp index` to build the index.");
                return Ok(());
            }

            if let Some(selected) = pick_repo(repos) {
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

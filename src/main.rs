use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "codemap",
    version,
    about = "Code intelligence for JS/TS codebases"
)]
#[command(propagate_version = true)]
struct Cli {
    /// Project root directory
    #[arg(short, long, default_value = ".")]
    root: PathBuf,

    /// Verbosity (-v, -vv, -vvv)
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Index the codebase: parse files, build graph, compute ranks
    Index(IndexArgs),

    /// Generate a code map for the top-ranked files
    Map {
        /// Maximum tokens in output
        #[arg(short, long, default_value_t = 1500)]
        tokens: usize,

        /// Suppress the CLI instructions footer
        #[arg(long)]
        no_instructions: bool,
    },

    /// Find where a symbol is defined and who uses it
    Symbol {
        /// Symbol name or pattern
        #[arg()]
        pattern: String,

        /// Max results
        #[arg(short, long, default_value_t = 10)]
        limit: usize,

        /// Show all references without truncation
        #[arg(long)]
        all: bool,

        /// Exact name match only (default is substring)
        #[arg(long)]
        exact: bool,

        /// JSON output
        #[arg(long)]
        json: bool,
    },

    /// Suggest the most relevant files for a task
    Context {
        /// Natural language task description
        #[arg()]
        query: String,

        /// Max results
        #[arg(short, long, default_value_t = 10)]
        limit: usize,

        /// JSON output
        #[arg(long)]
        json: bool,

        /// Also print file contents
        #[arg(long)]
        include_content: bool,
    },

    /// Configure Claude Code hooks (the only setup step needed)
    Setup {
        /// Skip the PostToolUse re-indexing hook
        #[arg(long)]
        no_post_hook: bool,

        /// Write to ~/.claude/settings.json instead of project-local
        #[arg(long)]
        global: bool,

        /// Print what would be written without writing
        #[arg(long)]
        dry_run: bool,
    },

    /// Show imports and importers of a file
    Deps {
        /// File path to inspect
        #[arg()]
        file: String,

        /// Direction: imports, importers, or both
        #[arg(short, long, default_value = "both")]
        direction: String,

        /// Traversal depth
        #[arg(long, default_value_t = 1)]
        depth: usize,

        /// Show all importers without truncation
        #[arg(long)]
        all: bool,

        /// JSON output
        #[arg(long)]
        json: bool,
    },
}

#[derive(Args, Debug)]
struct IndexArgs {
    /// Force full re-index (ignore cache)
    #[arg(long)]
    force: bool,

    /// Incremental: only re-index files with newer mtime
    #[arg(long)]
    incremental: bool,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_max_level(match cli.verbose {
            0 => tracing::Level::WARN,
            1 => tracing::Level::INFO,
            2 => tracing::Level::DEBUG,
            _ => tracing::Level::TRACE,
        })
        .with_writer(std::io::stderr)
        .init();

    let root = std::fs::canonicalize(&cli.root)?;

    match cli.command {
        Commands::Index(args) => {
            tracing::info!(root = %root.display(), force = args.force, incremental = args.incremental, "Indexing codebase");
            codemap::index::run_index(&root, args.force, args.incremental)?;
        }
        Commands::Map {
            tokens,
            no_instructions,
        } => {
            tracing::info!(tokens, no_instructions, "Generating code map");
            codemap::map::run_map(&root, tokens, no_instructions)?;
        }
        Commands::Context {
            query,
            limit,
            json,
            include_content,
        } => {
            tracing::info!(
                query,
                limit,
                json,
                include_content,
                "Finding relevant files"
            );
            codemap::context::run_context(&root, &query, limit, json, include_content)?;
        }
        Commands::Symbol {
            pattern,
            limit,
            all,
            exact,
            json,
        } => {
            tracing::info!(pattern, limit, all, exact, json, "Looking up symbol");
            codemap::symbol::run_symbol(&root, &pattern, limit, all, exact, json)?;
        }
        Commands::Setup {
            no_post_hook,
            global,
            dry_run,
        } => {
            tracing::info!(no_post_hook, global, dry_run, "Setting up codemap");
            codemap::setup::run_setup(&root, no_post_hook, global, dry_run)?;
        }
        Commands::Deps {
            file,
            direction,
            depth,
            all,
            json,
        } => {
            tracing::info!(file, direction, depth, all, json, "Inspecting deps");
            codemap::deps::run_deps(&root, &file, &direction, depth, all, json)?;
        }
    }

    Ok(())
}

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

    /// Enrich file metadata with LLM-generated summaries
    Enrich(EnrichArgs),

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

#[derive(Args, Debug)]
struct EnrichArgs {
    /// List files needing enrichment
    #[arg(long)]
    list: bool,

    /// Set enrichment for a specific file
    #[arg(long)]
    set: Option<String>,

    /// Summary text (used with --set)
    #[arg(long)]
    summary: Option<String>,

    /// When-to-use text (used with --set)
    #[arg(long)]
    when_to_use: Option<String>,

    /// Clear enrichment for a file
    #[arg(long)]
    clear: Option<String>,

    /// Clear all enrichments
    #[arg(long)]
    clear_all: bool,

    /// Show enrichment coverage stats
    #[arg(long)]
    stats: bool,

    /// Use API for bulk enrichment
    #[arg(long)]
    api: bool,

    /// API key (overrides env var)
    #[arg(long)]
    api_key: Option<String>,

    /// Provider: gemini or anthropic
    #[arg(long, default_value = "gemini")]
    provider: String,

    /// Model override
    #[arg(long)]
    model: Option<String>,

    /// Max files to enrich (by PageRank)
    #[arg(long)]
    top: Option<usize>,

    /// Re-enrich all files, even already enriched ones
    #[arg(long)]
    force: bool,

    /// Show estimated cost without making API calls
    #[arg(long)]
    dry_run: bool,

    /// Max parallel API requests
    #[arg(long, default_value_t = 8)]
    concurrency: usize,

    /// JSON output (for --list and --stats)
    #[arg(long)]
    json: bool,
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
        Commands::Enrich(args) => {
            tracing::info!("Running enrich command");
            codemap::enrich::run_enrich(
                &root,
                codemap::enrich::EnrichOpts {
                    list: args.list,
                    set: args.set.as_deref(),
                    summary: args.summary.as_deref(),
                    when_to_use: args.when_to_use.as_deref(),
                    clear: args.clear.as_deref(),
                    clear_all: args.clear_all,
                    stats: args.stats,
                    api: args.api,
                    api_key: args.api_key.as_deref(),
                    provider: &args.provider,
                    model: args.model.as_deref(),
                    top: args.top,
                    force: args.force,
                    dry_run: args.dry_run,
                    concurrency: args.concurrency,
                    json: args.json,
                },
            )?;
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

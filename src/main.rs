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

        /// Format: tree, json, or signatures
        #[arg(short, long, default_value = "tree")]
        format: String,
    },

    /// Query symbols or files by name
    Query {
        #[arg()]
        pattern: String,

        #[arg(short, long, default_value_t = 10)]
        limit: usize,
    },

    /// Start the MCP server (stdio transport)
    Serve,
}

#[derive(Args, Debug)]
struct IndexArgs {
    /// Force full re-index (ignore cache)
    #[arg(long)]
    force: bool,
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
            tracing::info!(root = %root.display(), force = args.force, "Indexing codebase");
            codemap::index::run_index(&root, args.force)?;
        }
        Commands::Map { tokens, format } => {
            tracing::info!(tokens, format, "Generating code map");
            codemap::map::run_map(&root, tokens, &format)?;
        }
        Commands::Query { pattern, limit } => {
            tracing::info!(pattern, limit, "Querying symbols");
            codemap::query::run_query(&root, &pattern, limit)?;
        }
        Commands::Serve => {
            tracing::info!("Starting MCP server");
            let db_path = root.join(".codemap/index.db");
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(codemap::mcp::run_mcp_server(
                db_path.to_string_lossy().to_string(),
                root,
            ))?;
        }
    }

    Ok(())
}

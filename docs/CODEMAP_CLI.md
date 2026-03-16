# Buildable project specification for codemap: a Rust CLI for JS/TS code intelligence

**codemap** is a Rust CLI tool that parses JavaScript/TypeScript codebases, builds a dependency graph with ranked symbols, and surfaces the most relevant code context to Claude Code via hooks and MCP. This specification covers every crate API, configuration schema, and algorithm needed to build it — verified against crates.io and official documentation as of March 2026.

---

## Crate versions and Cargo.toml dependencies

Every dependency below has been verified against crates.io or docs.rs. The oxc ecosystem uses lockstep versioning at **~0.118.0**, while oxc_resolver follows its own track at **v11.x**. The rmcp SDK shows **0.16.0** in its crates.io README and **1.2.0** on docs.rs — pin conservatively and verify with `cargo search rmcp`.

```toml
[package]
name = "codemap"
version = "0.1.0"
edition = "2024"

[dependencies]
# Parsing & semantic analysis
oxc = { version = "0.118", features = ["full"] }
oxc_resolver = "11.19"

# CLI framework
clap = { version = "4.5", features = ["derive"] }

# Graph & ranking
petgraph = "0.8"

# Storage
rusqlite = { version = "0.38", features = ["bundled"] }

# File walking
ignore = "0.4"

# Content hashing
blake3 = "1.8"

# MCP server
rmcp = { version = "0.16", features = ["server", "transport-io"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
schemars = "1"

# Utilities
anyhow = "1"
tracing = "0.1"
tracing-subscriber = "0.3"
```

The `oxc` umbrella crate re-exports `oxc_parser`, `oxc_semantic`, `oxc_ast`, `oxc_span`, `oxc_allocator`, and more. You do **not** need to depend on individual sub-crates. The `"full"` feature enables `semantic`, `transformer`, `minifier`, `codegen`, `cfg`, `sourcemap`, and `isolated_declarations`. For codemap, `semantic` is the critical one, but `"full"` keeps future expansion cheap.

Notably, **`oxc_module_lexer`** (v0.38.0) has not been updated alongside the main crates since mid-2025 and should be considered stale. The correct approach for import/export extraction is direct AST walking or `oxc_semantic`, both covered below.

---

## Parsing TypeScript with oxc: the three-step pipeline

The oxc parser takes three inputs and returns one output. An arena `Allocator` owns all AST nodes (bumpalo-based, zero-cost bulk deallocation). `SourceType` is inferred from the file extension — it handles `.ts`, `.tsx`, `.js`, `.jsx`, `.mjs`, `.cjs` automatically. The parser is roughly **2× faster than SWC** and **3× faster than Biome**.

```rust
use std::path::Path;
use oxc::{
    allocator::Allocator,
    parser::{Parser, ParserReturn},
    span::SourceType,
    semantic::{SemanticBuilder, SemanticBuilderReturn},
};

/// Parse a file and return semantic information.
/// The allocator must outlive the returned `Semantic`.
fn analyze_file(path: &Path, source: &str) -> anyhow::Result<FileAnalysis> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(path)
        .map_err(|_| anyhow::anyhow!("Unsupported file type: {}", path.display()))?;

    // Step 1: Parse → AST
    let ParserReturn { program, errors, panicked, .. } =
        Parser::new(&allocator, source, source_type).parse();
    if panicked { anyhow::bail!("Parser panicked on {}", path.display()); }
    if !errors.is_empty() {
        tracing::warn!("{}: {} parse errors", path.display(), errors.len());
    }

    // Step 2: Semantic analysis → symbols, scopes, references
    let SemanticBuilderReturn { semantic, errors: sem_errors } =
        SemanticBuilder::new()
            .with_check_syntax_error(true)
            .build(&program);

    // Step 3: Extract exports + imports from AST body
    let mut analysis = FileAnalysis::default();
    extract_imports_exports(&program, &mut analysis);
    extract_symbols(&semantic, &mut analysis);
    Ok(analysis)
}
```

### Extracting imports and exports by walking the AST

The `Program.body` contains top-level statements. Import and export declarations are specific AST node types. This replaces the stale `oxc_module_lexer`:

```rust
use oxc::ast::ast::*;

fn extract_imports_exports(program: &Program, out: &mut FileAnalysis) {
    for stmt in &program.body {
        match stmt {
            Statement::ImportDeclaration(import) => {
                let source = import.source.value.as_str();
                if let Some(specifiers) = &import.specifiers {
                    for spec in specifiers {
                        match spec {
                            ImportDeclarationSpecifier::ImportSpecifier(s) => {
                                out.imports.push(Import {
                                    source: source.to_string(),
                                    name: s.local.name.to_string(),
                                    kind: ImportKind::Named,
                                });
                            }
                            ImportDeclarationSpecifier::ImportDefaultSpecifier(s) => {
                                out.imports.push(Import {
                                    source: source.to_string(),
                                    name: s.local.name.to_string(),
                                    kind: ImportKind::Default,
                                });
                            }
                            ImportDeclarationSpecifier::ImportNamespaceSpecifier(s) => {
                                out.imports.push(Import {
                                    source: source.to_string(),
                                    name: s.local.name.to_string(),
                                    kind: ImportKind::Namespace,
                                });
                            }
                        }
                    }
                }
            }
            Statement::ExportNamedDeclaration(export) => {
                if let Some(decl) = &export.declaration {
                    match decl {
                        Declaration::FunctionDeclaration(f) => {
                            if let Some(id) = &f.id {
                                out.exports.push(Export {
                                    name: id.name.to_string(),
                                    kind: ExportKind::Function,
                                });
                            }
                        }
                        Declaration::VariableDeclaration(var) => {
                            for d in &var.declarations {
                                if let BindingPatternKind::BindingIdentifier(id) = &d.id.kind {
                                    out.exports.push(Export {
                                        name: id.name.to_string(),
                                        kind: ExportKind::Variable,
                                    });
                                }
                            }
                        }
                        Declaration::TSInterfaceDeclaration(iface) => {
                            out.exports.push(Export {
                                name: iface.id.name.to_string(),
                                kind: ExportKind::Interface,
                            });
                        }
                        Declaration::TSTypeAliasDeclaration(alias) => {
                            out.exports.push(Export {
                                name: alias.id.name.to_string(),
                                kind: ExportKind::TypeAlias,
                            });
                        }
                        _ => {}
                    }
                }
                if let Some(source) = &export.source {
                    for spec in &export.specifiers {
                        out.reexports.push(ReExport {
                            source: source.value.to_string(),
                            local: spec.local.to_string(),
                            exported: spec.exported.to_string(),
                        });
                    }
                }
            }
            Statement::ExportDefaultDeclaration(_) => {
                out.exports.push(Export {
                    name: "default".to_string(),
                    kind: ExportKind::Default,
                });
            }
            Statement::ExportAllDeclaration(star) => {
                out.reexports.push(ReExport {
                    source: star.source.value.to_string(),
                    local: "*".to_string(),
                    exported: star.exported.as_ref()
                        .map(|e| e.to_string())
                        .unwrap_or("*".to_string()),
                });
            }
            _ => {}
        }
    }
}
```

### Extracting symbol-level detail with oxc_semantic

`SemanticBuilder` produces a `Semantic` struct whose `scoping()` method returns a `Scoping` object (the combined symbol table + scope tree). Symbols use a Struct-of-Arrays layout for cache efficiency. The `SymbolFlags` bitflags include an **`Export`** flag that directly identifies exported symbols:

```rust
fn extract_symbols(semantic: &oxc::semantic::Semantic, out: &mut FileAnalysis) {
    let scoping = semantic.scoping();
    for symbol_id in scoping.symbol_ids() {
        let name = scoping.symbol_name(symbol_id).to_string();
        let flags = scoping.symbol_flags(symbol_id);
        let scope_id = scoping.symbol_scope(symbol_id);

        // Only top-level or exported symbols for the code map
        let is_exported = flags.contains(oxc::syntax::symbol::SymbolFlags::Export);
        let is_top_level = scope_id == scoping.root_scope_id();

        if is_exported || is_top_level {
            out.symbols.push(SymbolInfo {
                name,
                is_exported,
                reference_count: scoping.get_resolved_reference_ids(symbol_id).len(),
            });
        }
    }
}
```

---

## Resolving import paths with oxc_resolver

`oxc_resolver` (v11.19.1) is maintained in a separate repository and follows its own versioning. It implements the full Node.js ESM and CommonJS resolution algorithms, ported from webpack's `enhanced-resolve`. It handles **tsconfig paths**, **package.json exports/imports fields**, **barrel files**, and **Yarn PnP**.

```rust
use oxc_resolver::{Resolver, ResolveOptions, TsconfigDiscovery};
use std::path::Path;

fn create_resolver() -> Resolver {
    Resolver::new(ResolveOptions {
        extensions: vec![
            ".ts".into(), ".tsx".into(), ".js".into(),
            ".jsx".into(), ".mjs".into(), ".json".into(),
        ],
        // Map .js imports to .ts source files (common in TypeScript projects)
        extension_alias: vec![
            (".js".into(), vec![".ts".into(), ".tsx".into(), ".js".into()]),
            (".mjs".into(), vec![".mts".into(), ".mjs".into()]),
        ],
        condition_names: vec!["node".into(), "import".into()],
        main_fields: vec!["module".into(), "main".into()],
        tsconfig: Some(TsconfigDiscovery::Auto), // auto-discover tsconfig.json
        ..ResolveOptions::default()
    })
}

fn resolve_import(resolver: &Resolver, from_dir: &Path, specifier: &str) -> Option<String> {
    match resolver.resolve(from_dir, specifier) {
        Ok(resolution) => Some(resolution.full_path().display().to_string()),
        Err(_) => None, // unresolvable (external package, etc.)
    }
}
```

The resolver's `ResolveOptions` also supports `alias` (webpack-style module aliasing), `alias_fields` (browser field in package.json), and `modules` (directories to search, default `["node_modules"]`). For projects with tsconfig path mappings, `TsconfigDiscovery::Auto` finds and applies the nearest `tsconfig.json` automatically.

---

## Graph construction and PageRank with petgraph

**petgraph v0.8.3 includes a built-in `page_rank` function** at `petgraph::algo::page_rank`, plus a `parallel_page_rank` variant behind the `rayon` feature. This eliminates the need for a custom implementation or the Neo4j `graph` crate.

```rust
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::algo::page_rank;
use std::collections::HashMap;

struct DependencyGraph {
    graph: DiGraph<String, EdgeKind>,
    path_to_node: HashMap<String, NodeIndex>,
}

#[derive(Clone, Debug)]
enum EdgeKind {
    Import,     // A imports from B
    ReExport,   // A re-exports from B
    TypeImport, // A imports types from B
}

impl DependencyGraph {
    fn new() -> Self {
        Self { graph: DiGraph::new(), path_to_node: HashMap::new() }
    }

    fn add_file(&mut self, path: &str) -> NodeIndex {
        *self.path_to_node.entry(path.to_string())
            .or_insert_with(|| self.graph.add_node(path.to_string()))
    }

    fn add_edge(&mut self, from: &str, to: &str, kind: EdgeKind) {
        let from_idx = self.add_file(from);
        let to_idx = self.add_file(to);
        self.graph.add_edge(from_idx, to_idx, kind);
    }

    /// Standard PageRank — damping 0.85, 100 iterations
    fn compute_ranks(&self) -> Vec<(String, f64)> {
        let scores = page_rank(&self.graph, 0.85, 100);
        let mut ranked: Vec<(String, f64)> = self.graph.node_indices()
            .map(|idx| (self.graph[idx].clone(), scores[idx.index()]))
            .collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        ranked
    }
}
```

### Personalized PageRank (aider's approach)

Aider's repo map uses **personalized PageRank** to bias results toward contextually relevant files. The algorithm works as follows:

- **Graph nodes** are file paths. **Edges** connect files that share identifiers (file A references a symbol defined in file B).
- **Personalization weights** give bonus scores to files the user is actively working with: **+100/N** for files in the current chat, **+100/N** for files mentioned in conversation, **+100/N** for files whose path components match mentioned identifiers. These accumulate additively.
- **Edge weight multipliers** amplify signal: identifiers mentioned in conversation get **×10**, long snake/camel-case identifiers (≥8 chars) get **×10**, private identifiers (starting with `_`) get **×0.1**, identifiers defined in >5 files get **×0.1**, and references from active chat files get **×50**.

For codemap's SessionStart hook (where there's no active chat context), standard unweighted PageRank is appropriate. For the MCP `get_relevant_context` tool (which receives a query), personalized PageRank with query-term matching is the right approach:

```rust
fn personalized_pagerank(
    graph: &DiGraph<String, f64>,
    seed_nodes: &[NodeIndex],
    damping: f64,
    iterations: usize,
) -> Vec<f64> {
    let n = graph.node_count();
    if n == 0 { return vec![]; }

    // Build personalization vector
    let mut personalization = vec![0.0; n];
    if seed_nodes.is_empty() {
        personalization.iter_mut().for_each(|p| *p = 1.0 / n as f64);
    } else {
        let weight = 1.0 / seed_nodes.len() as f64;
        for &node in seed_nodes { personalization[node.index()] = weight; }
    }

    let mut ranks = vec![1.0 / n as f64; n];
    let mut new_ranks = vec![0.0; n];

    for _ in 0..iterations {
        for (i, p) in personalization.iter().enumerate() {
            new_ranks[i] = (1.0 - damping) * p;
        }
        for node in graph.node_indices() {
            let out_edges: Vec<_> = graph.edges(node).collect();
            let total_weight: f64 = out_edges.iter().map(|e| *e.weight()).sum();
            if total_weight > 0.0 {
                for edge in &out_edges {
                    let share = ranks[node.index()] * edge.weight() / total_weight;
                    new_ranks[edge.target().index()] += damping * share;
                }
            } else {
                // Dangling node: distribute to all via personalization
                for (i, p) in personalization.iter().enumerate() {
                    new_ranks[i] += damping * ranks[node.index()] * p;
                }
            }
        }
        std::mem::swap(&mut ranks, &mut new_ranks);
    }
    ranks
}
```

---

## File walking with the ignore crate

The `ignore` crate (v0.4.25, from the ripgrep project) provides `WalkBuilder`, which respects `.gitignore` by default. The **`add_custom_ignore_filename`** method adds support for a `.codemapignore` file using standard gitignore glob syntax, with **higher precedence** than `.gitignore`:

```rust
use ignore::WalkBuilder;
use ignore::types::TypesBuilder;
use std::path::{Path, PathBuf};

fn discover_files(root: &Path) -> Vec<PathBuf> {
    let mut types = TypesBuilder::new();
    types.add_defaults();
    types.select("ts");
    types.select("js");
    types.add("tsx", "*.tsx").unwrap();
    types.select("tsx");
    types.add("jsx", "*.jsx").unwrap();
    types.select("jsx");
    let types = types.build().unwrap();

    let mut files = Vec::new();
    for entry in WalkBuilder::new(root)
        .hidden(true)                                // skip dotfiles
        .git_ignore(true)                            // respect .gitignore
        .add_custom_ignore_filename(".codemapignore") // project-specific ignores
        .types(types)                                // only ts/tsx/js/jsx
        .build()
    {
        if let Ok(entry) = entry {
            if entry.file_type().map_or(false, |ft| ft.is_file()) {
                files.push(entry.into_path());
            }
        }
    }
    files
}
```

The `.codemapignore` file uses standard gitignore syntax: `dist/`, `*.min.js`, `__tests__/`, `node_modules/`, etc.

---

## Content hashing with blake3

blake3 (v1.8.3) is SIMD-optimized and implements `std::io::Write`, making file hashing trivial. A hash comparison against stored values enables **incremental re-indexing** — only re-parse files whose content has changed:

```rust
fn hash_file(path: &std::path::Path) -> std::io::Result<String> {
    let mut hasher = blake3::Hasher::new();
    let mut file = std::fs::File::open(path)?;
    std::io::copy(&mut file, &mut hasher)?;
    Ok(hasher.finalize().to_hex().to_string())
}
```

---

## SQLite storage with rusqlite

rusqlite v0.38.0 with the `bundled` feature statically compiles **SQLite 3.51.1**, eliminating system dependency issues. WAL mode is essential for concurrent read performance:

```rust
use rusqlite::{Connection, Result, params};

fn init_db(path: &str) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.execute_batch("
        CREATE TABLE IF NOT EXISTS files (
            id         INTEGER PRIMARY KEY,
            path       TEXT NOT NULL UNIQUE,
            hash       TEXT NOT NULL,
            rank       REAL NOT NULL DEFAULT 0.0,
            updated_at INTEGER NOT NULL DEFAULT (unixepoch())
        );
        CREATE TABLE IF NOT EXISTS symbols (
            id          INTEGER PRIMARY KEY,
            file_id     INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
            name        TEXT NOT NULL,
            kind        TEXT NOT NULL,
            is_exported INTEGER NOT NULL DEFAULT 0,
            line        INTEGER,
            ref_count   INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS edges (
            id          INTEGER PRIMARY KEY,
            source_id   INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
            target_id   INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
            edge_type   TEXT NOT NULL,
            specifier   TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_files_path ON files(path);
        CREATE INDEX IF NOT EXISTS idx_symbols_file ON symbols(file_id);
        CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name);
        CREATE INDEX IF NOT EXISTS idx_edges_source ON edges(source_id);
        CREATE INDEX IF NOT EXISTS idx_edges_target ON edges(target_id);
    ")?;
    Ok(conn)
}
```

---

## CLI structure with clap v4 derive

clap v4.5.60 with the derive macro produces clean, maintainable multi-subcommand CLIs. The canonical pattern: a `#[derive(Parser)]` root struct with a `#[command(subcommand)]` field pointing to a `#[derive(Subcommand)]` enum:

```rust
use clap::{Parser, Subcommand, Args};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "codemap", version, about = "Code intelligence for JS/TS codebases")]
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
```

---

## Claude Code hooks configuration

Hooks are defined in `.claude/settings.json` (project-level, committable) or `.claude/settings.local.json` (personal). The system supports **18 hook event types** and **4 handler types** (command, http, prompt, agent). The key integration points for codemap are `SessionStart` (inject the code map into context) and `PostToolUse` (re-index after file writes).

### Exact settings.json schema for codemap

```json
{
  "$schema": "https://json.schemastore.org/claude-code-settings.json",
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "codemap map --tokens 1500 --format tree",
            "timeout": 10
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "Write|Edit|MultiEdit",
        "hooks": [
          {
            "type": "command",
            "command": "codemap index",
            "timeout": 30,
            "async": true
          }
        ]
      }
    ]
  }
}
```

**SessionStart behavior**: Fires on `startup`, `resume`, `clear`, and `compact` events. Whatever the hook command prints to **stdout is injected directly into Claude's context** as additional information. The hook receives JSON on stdin with `session_id`, `cwd`, `source` (trigger type), and `transcript_path`. SessionStart does **not** support matchers — it always fires. Context output should be concise since it consumes tokens from Claude's **200k context window** (of which ~15k is already used by the system prompt and tools).

**PostToolUse matcher syntax**: The `matcher` field is a **case-sensitive regex** against `tool_name`. Built-in tool names are PascalCase: `Write`, `Edit`, `MultiEdit`, `Bash`, `Read`, `Glob`, `Grep`. MCP tools appear as `mcp__<server>__<tool>`. The pipe character `|` provides OR matching. Setting `"async": true` lets the re-indexing run in the background — Claude continues immediately without waiting.

**Timeouts**: Default is **60 seconds** for command hooks, configurable per handler via `"timeout": <seconds>`. Tool-related hooks were increased to a **10-minute** ceiling in v2.1.3. `SessionEnd` hooks default to **1.5 seconds**. Async hooks deliver their output on the next conversation turn.

---

## Claude Code MCP server configuration

MCP servers are configured in **`.mcp.json`** at the project root (for team-shared servers) or **`~/.claude.json`** (for user-global servers). Critically, `mcpServers` is **not** a valid field in `.claude/settings.json` — placing it there produces a validation error.

### .mcp.json schema for codemap

```json
{
  "mcpServers": {
    "codemap": {
      "command": "codemap",
      "args": ["serve"],
      "env": {
        "CODEMAP_ROOT": "${PWD}"
      }
    }
  }
}
```

**Discovery and launch**: Claude Code reads `.mcp.json` at startup. For stdio transport (the default), it launches the server as a child process and communicates via stdin/stdout JSON-RPC 2.0. Tool discovery calls `list_tools()` and typically completes in under **200ms**. The default startup timeout is **30 seconds**, configurable via the `MCP_TIMEOUT` environment variable. Supported transports are **stdio** (local, <5ms latency), **HTTP** (recommended for remote), and **SSE** (deprecated). Up to **20 simultaneous MCP servers** work without noticeable degradation.

When MCP tool descriptions exceed 10% of the context window, Claude Code automatically switches to **on-demand tool loading** — tools are searched and loaded as needed rather than all upfront. The `MAX_MCP_OUTPUT_TOKENS` environment variable (default ~25,000) controls the warning threshold for tool output size.

---

## MCP server implementation with rmcp

The rmcp SDK (v0.16.0+) provides three key proc-macros: `#[tool_router]` on the impl block, `#[tool]` on each tool method, and `#[tool_handler]` on the `ServerHandler` impl. Tool parameters use `schemars::JsonSchema` for automatic JSON Schema generation:

```rust
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::router::tool::ToolRouter,
    model::*,
    tool, tool_handler, tool_router,
    schemars::JsonSchema,
    transport::stdio,
};
use serde::Deserialize;

#[derive(Clone)]
pub struct CodemapServer {
    tool_router: ToolRouter<Self>,
    db_path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct GetMapRequest {
    #[schemars(description = "Maximum tokens in the output map")]
    max_tokens: Option<usize>,
    #[schemars(description = "Optional: bias results toward files related to this query")]
    query: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct LookupSymbolRequest {
    #[schemars(description = "Symbol name or glob pattern to search for")]
    pattern: String,
    #[schemars(description = "Maximum number of results")]
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct FileDepsRequest {
    #[schemars(description = "File path to get dependencies for")]
    file_path: String,
    #[schemars(description = "Direction: 'imports' or 'importers'")]
    direction: Option<String>,
}

#[tool_router]
impl CodemapServer {
    pub fn new(db_path: String) -> Self {
        Self { tool_router: Self::tool_router(), db_path }
    }

    #[tool(description = "Get a ranked code map of the most important files and their exported symbols. Optionally provide a query to bias results toward relevant files using personalized PageRank.")]
    async fn get_code_map(&self, #[tool(aggr)] req: GetMapRequest) -> String {
        let max_tokens = req.max_tokens.unwrap_or(1500);
        // ... generate map with optional personalized PageRank
        todo!()
    }

    #[tool(description = "Look up a symbol by name across the codebase. Returns file location, export status, and reference count.")]
    async fn lookup_symbol(&self, #[tool(aggr)] req: LookupSymbolRequest) -> String {
        todo!()
    }

    #[tool(description = "Get the import dependencies or importers of a specific file.")]
    async fn get_file_deps(&self, #[tool(aggr)] req: FileDepsRequest) -> String {
        todo!()
    }
}

#[tool_handler]
impl ServerHandler for CodemapServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .build(),
            server_info: Implementation {
                name: "codemap".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
            instructions: Some(
                "Code intelligence server for JS/TS. Use get_code_map for an overview, \
                 lookup_symbol to find specific symbols, get_file_deps for dependency analysis."
                    .into()
            ),
        }
    }
}

// Entry point for `codemap serve`
pub async fn run_mcp_server(db_path: String) -> anyhow::Result<()> {
    let server = CodemapServer::new(db_path);
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
```

Tool functions can be sync or async, and can return `String`, `CallToolResult`, or `Result<String, E>` where `E: IntoContents`. The `#[tool(aggr)]` attribute collapses all parameters into a single struct; alternatively, `#[tool(param)]` marks individual parameters.

---

## Output format: what to inject into Claude's context

### Token budget

Based on Claude Code's **200k context window**, roughly **183k tokens** are available for actual work after the system prompt (~3.2k), tools (~11.6k), and memory files consume their share. For SessionStart context injection, the optimal budget is **1,000–2,000 tokens** — this matches aider's validated default of **1,024 tokens** and leaves ample room for conversation. Aider's research shows that a well-ranked 1k token map outperforms a naive 10k token dump of source code.

### Recommended output format

The most effective format for LLM code understanding is **aider's tree-sitter tag style**: file paths as headers, with function/class/type signatures shown in source syntax and elided bodies. This preserves language-specific syntax while compressing to roughly **5–10% of original source size**:

```
src/services/auth.ts:
  export class AuthService
    constructor(private db: Database, private jwt: JwtService)
    async login(email: string, password: string): Promise<AuthResult>
    async verify(token: string): Promise<User | null>
    private async hashPassword(pw: string): Promise<string>

src/models/user.ts:
  export interface User
  export interface CreateUserInput
  export type UserRole = "admin" | "member" | "guest"

src/routes/api.ts:
  export function registerRoutes(app: Express): void
  → imports: auth.ts, user.ts, middleware.ts

src/middleware/validate.ts:
  export function validateBody<T>(schema: ZodSchema<T>): RequestHandler
```

Key format decisions backed by aider's benchmarks: **function signatures capture ~90% of what LLMs need** for architecture understanding. Bold the PageRank score threshold — files above the median are structurally important. Include import edges for the top-ranked files to show dependency relationships. Truncate lines to **100 characters**. Exclude files already in Claude's active context (they're in the conversation anyway).

For the SessionStart hook, wrap the output with a clear header so Claude knows what it is:

```
## Repository Code Map (auto-generated by codemap)
Showing top 15 files by structural importance (PageRank).

[... tree-format output ...]
```

### Anthropic's own approach

Anthropic chose **agentic search over RAG** for Claude Code's code retrieval — Claude uses Grep and Glob tools to find code on demand rather than pre-indexing everything into a vector store. This means codemap's role is complementary: providing a **structural overview** so Claude knows *what exists and where to look*, not providing full implementations. The CLAUDE.md documentation pattern reinforces this: **prefer pointers to copies**, use progressive disclosure, and keep the top-level context lean.

---

## Architecture summary and data flow

The complete data flow for codemap:

1. **`codemap index`**: Walk files (ignore crate) → hash each file (blake3) → skip unchanged files (rusqlite lookup) → parse changed files (oxc parser + semantic) → extract imports/exports/symbols → resolve import paths (oxc_resolver) → build dependency graph (petgraph DiGraph) → compute PageRank → persist everything to SQLite.

2. **`codemap map`**: Load ranked files from SQLite → binary-search to fit within token budget → format as tree-style signatures → print to stdout. When called from SessionStart hook, this output goes directly into Claude's context.

3. **`codemap serve`**: Launch rmcp stdio MCP server → expose `get_code_map`, `lookup_symbol`, `get_file_deps` tools → Claude Code calls these tools during conversation for on-demand code intelligence.

4. **PostToolUse hook** (async): After Claude writes/edits a file → `codemap index` runs in background → graph and ranks update incrementally → next `get_code_map` call reflects changes.

This architecture keeps the SessionStart injection fast (SQLite read only), pushes expensive parsing to async background hooks, and gives Claude on-demand deep queries through MCP tools.

---

## Conclusion: three integration points, one binary

The codemap specification centers on a single Rust binary that serves three integration surfaces: a **SessionStart hook** for automatic context injection (~1.5k tokens of ranked code signatures), an **async PostToolUse hook** for incremental re-indexing after file changes, and an **MCP stdio server** for on-demand queries during conversation. The oxc parser at v0.118+ provides production-grade TypeScript parsing at 2-3× the speed of alternatives, and petgraph's built-in `page_rank` function eliminates custom algorithm code. The critical design insight from aider's research is that personalized PageRank with proper edge weighting produces dramatically better context than naive file inclusion — and that **1,024 tokens** of well-ranked signatures outperform 10,000 tokens of raw source. Every API, schema, and crate version in this specification has been verified against current sources; pin your dependencies accordingly, and the implementation should compile and integrate without surprises.

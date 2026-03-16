# codemap

Rust CLI that parses JS/TS codebases, builds a dependency graph with PageRank-ranked symbols, and surfaces structural context to Claude Code via hooks.

## Quick reference

```bash
cargo build                    # debug build
cargo build --release          # release build (LTO, stripped)
cargo fmt                      # format (runs automatically via PostToolUse hook)
cargo clippy                   # lint
./target/debug/codemap index   # index current project
./target/debug/codemap map     # print code map
```

## Architecture

Single binary, no async runtime. All source files are in `src/` (flat module layout).

**Data flow for `codemap index`:**
discover files (ignore crate) → hash with blake3 → skip unchanged → parse with oxc → extract imports/exports/symbols → resolve imports (oxc_resolver) → build DiGraph (petgraph) → compute PageRank → persist to SQLite (.codemap/index.db)

**Key modules:**
- `main.rs` — CLI entry (clap derive), dispatches to command modules
- `index.rs` — orchestrates the full index pipeline
- `map.rs` — renders ranked code map (tree format with signatures)
- `symbol.rs` — symbol lookup (definitions + references)
- `deps.rs` — import graph traversal (BFS with depth)
- `context.rs` — keyword-based file relevance scoring + graph expansion
- `setup.rs` — writes Claude Code hook config to settings files
- `parser.rs` — oxc parsing, semantic analysis, signature extraction
- `resolver.rs` — oxc_resolver for import path resolution
- `walk.rs` — file discovery with .gitignore + .codemapignore support
- `graph.rs` — petgraph DiGraph construction + PageRank (damping 0.85, 100 iter)
- `db.rs` — rusqlite SQLite operations (WAL mode, 3 tables: files, symbols, edges)
- `hash.rs` — blake3 content hashing
- `scorer.rs` — keyword scoring + 1-hop graph expansion for context command
- `types.rs` — shared data structures (Import, Export, SymbolInfo, etc.)

## Key dependencies

| Crate | Purpose |
|-------|---------|
| oxc 0.118 | JS/TS parser + semantic analysis |
| oxc_resolver 11.19 | Import path resolution (tsconfig, ESM, CJS) |
| petgraph 0.8 | Dependency graph + built-in `page_rank()` |
| rusqlite 0.38 (bundled) | SQLite with bundled 3.51.1 |
| clap 4.5 (derive) | CLI argument parsing |
| ignore 0.4 | File walking with gitignore support |
| blake3 1.8 | SIMD-optimized content hashing |

## Conventions

- Rust 2024 edition
- All logging goes to stderr via `tracing` (stdout is reserved for output)
- Errors use `anyhow::Result`
- CLI output supports `--json` for structured output on query commands
- Signatures truncated to 100 chars
- Token budget: `--tokens N` where N chars ≈ N/4 tokens

## npm distribution

Platform binaries are published as optional deps under `npm/`. The `scripts/run.js` entry point detects the platform and executes the appropriate binary. `scripts/postinstall.js` handles binary setup.

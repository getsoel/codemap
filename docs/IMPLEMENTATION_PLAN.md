# codemap Implementation Plan

Based on the spec in [CODEMAP_CLI.md](./CODEMAP_CLI.md). The project has a `Cargo.toml` with all dependencies and a skeleton `main.rs` with CLI parsing (all commands stubbed).

---

## Phase 1: Core Data Types & Storage

**Files**: `src/types.rs`, `src/db.rs`, `src/lib.rs`

### 1a. `src/types.rs` — Shared data structures

- `FileAnalysis { imports, exports, reexports, symbols }`
- `Import { source, name, kind: ImportKind }` — `ImportKind = Named | Default | Namespace`
- `Export { name, kind: ExportKind, line }` — `ExportKind = Function | Variable | Class | Interface | TypeAlias | Enum | Default`
- `ReExport { source, local, exported }`
- `SymbolInfo { name, is_exported, reference_count }`

### 1b. `src/db.rs` — SQLite storage layer

- `init_db(path) -> Connection` — create tables (`files`, `symbols`, `edges`), enable WAL mode, create indexes (spec lines 444–478)
- `get_file_hash(conn, path) -> Option<String>` — lookup existing hash for incremental skip
- `upsert_file(conn, path, hash, rank)` — insert or update file record
- `delete_stale_files(conn, known_paths)` — remove files no longer on disk
- `insert_symbols(conn, file_id, symbols)` — batch insert symbols (clear old first)
- `insert_edges(conn, file_id, edges)` — batch insert edges (clear old first)
- `update_ranks(conn, ranks: &[(String, f64)])` — bulk update rank column
- `get_ranked_files(conn, limit) -> Vec<RankedFile>` — load files ordered by rank
- `query_symbols(conn, pattern, limit) -> Vec<SymbolResult>` — LIKE query on symbol name
- `get_file_deps(conn, file_path, direction) -> Vec<DepEdge>` — query edges table

### 1c. `src/lib.rs` — Module declarations

**Verify**: Unit tests that create an in-memory SQLite DB, insert/query data.

---

## Phase 2: File Discovery & Hashing

**Files**: `src/walk.rs`, `src/hash.rs`

### 2a. `src/walk.rs` — File discovery

- `discover_files(root: &Path) -> Vec<PathBuf>` — WalkBuilder with `.codemapignore` support, filtering to `.ts/.tsx/.js/.jsx/.mjs/.cjs` (spec lines 389–415)

### 2b. `src/hash.rs` — Content hashing

- `hash_file(path: &Path) -> io::Result<String>` — blake3 streaming hash (spec lines 427–433)

**Verify**: Point at a test fixture directory, verify correct files discovered, hashes stable.

---

## Phase 3: Parsing & Analysis

**Files**: `src/parser.rs`

### 3a. `analyze_file(path, source) -> Result<FileAnalysis>`

- Allocator → parser → semantic builder (spec lines 71–95)

### 3b. `extract_imports_exports(program, out)`

- Walk `Program.body` for import/export declarations (spec lines 105–202)
- Handle all export kinds: Function, Variable, Class, Interface, TypeAlias, Enum, Default

### 3c. `extract_symbols(semantic, out)`

- Iterate `scoping.symbol_ids()`, collect top-level and exported symbols with reference counts (spec lines 210–229)

**Verify**: Parse a handful of real `.ts` files, assert correct imports/exports/symbols extracted.

---

## Phase 4: Module Resolution & Graph

**Files**: `src/resolver.rs`, `src/graph.rs`

### 4a. `src/resolver.rs` — Import resolution

- `create_resolver() -> Resolver` — TS extensions, extension aliases, tsconfig auto-discovery (spec lines 239–258)
- `resolve_import(resolver, from_dir, specifier) -> Option<PathBuf>` — returns None for external packages

### 4b. `src/graph.rs` — Dependency graph + ranking

- `DependencyGraph` struct wrapping `DiGraph<String, EdgeKind>` + `HashMap<String, NodeIndex>` (spec lines 281–318)
- `EdgeKind = Import | ReExport | TypeImport`
- `add_file`, `add_edge` methods
- `compute_ranks() -> Vec<(String, f64)>` — standard PageRank via `petgraph::algo::page_rank` (damping 0.85, 100 iterations)
- `personalized_pagerank(graph, seed_nodes, damping, iterations) -> Vec<f64>` — for MCP query-biased ranking (spec lines 332–375)

**Verify**: Build a small graph manually, verify PageRank output is sensible.

---

## Phase 5: Index Command

**Files**: `src/index.rs`, update `src/main.rs`

### 5a. `run_index(root: &Path, force: bool) -> Result<()>`

Pipeline:
1. Discover files (`walk`)
2. For each file, hash and check DB (`hash`, `db`)
3. Parse changed files (`parser`)
4. Resolve imports (`resolver`)
5. Build graph (`graph`)
6. Compute PageRank (`graph`)
7. Persist to SQLite in a transaction (`db`)

- DB location: `<root>/.codemap/index.db` (create `.codemap/` dir if missing)
- Log progress: file count, skipped (unchanged), parsed, errors

### 5b. Wire `Commands::Index` in `main.rs`

**Verify**: Run `codemap index` on a real TS project, inspect the SQLite DB.

---

## Phase 6: Map & Query Commands

**Files**: `src/map.rs`, `src/query.rs`, update `src/main.rs`

### 6a. `src/map.rs` — Code map generation

- `run_map(root: &Path, tokens: usize, format: &str) -> Result<()>`
- Load ranked files from DB
- For each file (in rank order): re-parse to extract signatures, format as tree-style output (spec lines 732–751)
- Token budget: estimate ~4 chars/token, accumulate until budget exhausted
- Wrap output with header: `## Repository Code Map (auto-generated by codemap)`
- Support formats: `tree` (default), `json`
- Print to stdout

### 6b. `src/query.rs` — Symbol/file search

- `run_query(root: &Path, pattern: &str, limit: usize) -> Result<()>`
- Query symbols table with LIKE matching
- Print results with file path, line number, export status

### 6c. Wire `Commands::Map` and `Commands::Query` in `main.rs`

**Verify**: Run `codemap map --tokens 1500` and verify concise, useful output.

---

## Phase 7: MCP Server

**Files**: `src/mcp.rs`, update `src/main.rs`

### 7a. `CodemapServer` with rmcp

- Struct with `ToolRouter` (spec lines 633–714)
- Three tools:
  - `get_code_map(max_tokens, query)` — ranked map, personalized PageRank when query provided
  - `lookup_symbol(pattern, limit)` — symbol search
  - `get_file_deps(file_path, direction)` — imports or importers of a file
- `run_mcp_server(db_path) -> Result<()>` — stdio transport entry point

### 7b. Wire `Commands::Serve` in `main.rs`

- Use `tokio::runtime::Runtime` to run async MCP server from sync main

**Verify**: `echo '{"jsonrpc":"2.0","method":"tools/list","id":1}' | codemap serve`

---

## Post-Implementation: Integration Config

### Hook config (`.claude/settings.json`)

SessionStart hook for context injection and PostToolUse async hook for re-indexing (spec lines 552–581). Note: the PostToolUse `cargo fmt` hook is already configured; the `codemap index` hook should be added once Phase 5 is complete.

### MCP config (`.mcp.json`)

MCP server config for `codemap serve` (spec lines 599–609). Add once Phase 7 is complete.

---

## Module Dependency Graph

```
main.rs
├── types.rs      (no deps)
├── db.rs         (types)
├── hash.rs       (no deps)
├── walk.rs       (no deps)
├── parser.rs     (types)
├── resolver.rs   (no deps)
├── graph.rs      (types)
├── index.rs      (walk, hash, parser, resolver, graph, db)
├── map.rs        (db, parser)
├── query.rs      (db)
└── mcp.rs        (db, map, graph, parser)
```

## Build Order

Each phase builds on the last. Phases 1 & 2 can be built in parallel. Phase 3 is the most complex (oxc API surface). Phase 5 is the first end-to-end milestone. Phase 7 (MCP) depends on everything else.

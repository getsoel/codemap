# Layer 1: Rust Performance Benchmarks — Specification

## Goal

Track codemap's execution performance over time using criterion micro-benchmarks. Detect regressions when changing the parser, scorer, graph, or DB layer.

## Benchmark groups

### 1. Parser (`benches/parser.rs`)

The parser (oxc) is the CPU bottleneck during indexing. Benchmark both parse+analyze and signature extraction.

#### Functions under test

```rust
// Full parse: AST + semantic analysis + import/export extraction
parser::analyze_file(path: &Path, source: &str) -> Result<FileAnalysis>

// Signature extraction for code map generation
parser::extract_signatures(path: &Path, source: &str) -> Vec<String>
```

#### Benchmark cases

| Name | Input | What it measures |
|------|-------|-----------------|
| `parse_small` | 50 LOC, 2 imports, 3 exports | Baseline parse overhead |
| `parse_medium` | 300 LOC, 10 imports, 15 exports | Typical file |
| `parse_large` | 2000 LOC, 30 imports, 40 exports | Large file (e.g. Editor.ts) |
| `parse_types_heavy` | 500 LOC, all type exports | Type-heavy file (interfaces, type aliases) |
| `signatures_medium` | 300 LOC file | Signature extraction cost |
| `signatures_large` | 2000 LOC file | Signature extraction at scale |

#### Synthetic fixtures

Generate TS source strings in the benchmark setup:

```rust
fn gen_ts_source(imports: usize, functions: usize, interfaces: usize) -> String {
    let mut s = String::new();
    for i in 0..imports {
        s.push_str(&format!("import {{ thing{i} }} from './mod{i}';\n"));
    }
    for i in 0..functions {
        s.push_str(&format!(
            "export function func{i}(a: string, b: number): boolean {{\n  return a.length > b;\n}}\n\n"
        ));
    }
    for i in 0..interfaces {
        s.push_str(&format!(
            "export interface Item{i} {{\n  id: string;\n  name: string;\n  value: number;\n}}\n\n"
        ));
    }
    s
}
```

These are pure functions — no filesystem or DB needed.

---

### 2. Graph & PageRank (`benches/graph.rs`)

PageRank computation scales with edges × iterations. Benchmark graph construction and ranking separately.

#### Functions under test

```rust
// Graph construction
graph::DependencyGraph::new()
graph.add_file(path: &str)
graph.add_edge(from: &str, to: &str)

// PageRank (damping=0.85, 100 iterations)
graph.compute_ranks() -> Vec<(String, f64)>
```

#### Benchmark cases

| Name | Nodes | Edges | Topology | What it measures |
|------|-------|-------|----------|-----------------|
| `rank_10_chain` | 10 | 9 | Linear chain | Minimum viable graph |
| `rank_100_sparse` | 100 | 150 | Random DAG | Typical small project |
| `rank_100_dense` | 100 | 500 | Dense DAG | Import-heavy project |
| `rank_500_sparse` | 500 | 800 | Random DAG | Medium project |
| `rank_1000_sparse` | 1000 | 2000 | Random DAG | Large project |
| `rank_1000_star` | 1000 | 999 | Star (1 hub) | Worst-case hub (e.g. types.ts) |
| `rank_2000_realistic` | 2000 | 8000 | Power-law | Matches tldraw fixture (2293 files, 8627 edges) |

#### Synthetic graph generation

```rust
fn gen_chain(n: usize) -> DependencyGraph {
    let mut g = DependencyGraph::new();
    for i in 0..n {
        g.add_file(&format!("file{i}.ts"));
    }
    for i in 0..n-1 {
        g.add_edge(&format!("file{i}.ts"), &format!("file{}.ts", i+1));
    }
    g
}

fn gen_random_dag(nodes: usize, edges: usize, seed: u64) -> DependencyGraph {
    // Deterministic pseudo-random DAG: edges only go from lower to higher index
    let mut g = DependencyGraph::new();
    for i in 0..nodes {
        g.add_file(&format!("src/mod{i}/index.ts"));
    }
    let mut rng = simple_rng(seed);
    for _ in 0..edges {
        let from = rng.next() % nodes;
        let to = rng.next() % nodes;
        if from != to {
            g.add_edge(
                &format!("src/mod{from}/index.ts"),
                &format!("src/mod{to}/index.ts"),
            );
        }
    }
    g
}
```

Pure computation — no I/O.

---

### 3. Hashing (`benches/hash.rs`)

Blake3 is SIMD-optimized and extremely fast. Benchmark to establish a baseline and detect if the hashing strategy changes.

#### Function under test

```rust
hash::hash_bytes(data: &[u8]) -> String
```

#### Benchmark cases

| Name | Input size | What it measures |
|------|-----------|-----------------|
| `hash_1kb` | 1 KB | Small file overhead |
| `hash_10kb` | 10 KB | Typical TS file |
| `hash_100kb` | 100 KB | Large file |
| `hash_1mb` | 1 MB | Very large file |

Use `vec![b'x'; size]` for deterministic inputs. Pure, no I/O.

---

### 4. Scorer (`benches/scorer.rs`)

The scorer is called on every `codemap context` query. Phase 1 (keyword matching) is CPU-bound; Phase 2 (graph expansion) hits the DB.

#### Functions under test

```rust
scorer::tokenize_query(query: &str) -> Vec<String>
scorer::score_files(keywords: &[String], files: &[FileWithExportsAndEnrichment], conn: &Connection) -> Vec<ScoredFile>
```

#### Benchmark cases

| Name | Files | Keywords | Enrichment | What it measures |
|------|-------|----------|-----------|-----------------|
| `tokenize_short` | — | 3-word query | — | Tokenizer baseline |
| `tokenize_long` | — | 20-word query | — | Stop-word filtering at scale |
| `score_50_files` | 50 | 3 keywords | No | Small project scoring |
| `score_200_files` | 200 | 3 keywords | No | Medium project |
| `score_500_files` | 500 | 3 keywords | No | Large project |
| `score_500_enriched` | 500 | 3 keywords | Yes | With enrichment matching |
| `score_500_5kw` | 500 | 5 keywords | Yes | More keywords = more matching |

#### Setup

Use in-memory SQLite (`init_db(":memory:")`) populated with synthetic files and edges.
The `init_test_db()` function is `#[cfg(test)]` only, so benchmarks should call
`db::init_db(":memory:")` directly (it's public).

```rust
fn gen_files(n: usize, with_enrichment: bool) -> Vec<FileWithExportsAndEnrichment> {
    (0..n).map(|i| FileWithExportsAndEnrichment {
        path: format!("src/mod{i}/index.ts"),
        rank: (n - i) as f64 / n as f64,
        exports: vec![format!("export{i}"), format!("helper{i}"), format!("Type{i}")],
        summary_enriched: if with_enrichment {
            Some(format!("Module {i} handles feature {i} processing"))
        } else { None },
        when_to_use_enriched: if with_enrichment {
            Some(format!("Modify when changing feature {i} behavior"))
        } else { None },
    }).collect()
}

fn populate_db(conn: &Connection, n: usize) {
    // Insert files and edges so Phase 2 graph expansion works
    for i in 0..n {
        db::upsert_file(conn, &format!("src/mod{i}/index.ts"), &format!("hash{i}"), ...);
    }
    // Add some edges between files
    ...
}
```

---

### 5. Database operations (`benches/db.rs`)

Benchmark the hot path DB operations. Use in-memory SQLite to isolate SQL cost from disk I/O.

#### Functions under test

```rust
db::upsert_file(conn, path, hash, rank) -> Result<i64>
db::insert_symbols(conn, file_id, symbols) -> Result<()>
db::insert_edges(conn, source_id, edges) -> Result<()>
db::query_symbols(conn, pattern, limit, exact) -> Result<Vec<SymbolResult>>
db::get_file_deps(conn, file_path, direction) -> Result<Vec<DepEdge>>
db::get_all_files_with_exports_and_enrichment(conn) -> Result<Vec<...>>
```

#### Benchmark cases

| Name | Operation | Scale | What it measures |
|------|-----------|-------|-----------------|
| `upsert_single` | `upsert_file` | 1 row | Single insert baseline |
| `upsert_batch_100` | `upsert_file` × 100 | 100 rows | Bulk insert in transaction |
| `insert_symbols_10` | `insert_symbols` | 10 symbols | Typical file |
| `insert_symbols_50` | `insert_symbols` | 50 symbols | Large file |
| `query_symbol_exact` | `query_symbols` exact | 1000 files in DB | Exact name lookup |
| `query_symbol_like` | `query_symbols` LIKE | 1000 files in DB | Substring search |
| `get_deps_imports` | `get_file_deps` | 1000 files, 2000 edges | Import lookup |
| `get_deps_importers` | `get_file_deps` | 1000 files, 2000 edges | Reverse lookup |
| `load_all_files_100` | `get_all_files_with_exports_and_enrichment` | 100 files | Context command data load |
| `load_all_files_1000` | `get_all_files_with_exports_and_enrichment` | 1000 files | Large project data load |

All use `:memory:` SQLite with synthetic data populated in setup.

---

### 6. End-to-end index (`benches/index.rs`)

Full pipeline benchmark using the test fixture. This is slower and noisier but catches real regressions.

#### Function under test

```rust
index::run_index(root: &Path, force: bool, incremental: bool) -> Result<()>
```

#### Benchmark cases

| Name | Fixture | Mode | What it measures |
|------|---------|------|-----------------|
| `index_simple_full` | `tests/fixtures/simple/` (3 files) | force=true | Minimum pipeline |
| `index_simple_incremental` | Same, after initial index | incremental=true | Incremental skip path |
| `index_medium_full` | Generated 50-file fixture | force=true | Realistic project |
| `index_medium_incremental` | Same, no changes | incremental=true | No-op incremental cost |

The medium fixture is generated in bench setup using `tempfile::TempDir`:

```rust
fn create_medium_fixture() -> TempDir {
    let dir = TempDir::new().unwrap();
    for i in 0..50 {
        let path = dir.path().join(format!("src/mod{i}.ts"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, gen_ts_source(3, 5, 2)).unwrap();
    }
    dir
}
```

---

## File structure

```
benches/
  parser.rs       # parse + signature extraction
  graph.rs        # graph construction + PageRank
  hash.rs         # blake3 hashing
  scorer.rs       # tokenize + score_files
  db.rs           # upsert, insert, query operations
  index.rs        # end-to-end pipeline
  helpers.rs      # shared synthetic data generators (gen_ts_source, gen_random_dag, etc.)
```

## Cargo.toml additions

```toml
[dev-dependencies]
criterion = { version = "0.5", features = ["html_reports"] }

[[bench]]
name = "parser"
harness = false

[[bench]]
name = "graph"
harness = false

[[bench]]
name = "hash"
harness = false

[[bench]]
name = "scorer"
harness = false

[[bench]]
name = "db"
harness = false

[[bench]]
name = "index"
harness = false
```

## Running benchmarks

```bash
# Run all benchmarks
cargo bench

# Run a specific group
cargo bench --bench parser

# Run a specific case
cargo bench --bench graph -- rank_1000

# Compare against baseline (criterion stores in target/criterion/)
cargo bench -- --save-baseline main
# ... make changes ...
cargo bench -- --baseline main
```

## History integration

Criterion stores HTML reports in `target/criterion/` but these are wiped on `cargo clean`.
For durable tracking, export criterion results to the eval history DB:

```bash
# After running benchmarks, archive to history.db
cargo bench --bench parser -- --output-format json > /tmp/bench.json
codemap-eval archive-bench --input /tmp/bench.json
```

This is a stretch goal — criterion's built-in comparison is sufficient for local development.
The main value of Layer 1 is catching regressions during development, not long-term tracking
(that's what Layer 2 handles for quality, and Layer 3 for end-to-end effectiveness).

## Priority order

1. **parser.rs** — most impactful, highest CPU cost
2. **graph.rs** — PageRank scaling is critical as project size grows
3. **scorer.rs** — directly affects `codemap context` latency
4. **db.rs** — catches SQLite query regressions
5. **hash.rs** — baseline, unlikely to change but cheap to add
6. **index.rs** — end-to-end, noisiest but most realistic

## Notes

- All benchmarks use `black_box()` to prevent compiler optimization of unused results
- Pure functions (parser, graph, hash) are the most stable benchmarks
- I/O benchmarks (db, scorer, index) use in-memory SQLite or tempdir to reduce noise
- Criterion default: 100 iterations with statistical analysis (mean, std, confidence intervals)
- Use `criterion_group!` with `sample_size(10)` for slow benchmarks (index end-to-end)

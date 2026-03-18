#![allow(dead_code)]

use codemap::db;
use codemap::graph::{DependencyGraph, EdgeKind};
use rusqlite::Connection;

/// Generate synthetic TypeScript source with the given number of imports, functions, and interfaces.
pub fn gen_ts_source(imports: usize, functions: usize, interfaces: usize) -> String {
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

/// Generate a linear chain graph: file0 -> file1 -> ... -> file(n-1).
pub fn gen_chain(n: usize) -> DependencyGraph {
    let mut g = DependencyGraph::new();
    for i in 0..n {
        g.add_file(&format!("file{i}.ts"));
    }
    for i in 0..n - 1 {
        g.add_edge(
            &format!("file{i}.ts"),
            &format!("file{}.ts", i + 1),
            EdgeKind::Import,
        );
    }
    g
}

/// Simple deterministic PRNG (xorshift64).
struct SimpleRng(u64);

impl SimpleRng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }
    fn next(&mut self) -> usize {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x as usize
    }
}

/// Generate a random DAG with the given number of nodes and edges.
pub fn gen_random_dag(nodes: usize, edges: usize, seed: u64) -> DependencyGraph {
    let mut g = DependencyGraph::new();
    for i in 0..nodes {
        g.add_file(&format!("src/mod{i}/index.ts"));
    }
    let mut rng = SimpleRng::new(seed);
    for _ in 0..edges {
        let from = rng.next() % nodes;
        let to = rng.next() % nodes;
        if from != to {
            g.add_edge(
                &format!("src/mod{from}/index.ts"),
                &format!("src/mod{to}/index.ts"),
                EdgeKind::Import,
            );
        }
    }
    g
}

/// Generate a star graph: one hub with (n-1) spokes.
pub fn gen_star(n: usize) -> DependencyGraph {
    let mut g = DependencyGraph::new();
    g.add_file("hub.ts");
    for i in 1..n {
        g.add_file(&format!("spoke{i}.ts"));
        g.add_edge(&format!("spoke{i}.ts"), "hub.ts", EdgeKind::Import);
    }
    g
}

/// Generate synthetic symbol tuples for DB insertion.
pub fn gen_symbols(
    count: usize,
    file_index: usize,
) -> Vec<(String, String, bool, Option<i32>, usize)> {
    (0..count)
        .map(|j| {
            (
                format!("symbol{file_index}_{j}"),
                "function".to_string(),
                true,
                Some((j * 10) as i32),
                j + 1,
            )
        })
        .collect()
}

/// Generate synthetic files for scorer benchmarks.
pub fn gen_files(n: usize, with_enrichment: bool) -> Vec<db::FileWithExportsAndEnrichment> {
    (0..n)
        .map(|i| db::FileWithExportsAndEnrichment {
            path: format!("src/mod{i}/index.ts"),
            rank: (n - i) as f64 / n as f64,
            exports: vec![
                format!("export{i}"),
                format!("helper{i}"),
                format!("Type{i}"),
            ],
            summary_enriched: if with_enrichment {
                Some(format!("Module {i} handles feature {i} processing"))
            } else {
                None
            },
            when_to_use_enriched: if with_enrichment {
                Some(format!("Modify when changing feature {i} behavior"))
            } else {
                None
            },
        })
        .collect()
}

/// Populate an in-memory DB with synthetic files and edges for scorer/db benchmarks.
pub fn populate_db(conn: &Connection, n: usize) {
    for i in 0..n {
        let path = format!("src/mod{i}/index.ts");
        let hash = format!("hash{i}");
        let rank = (n - i) as f64 / n as f64;
        let file_id = db::upsert_file(conn, &path, &hash, rank).unwrap();

        let symbols = gen_symbols(5, i);
        db::insert_symbols(conn, file_id, &symbols).unwrap();
    }

    // Add random edges between files
    let mut rng = SimpleRng::new(42);
    for _ in 0..n * 2 {
        let from = rng.next() % n;
        let to = rng.next() % n;
        if from != to {
            let from_path = format!("src/mod{from}/index.ts");
            let to_path = format!("src/mod{to}/index.ts");
            // Get file IDs by querying
            let from_id: i64 = conn
                .query_row(
                    "SELECT id FROM files WHERE path = ?1",
                    [&from_path],
                    |row| row.get(0),
                )
                .unwrap();
            let to_id: i64 = conn
                .query_row("SELECT id FROM files WHERE path = ?1", [&to_path], |row| {
                    row.get(0)
                })
                .unwrap();
            db::insert_edges(
                conn,
                from_id,
                &[(to_id, "import".to_string(), Some("thing".to_string()))],
            )
            .unwrap();
        }
    }
}

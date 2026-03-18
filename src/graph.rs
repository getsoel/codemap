use petgraph::Direction;
/// Dependency graph and PageRank with petgraph.
use petgraph::algo::page_rank;
use petgraph::graph::{DiGraph, NodeIndex};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug)]
pub enum EdgeKind {
    Import,
    ReExport,
    TypeImport,
}

pub struct DependencyGraph {
    pub graph: DiGraph<String, EdgeKind>,
    pub path_to_node: HashMap<String, NodeIndex>,
}

impl Default for DependencyGraph {
    fn default() -> Self {
        Self {
            graph: DiGraph::new(),
            path_to_node: HashMap::new(),
        }
    }
}

impl DependencyGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_file(&mut self, path: &str) -> NodeIndex {
        *self
            .path_to_node
            .entry(path.to_string())
            .or_insert_with(|| self.graph.add_node(path.to_string()))
    }

    pub fn add_edge(&mut self, from: &str, to: &str, kind: EdgeKind) {
        let from_idx = self.add_file(from);
        let to_idx = self.add_file(to);
        self.graph.add_edge(from_idx, to_idx, kind);
    }

    /// PageRank (damping 0.85, 100 iterations) boosted by in-degree.
    /// Scores are N-relative × log2(importers + 2) to counter rank-sink effects
    /// where leaf nodes with few importers accumulate disproportionate rank.
    pub fn compute_ranks(&self) -> Vec<(String, f64)> {
        let scores = page_rank(&self.graph, 0.85_f64, 100);
        let n = self.graph.node_count() as f64;
        let mut ranked: Vec<(String, f64)> = self
            .graph
            .node_indices()
            .map(|idx| {
                let pr = scores[idx.index()] * n;
                let in_deg = self.graph.edges_directed(idx, Direction::Incoming).count() as f64;
                (self.graph[idx].clone(), pr * (in_deg + 2.0_f64).log2())
            })
            .collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        ranked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_file_is_idempotent() {
        let mut g = DependencyGraph::new();
        let idx1 = g.add_file("a.ts");
        let idx2 = g.add_file("a.ts");
        assert_eq!(idx1, idx2);
        assert_eq!(g.graph.node_count(), 1);
    }

    #[test]
    fn add_edge_creates_nodes_and_edge() {
        let mut g = DependencyGraph::new();
        g.add_edge("a.ts", "b.ts", EdgeKind::Import);
        assert_eq!(g.graph.node_count(), 2);
        assert_eq!(g.graph.edge_count(), 1);
    }

    #[test]
    fn compute_ranks_single_node() {
        let mut g = DependencyGraph::new();
        g.add_file("a.ts");
        let ranks = g.compute_ranks();
        assert_eq!(ranks.len(), 1);
        assert!(ranks[0].1 > 0.0);
    }

    #[test]
    fn compute_ranks_star_topology() {
        // Many nodes importing one central node — center should rank highest
        let mut g = DependencyGraph::new();
        for i in 0..5 {
            g.add_edge(&format!("leaf{i}.ts"), "center.ts", EdgeKind::Import);
        }
        let ranks = g.compute_ranks();
        assert_eq!(ranks[0].0, "center.ts");
    }

    #[test]
    fn compute_ranks_chain() {
        // a -> b -> c: c should rank highest (most transitively imported)
        let mut g = DependencyGraph::new();
        g.add_edge("a.ts", "b.ts", EdgeKind::Import);
        g.add_edge("b.ts", "c.ts", EdgeKind::Import);
        let ranks = g.compute_ranks();
        assert_eq!(ranks[0].0, "c.ts");
    }

    #[test]
    fn compute_ranks_disconnected_equal() {
        let mut g = DependencyGraph::new();
        g.add_file("a.ts");
        g.add_file("b.ts");
        let ranks = g.compute_ranks();
        assert_eq!(ranks.len(), 2);
        // Disconnected nodes should have equal rank
        assert!((ranks[0].1 - ranks[1].1).abs() < 1e-10);
    }

    #[test]
    fn empty_graph() {
        let g = DependencyGraph::new();
        let ranks = g.compute_ranks();
        assert!(ranks.is_empty());
    }
}

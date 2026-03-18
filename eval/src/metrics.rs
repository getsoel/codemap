/// Information retrieval metrics for evaluating scorer quality.
use std::collections::{HashMap, HashSet};

/// Precision@k: fraction of top-k results that are relevant.
pub fn precision_at_k(returned: &[String], relevant: &HashSet<String>, k: usize) -> f64 {
    let n = returned.len().min(k);
    if n == 0 {
        return 0.0;
    }
    let hits = returned
        .iter()
        .take(k)
        .filter(|r| relevant.contains(r.as_str()))
        .count();
    hits as f64 / n as f64
}

/// Recall@k: fraction of relevant files that appear in top-k results.
pub fn recall_at_k(returned: &[String], relevant: &HashSet<String>, k: usize) -> f64 {
    if relevant.is_empty() {
        return 0.0;
    }
    let top_k: HashSet<&str> = returned.iter().take(k).map(|s| s.as_str()).collect();
    let hits = relevant
        .iter()
        .filter(|r| top_k.contains(r.as_str()))
        .count();
    hits as f64 / relevant.len() as f64
}

/// Mean Reciprocal Rank: 1/position of first relevant result (0 if none found).
pub fn reciprocal_rank(returned: &[String], relevant: &HashSet<String>) -> f64 {
    for (i, path) in returned.iter().enumerate() {
        if relevant.contains(path.as_str()) {
            return 1.0 / (i as f64 + 1.0);
        }
    }
    0.0
}

/// NDCG@k: Normalized Discounted Cumulative Gain using graded relevance.
/// `relevance_map` maps file path -> relevance grade (e.g., 1-3).
pub fn ndcg_at_k(returned: &[String], relevance_map: &HashMap<String, u8>, k: usize) -> f64 {
    let dcg = dcg_at_k(returned, relevance_map, k);
    let ideal_dcg = ideal_dcg(relevance_map, k);
    if ideal_dcg == 0.0 {
        return 0.0;
    }
    dcg / ideal_dcg
}

fn dcg_at_k(returned: &[String], relevance_map: &HashMap<String, u8>, k: usize) -> f64 {
    returned
        .iter()
        .take(k)
        .enumerate()
        .map(|(i, path)| {
            let rel = *relevance_map.get(path).unwrap_or(&0) as f64;
            (2.0_f64.powf(rel) - 1.0) / (i as f64 + 2.0).log2()
        })
        .sum()
}

fn ideal_dcg(relevance_map: &HashMap<String, u8>, k: usize) -> f64 {
    let mut rels: Vec<u8> = relevance_map.values().copied().collect();
    rels.sort_unstable_by(|a, b| b.cmp(a));
    rels.iter()
        .take(k)
        .enumerate()
        .map(|(i, &rel)| {
            let rel = rel as f64;
            (2.0_f64.powf(rel) - 1.0) / (i as f64 + 2.0).log2()
        })
        .sum()
}

/// Aggregate metrics across multiple eval cases.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AggregateMetrics {
    pub count: usize,
    pub precision_at_5: f64,
    pub precision_at_10: f64,
    pub recall_at_5: f64,
    pub recall_at_10: f64,
    pub mrr: f64,
    pub ndcg_at_10: f64,
}

/// Per-case metrics for detailed reporting.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CaseMetrics {
    pub case_id: String,
    pub query: String,
    pub precision_at_5: f64,
    pub precision_at_10: f64,
    pub recall_at_5: f64,
    pub recall_at_10: f64,
    pub mrr: f64,
    pub ndcg_at_10: f64,
    pub returned_count: usize,
    pub relevant_count: usize,
    pub hits_at_10: usize,
}

impl CaseMetrics {
    pub fn compute(
        case_id: &str,
        query: &str,
        returned: &[String],
        relevance_map: &HashMap<String, u8>,
    ) -> Self {
        let relevant: HashSet<String> = relevance_map.keys().cloned().collect();
        let hits_at_10 = returned
            .iter()
            .take(10)
            .filter(|r| relevant.contains(r.as_str()))
            .count();

        Self {
            case_id: case_id.to_string(),
            query: query.to_string(),
            precision_at_5: precision_at_k(returned, &relevant, 5),
            precision_at_10: precision_at_k(returned, &relevant, 10),
            recall_at_5: recall_at_k(returned, &relevant, 5),
            recall_at_10: recall_at_k(returned, &relevant, 10),
            mrr: reciprocal_rank(returned, &relevant),
            ndcg_at_10: ndcg_at_k(returned, relevance_map, 10),
            returned_count: returned.len(),
            relevant_count: relevant.len(),
            hits_at_10,
        }
    }
}

/// Aggregate a list of per-case metrics into a summary.
pub fn aggregate(cases: &[CaseMetrics]) -> AggregateMetrics {
    let n = cases.len();
    if n == 0 {
        return AggregateMetrics {
            count: 0,
            precision_at_5: 0.0,
            precision_at_10: 0.0,
            recall_at_5: 0.0,
            recall_at_10: 0.0,
            mrr: 0.0,
            ndcg_at_10: 0.0,
        };
    }

    let mean = |f: fn(&CaseMetrics) -> f64| -> f64 { cases.iter().map(f).sum::<f64>() / n as f64 };

    AggregateMetrics {
        count: n,
        precision_at_5: mean(|c| c.precision_at_5),
        precision_at_10: mean(|c| c.precision_at_10),
        recall_at_5: mean(|c| c.recall_at_5),
        recall_at_10: mean(|c| c.recall_at_10),
        mrr: mean(|c| c.mrr),
        ndcg_at_10: mean(|c| c.ndcg_at_10),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precision_perfect() {
        let returned = vec!["a".into(), "b".into(), "c".into()];
        let relevant: HashSet<String> = ["a", "b", "c"].into_iter().map(Into::into).collect();
        assert_eq!(precision_at_k(&returned, &relevant, 3), 1.0);
    }

    #[test]
    fn precision_half() {
        let returned = vec!["a".into(), "x".into(), "b".into(), "y".into()];
        let relevant: HashSet<String> = ["a", "b"].into_iter().map(Into::into).collect();
        assert_eq!(precision_at_k(&returned, &relevant, 4), 0.5);
    }

    #[test]
    fn precision_empty_returned() {
        let returned: Vec<String> = vec![];
        let relevant: HashSet<String> = ["a"].into_iter().map(Into::into).collect();
        assert_eq!(precision_at_k(&returned, &relevant, 5), 0.0);
    }

    #[test]
    fn recall_perfect() {
        let returned = vec!["a".into(), "b".into(), "c".into()];
        let relevant: HashSet<String> = ["a", "b"].into_iter().map(Into::into).collect();
        assert_eq!(recall_at_k(&returned, &relevant, 3), 1.0);
    }

    #[test]
    fn recall_partial() {
        let returned = vec!["a".into(), "x".into()];
        let relevant: HashSet<String> = ["a", "b"].into_iter().map(Into::into).collect();
        assert_eq!(recall_at_k(&returned, &relevant, 2), 0.5);
    }

    #[test]
    fn recall_empty_relevant() {
        let returned = vec!["a".into()];
        let relevant: HashSet<String> = HashSet::new();
        assert_eq!(recall_at_k(&returned, &relevant, 5), 0.0);
    }

    #[test]
    fn rr_first_position() {
        let returned = vec!["a".into(), "b".into()];
        let relevant: HashSet<String> = ["a"].into_iter().map(Into::into).collect();
        assert_eq!(reciprocal_rank(&returned, &relevant), 1.0);
    }

    #[test]
    fn rr_second_position() {
        let returned = vec!["x".into(), "a".into()];
        let relevant: HashSet<String> = ["a"].into_iter().map(Into::into).collect();
        assert_eq!(reciprocal_rank(&returned, &relevant), 0.5);
    }

    #[test]
    fn rr_not_found() {
        let returned = vec!["x".into(), "y".into()];
        let relevant: HashSet<String> = ["a"].into_iter().map(Into::into).collect();
        assert_eq!(reciprocal_rank(&returned, &relevant), 0.0);
    }

    #[test]
    fn ndcg_perfect_ordering() {
        let returned = vec!["a".into(), "b".into(), "c".into()];
        let mut rel_map = HashMap::new();
        rel_map.insert("a".into(), 3);
        rel_map.insert("b".into(), 2);
        rel_map.insert("c".into(), 1);
        let score = ndcg_at_k(&returned, &rel_map, 3);
        assert!(
            (score - 1.0).abs() < 1e-10,
            "Perfect ordering should give NDCG=1.0, got {score}"
        );
    }

    #[test]
    fn ndcg_reversed_ordering() {
        let returned = vec!["c".into(), "b".into(), "a".into()];
        let mut rel_map = HashMap::new();
        rel_map.insert("a".into(), 3);
        rel_map.insert("b".into(), 2);
        rel_map.insert("c".into(), 1);
        let score = ndcg_at_k(&returned, &rel_map, 3);
        assert!(
            score < 1.0,
            "Reversed ordering should give NDCG < 1.0, got {score}"
        );
        assert!(score > 0.0);
    }

    #[test]
    fn ndcg_no_relevant() {
        let returned = vec!["x".into(), "y".into()];
        let rel_map = HashMap::new();
        assert_eq!(ndcg_at_k(&returned, &rel_map, 2), 0.0);
    }
}

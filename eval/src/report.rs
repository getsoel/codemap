/// Output formatting for eval results.
use crate::history::{self, HistoricRun};
use crate::metrics::{AggregateMetrics, CaseMetrics};

pub fn print_table(dataset: &str, language: &str, cases: &[CaseMetrics], agg: &AggregateMetrics) {
    println!("Dataset: {dataset} ({language}, {} cases)", agg.count);
    println!("{}", "─".repeat(78));
    println!(
        "  {:30} {:>6} {:>6} {:>6} {:>6} {:>8}",
        "Query", "P@5", "P@10", "R@10", "MRR", "NDCG@10"
    );
    println!("{}", "─".repeat(78));

    for c in cases {
        let query_display = if c.query.len() > 28 {
            format!("{}...", &c.query[..25])
        } else {
            c.query.clone()
        };
        println!(
            "  {:30} {:>5.2} {:>6.2} {:>6.2} {:>6.2} {:>8.2}",
            query_display, c.precision_at_5, c.precision_at_10, c.recall_at_10, c.mrr, c.ndcg_at_10,
        );
    }

    println!("{}", "─".repeat(78));
    println!(
        "  {:30} {:>5.2} {:>6.2} {:>6.2} {:>6.2} {:>8.2}",
        "MEAN", agg.precision_at_5, agg.precision_at_10, agg.recall_at_10, agg.mrr, agg.ndcg_at_10,
    );
    println!();
}

pub fn print_comparison(dataset: &str, current: &AggregateMetrics, baseline: &HistoricRun) {
    let b = &baseline.metrics;
    let date = if baseline.timestamp.len() >= 10 {
        &baseline.timestamp[..10]
    } else {
        &baseline.timestamp
    };

    println!(
        "Dataset: {dataset} (comparing against {} @ {date})",
        baseline.git_commit,
    );
    println!("{}", "─".repeat(56));
    println!(
        "  {:14} {:>8} {:>8} {:>8} {:>8}",
        "Metric", "Before", "After", "Delta", "%"
    );
    println!("{}", "─".repeat(56));

    let metrics: Vec<(&str, f64, f64)> = vec![
        ("MRR", history::json_f64(b, "mrr"), current.mrr),
        (
            "P@5",
            history::json_f64(b, "precision_at_5"),
            current.precision_at_5,
        ),
        (
            "P@10",
            history::json_f64(b, "precision_at_10"),
            current.precision_at_10,
        ),
        (
            "R@5",
            history::json_f64(b, "recall_at_5"),
            current.recall_at_5,
        ),
        (
            "R@10",
            history::json_f64(b, "recall_at_10"),
            current.recall_at_10,
        ),
        (
            "NDCG@10",
            history::json_f64(b, "ndcg_at_10"),
            current.ndcg_at_10,
        ),
    ];

    for (name, before, after) in &metrics {
        let delta = after - before;
        let pct = if *before > 0.0 {
            delta / before * 100.0
        } else {
            0.0
        };
        println!(
            "  {:14} {:>8.3} {:>8.3} {:>7.3} {:>+7.1}%",
            name, before, after, delta, pct,
        );
    }

    println!("{}", "─".repeat(56));
    println!();
}

pub fn print_json(
    dataset: &str,
    language: &str,
    cases: &[CaseMetrics],
    agg: &AggregateMetrics,
    git_commit: &str,
) {
    let output = serde_json::json!({
        "dataset": dataset,
        "language": language,
        "git_commit": git_commit,
        "aggregate": agg,
        "cases": cases,
    });
    println!("{}", serde_json::to_string_pretty(&output).unwrap());
}

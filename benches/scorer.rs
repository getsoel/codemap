mod helpers;

use codemap::{db, scorer};
use criterion::{Criterion, black_box, criterion_group, criterion_main};

fn bench_scorer(c: &mut Criterion) {
    // Tokenizer benchmarks
    c.bench_function("tokenize_short", |b| {
        b.iter(|| scorer::tokenize_query(black_box("parse file imports")));
    });

    c.bench_function("tokenize_long", |b| {
        b.iter(|| {
            scorer::tokenize_query(black_box(
                "how does the parser handle import resolution for typescript files with complex re-exports and namespace imports in the dependency graph",
            ))
        });
    });

    // Score benchmarks
    let kw3: Vec<String> = vec!["parse".into(), "import".into(), "graph".into()];
    let kw5: Vec<String> = vec![
        "parse".into(),
        "import".into(),
        "graph".into(),
        "export".into(),
        "module".into(),
    ];

    c.bench_function("score_50_files", |b| {
        let conn = db::init_db(":memory:").unwrap();
        helpers::populate_db(&conn, 50);
        let files = helpers::gen_files(50, false);
        b.iter(|| scorer::score_files(black_box(&kw3), black_box(&files), &conn));
    });

    c.bench_function("score_200_files", |b| {
        let conn = db::init_db(":memory:").unwrap();
        helpers::populate_db(&conn, 200);
        let files = helpers::gen_files(200, false);
        b.iter(|| scorer::score_files(black_box(&kw3), black_box(&files), &conn));
    });

    c.bench_function("score_500_files", |b| {
        let conn = db::init_db(":memory:").unwrap();
        helpers::populate_db(&conn, 500);
        let files = helpers::gen_files(500, false);
        b.iter(|| scorer::score_files(black_box(&kw3), black_box(&files), &conn));
    });

    c.bench_function("score_500_enriched", |b| {
        let conn = db::init_db(":memory:").unwrap();
        helpers::populate_db(&conn, 500);
        let files = helpers::gen_files(500, true);
        b.iter(|| scorer::score_files(black_box(&kw3), black_box(&files), &conn));
    });

    c.bench_function("score_500_5kw", |b| {
        let conn = db::init_db(":memory:").unwrap();
        helpers::populate_db(&conn, 500);
        let files = helpers::gen_files(500, true);
        b.iter(|| scorer::score_files(black_box(&kw5), black_box(&files), &conn));
    });
}

criterion_group!(benches, bench_scorer);
criterion_main!(benches);

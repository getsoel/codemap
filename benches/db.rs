mod helpers;

use codemap::db;
use criterion::{Criterion, black_box, criterion_group, criterion_main};

fn bench_db(c: &mut Criterion) {
    // Upsert benchmarks
    c.bench_function("upsert_single", |b| {
        let conn = db::init_db(":memory:").unwrap();
        b.iter(|| {
            black_box(
                db::upsert_file(&conn, black_box("src/file.ts"), black_box("hash0"), 0.5).unwrap(),
            );
        });
    });

    c.bench_function("upsert_batch_100", |b| {
        let conn = db::init_db(":memory:").unwrap();
        b.iter(|| {
            for i in 0..100 {
                db::upsert_file(
                    &conn,
                    &format!("src/file{i}.ts"),
                    &format!("hash{i}"),
                    i as f64 / 100.0,
                )
                .unwrap();
            }
        });
    });

    // Insert symbols benchmarks
    c.bench_function("insert_symbols_10", |b| {
        let conn = db::init_db(":memory:").unwrap();
        let file_id = db::upsert_file(&conn, "src/test.ts", "hash0", 0.5).unwrap();
        let symbols = helpers::gen_symbols(10, 0);
        b.iter(|| {
            db::insert_symbols(&conn, black_box(file_id), black_box(&symbols)).unwrap();
        });
    });

    c.bench_function("insert_symbols_50", |b| {
        let conn = db::init_db(":memory:").unwrap();
        let file_id = db::upsert_file(&conn, "src/test.ts", "hash0", 0.5).unwrap();
        let symbols = helpers::gen_symbols(50, 0);
        b.iter(|| {
            db::insert_symbols(&conn, black_box(file_id), black_box(&symbols)).unwrap();
        });
    });

    // Query benchmarks (pre-populate DB)
    c.bench_function("query_symbol_exact", |b| {
        let conn = db::init_db(":memory:").unwrap();
        helpers::populate_db(&conn, 1000);
        b.iter(|| {
            db::query_symbols(
                &conn,
                black_box("symbol500_2"),
                black_box(10),
                black_box(true),
            )
            .unwrap();
        });
    });

    c.bench_function("query_symbol_like", |b| {
        let conn = db::init_db(":memory:").unwrap();
        helpers::populate_db(&conn, 1000);
        b.iter(|| {
            db::query_symbols(&conn, black_box("symbol5"), black_box(10), black_box(false))
                .unwrap();
        });
    });

    // Dependency lookup benchmarks
    c.bench_function("get_deps_imports", |b| {
        let conn = db::init_db(":memory:").unwrap();
        helpers::populate_db(&conn, 1000);
        b.iter(|| {
            db::get_file_deps(&conn, black_box("src/mod50/index.ts"), black_box("imports"))
                .unwrap();
        });
    });

    c.bench_function("get_deps_importers", |b| {
        let conn = db::init_db(":memory:").unwrap();
        helpers::populate_db(&conn, 1000);
        b.iter(|| {
            db::get_file_deps(
                &conn,
                black_box("src/mod50/index.ts"),
                black_box("importers"),
            )
            .unwrap();
        });
    });

    // Load all files benchmarks
    c.bench_function("load_all_files_100", |b| {
        let conn = db::init_db(":memory:").unwrap();
        helpers::populate_db(&conn, 100);
        b.iter(|| {
            db::get_all_files_with_exports_and_enrichment(black_box(&conn)).unwrap();
        });
    });

    c.bench_function("load_all_files_1000", |b| {
        let conn = db::init_db(":memory:").unwrap();
        helpers::populate_db(&conn, 1000);
        b.iter(|| {
            db::get_all_files_with_exports_and_enrichment(black_box(&conn)).unwrap();
        });
    });
}

criterion_group!(benches, bench_db);
criterion_main!(benches);

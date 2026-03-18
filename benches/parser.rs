mod helpers;

use codemap::parser;
use criterion::{Criterion, black_box, criterion_group, criterion_main};
use std::path::Path;

fn bench_parser(c: &mut Criterion) {
    let small = helpers::gen_ts_source(2, 3, 0);
    let medium = helpers::gen_ts_source(10, 15, 0);
    let large = helpers::gen_ts_source(30, 40, 0);
    let types_heavy = helpers::gen_ts_source(5, 0, 50);

    let path = Path::new("bench_input.ts");

    c.bench_function("parse_small", |b| {
        b.iter(|| black_box(parser::analyze_file(black_box(path), black_box(&small)).unwrap()));
    });

    c.bench_function("parse_medium", |b| {
        b.iter(|| black_box(parser::analyze_file(black_box(path), black_box(&medium)).unwrap()));
    });

    c.bench_function("parse_large", |b| {
        b.iter(|| black_box(parser::analyze_file(black_box(path), black_box(&large)).unwrap()));
    });

    c.bench_function("parse_types_heavy", |b| {
        b.iter(|| {
            black_box(parser::analyze_file(black_box(path), black_box(&types_heavy)).unwrap())
        });
    });

    c.bench_function("signatures_medium", |b| {
        b.iter(|| {
            black_box(parser::extract_signatures(
                black_box(path),
                black_box(&medium),
            ))
        });
    });

    c.bench_function("signatures_large", |b| {
        b.iter(|| {
            black_box(parser::extract_signatures(
                black_box(path),
                black_box(&large),
            ))
        });
    });
}

criterion_group!(benches, bench_parser);
criterion_main!(benches);

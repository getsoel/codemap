mod helpers;

use criterion::{Criterion, black_box, criterion_group, criterion_main};

fn bench_graph(c: &mut Criterion) {
    c.bench_function("rank_10_chain", |b| {
        let g = helpers::gen_chain(10);
        b.iter(|| black_box(g.compute_ranks()));
    });

    c.bench_function("rank_100_sparse", |b| {
        let g = helpers::gen_random_dag(100, 150, 1);
        b.iter(|| black_box(g.compute_ranks()));
    });

    c.bench_function("rank_100_dense", |b| {
        let g = helpers::gen_random_dag(100, 500, 2);
        b.iter(|| black_box(g.compute_ranks()));
    });

    c.bench_function("rank_500_sparse", |b| {
        let g = helpers::gen_random_dag(500, 800, 3);
        b.iter(|| black_box(g.compute_ranks()));
    });

    c.bench_function("rank_1000_sparse", |b| {
        let g = helpers::gen_random_dag(1000, 2000, 4);
        b.iter(|| black_box(g.compute_ranks()));
    });

    c.bench_function("rank_1000_star", |b| {
        let g = helpers::gen_star(1000);
        b.iter(|| black_box(g.compute_ranks()));
    });

    c.bench_function("rank_2000_realistic", |b| {
        let g = helpers::gen_random_dag(2000, 8000, 5);
        b.iter(|| black_box(g.compute_ranks()));
    });
}

criterion_group!(benches, bench_graph);
criterion_main!(benches);

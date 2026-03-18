use codemap::hash;
use criterion::{Criterion, black_box, criterion_group, criterion_main};

fn bench_hash(c: &mut Criterion) {
    let kb1 = vec![b'x'; 1_024];
    let kb10 = vec![b'x'; 10_240];
    let kb100 = vec![b'x'; 102_400];
    let mb1 = vec![b'x'; 1_048_576];

    c.bench_function("hash_1kb", |b| {
        b.iter(|| hash::hash_bytes(black_box(&kb1)));
    });

    c.bench_function("hash_10kb", |b| {
        b.iter(|| hash::hash_bytes(black_box(&kb10)));
    });

    c.bench_function("hash_100kb", |b| {
        b.iter(|| hash::hash_bytes(black_box(&kb100)));
    });

    c.bench_function("hash_1mb", |b| {
        b.iter(|| hash::hash_bytes(black_box(&mb1)));
    });
}

criterion_group!(benches, bench_hash);
criterion_main!(benches);

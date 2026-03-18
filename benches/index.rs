mod helpers;

use codemap::index;
use criterion::{Criterion, black_box, criterion_group, criterion_main};
use std::path::Path;
use tempfile::TempDir;

fn copy_fixture_to_tmp(fixture: &Path) -> TempDir {
    let dir = TempDir::new().unwrap();
    for entry in std::fs::read_dir(fixture).unwrap() {
        let entry = entry.unwrap();
        let dest = dir.path().join(entry.file_name());
        std::fs::copy(entry.path(), dest).unwrap();
    }
    dir
}

fn create_medium_fixture() -> TempDir {
    let dir = TempDir::new().unwrap();
    for i in 0..50 {
        let path = dir.path().join(format!("src/mod{i}.ts"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, helpers::gen_ts_source(3, 5, 2)).unwrap();
    }
    dir
}

fn bench_index(c: &mut Criterion) {
    let simple_fixture = Path::new("tests/fixtures/simple");

    let mut group = c.benchmark_group("index");
    group.sample_size(10);

    group.bench_function("index_simple_full", |b| {
        let dir = copy_fixture_to_tmp(simple_fixture);
        b.iter(|| index::run_index(black_box(dir.path()), true, false).unwrap());
    });

    group.bench_function("index_simple_incremental", |b| {
        let dir = copy_fixture_to_tmp(simple_fixture);
        index::run_index(dir.path(), true, false).unwrap();
        b.iter(|| index::run_index(black_box(dir.path()), false, true).unwrap());
    });

    group.bench_function("index_medium_full", |b| {
        let dir = create_medium_fixture();
        b.iter(|| index::run_index(black_box(dir.path()), true, false).unwrap());
    });

    group.bench_function("index_medium_incremental", |b| {
        let dir = create_medium_fixture();
        index::run_index(dir.path(), true, false).unwrap();
        b.iter(|| index::run_index(black_box(dir.path()), false, true).unwrap());
    });

    group.finish();
}

criterion_group!(benches, bench_index);
criterion_main!(benches);

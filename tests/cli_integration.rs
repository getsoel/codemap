use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;

fn codemap() -> Command {
    Command::cargo_bin("codemap").unwrap()
}

/// Copy fixture files to a temp dir and run `codemap index`.
fn indexed_fixture_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let fixture_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/simple");
    for entry in fs::read_dir(&fixture_dir).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), dir.path().join(entry.file_name())).unwrap();
    }
    codemap()
        .arg("index")
        .current_dir(dir.path())
        .assert()
        .success();
    dir
}

#[test]
fn version_flag() {
    codemap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("codemap"));
}

#[test]
fn help_flag() {
    codemap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage"));
}

#[test]
fn index_and_map_on_fixture_project() {
    let dir = indexed_fixture_dir();

    // Verify .codemap/index.db was created
    assert!(dir.path().join(".codemap/index.db").exists());

    // Map should produce output
    codemap()
        .arg("map")
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());
}

#[test]
fn map_before_index_errors() {
    let dir = tempfile::tempdir().unwrap();
    codemap()
        .arg("map")
        .current_dir(dir.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("index"));
}

#[test]
fn symbol_json_output() {
    let dir = indexed_fixture_dir();

    codemap()
        .args(["symbol", "greet", "--json"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("greet"));
}

#[test]
fn enrich_stats_json() {
    let dir = indexed_fixture_dir();

    codemap()
        .args(["enrich", "--stats", "--json"])
        .current_dir(dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("total_files"));
}

#[test]
fn enrich_if_available_without_key() {
    let dir = indexed_fixture_dir();

    // --if-available should exit 0 even without API key
    codemap()
        .args(["enrich", "--api", "--if-available"])
        .current_dir(dir.path())
        .env_remove("GEMINI_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .assert()
        .success();
}

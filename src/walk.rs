use ignore::WalkBuilder;
/// File discovery using the ignore crate.
use ignore::types::TypesBuilder;
use std::path::{Path, PathBuf};

pub fn discover_files(root: &Path) -> Vec<PathBuf> {
    let mut types = TypesBuilder::new();
    types.add_defaults();
    types.select("ts");
    types.select("js");
    types.add("tsx", "*.tsx").unwrap();
    types.select("tsx");
    types.add("jsx", "*.jsx").unwrap();
    types.select("jsx");
    let types = types.build().unwrap();

    let mut files = Vec::new();
    for entry in WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .add_custom_ignore_filename(".codemapignore")
        .types(types)
        .build()
        .flatten()
    {
        if entry.file_type().is_some_and(|ft| ft.is_file()) {
            files.push(entry.into_path());
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn discovers_ts_and_js_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("index.ts"), "export const x = 1;").unwrap();
        fs::write(dir.path().join("utils.js"), "module.exports = {};").unwrap();
        fs::write(dir.path().join("app.tsx"), "export default () => null;").unwrap();
        fs::write(dir.path().join("comp.jsx"), "export default () => null;").unwrap();

        let files = discover_files(dir.path());
        assert_eq!(files.len(), 4);
    }

    #[test]
    fn ignores_non_js_ts_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("readme.md"), "# Hello").unwrap();
        fs::write(dir.path().join("style.css"), "body {}").unwrap();
        fs::write(dir.path().join("data.json"), "{}").unwrap();
        fs::write(dir.path().join("app.ts"), "export {};").unwrap();

        let files = discover_files(dir.path());
        assert_eq!(files.len(), 1);
        assert!(files[0].to_str().unwrap().ends_with("app.ts"));
    }

    #[test]
    fn respects_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        // Initialize a git repo so .gitignore is respected
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".gitignore"), "ignored.ts\n").unwrap();
        fs::write(dir.path().join("kept.ts"), "export {};").unwrap();
        fs::write(dir.path().join("ignored.ts"), "export {};").unwrap();

        let files = discover_files(dir.path());
        assert_eq!(files.len(), 1);
        assert!(files[0].to_str().unwrap().ends_with("kept.ts"));
    }

    #[test]
    fn respects_codemapignore() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".codemapignore"), "ignored.ts\n").unwrap();
        fs::write(dir.path().join("kept.ts"), "export {};").unwrap();
        fs::write(dir.path().join("ignored.ts"), "export {};").unwrap();

        let files = discover_files(dir.path());
        assert_eq!(files.len(), 1);
        assert!(files[0].to_str().unwrap().ends_with("kept.ts"));
    }

    #[test]
    fn empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        let files = discover_files(dir.path());
        assert!(files.is_empty());
    }

    #[test]
    fn discovers_nested_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src/components")).unwrap();
        fs::write(dir.path().join("src/index.ts"), "export {};").unwrap();
        fs::write(dir.path().join("src/components/Button.tsx"), "export {};").unwrap();

        let files = discover_files(dir.path());
        assert_eq!(files.len(), 2);
    }
}

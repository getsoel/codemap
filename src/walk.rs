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

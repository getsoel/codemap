/// Import resolution with oxc_resolver.
use oxc_resolver::{ResolveOptions, Resolver, TsconfigDiscovery};
use std::path::Path;

pub fn create_resolver() -> Resolver {
    Resolver::new(ResolveOptions {
        extensions: vec![
            ".ts".into(),
            ".tsx".into(),
            ".js".into(),
            ".jsx".into(),
            ".mjs".into(),
            ".json".into(),
        ],
        extension_alias: vec![
            (
                ".js".into(),
                vec![".ts".into(), ".tsx".into(), ".js".into()],
            ),
            (".mjs".into(), vec![".mts".into(), ".mjs".into()]),
        ],
        condition_names: vec!["node".into(), "import".into()],
        main_fields: vec!["module".into(), "main".into()],
        tsconfig: Some(TsconfigDiscovery::Auto),
        ..ResolveOptions::default()
    })
}

pub fn resolve_import(resolver: &Resolver, from_dir: &Path, specifier: &str) -> Option<String> {
    match resolver.resolve(from_dir, specifier) {
        Ok(resolution) => Some(resolution.full_path().display().to_string()),
        Err(_) => None,
    }
}

/// TypeScript/JavaScript parsing and analysis with oxc.
use crate::types::*;
use oxc::ast::ast::*;
use oxc::semantic::SemanticBuilderReturn;
use oxc::{
    allocator::Allocator,
    parser::{Parser, ParserReturn},
    span::SourceType,
};
use std::path::Path;

pub fn analyze_file(path: &Path, source: &str) -> anyhow::Result<FileAnalysis> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(path)
        .map_err(|_| anyhow::anyhow!("Unsupported file type: {}", path.display()))?;

    // Step 1: Parse → AST
    let ParserReturn {
        program,
        errors,
        panicked,
        ..
    } = Parser::new(&allocator, source, source_type).parse();
    if panicked {
        anyhow::bail!("Parser panicked on {}", path.display());
    }
    if !errors.is_empty() {
        tracing::warn!("{}: {} parse errors", path.display(), errors.len());
    }

    // Step 2: Semantic analysis → symbols, scopes, references
    let SemanticBuilderReturn {
        semantic,
        errors: _sem_errors,
    } = oxc::semantic::SemanticBuilder::new()
        .with_check_syntax_error(true)
        .build(&program);

    // Step 3: Extract imports, exports, and symbols
    let mut analysis = FileAnalysis::default();
    extract_imports_exports(&program, &mut analysis);
    extract_symbols(&semantic, &mut analysis);
    Ok(analysis)
}

fn extract_imports_exports(program: &Program, out: &mut FileAnalysis) {
    for stmt in &program.body {
        match stmt {
            Statement::ImportDeclaration(import) => {
                let source = import.source.value.as_str();
                if let Some(specifiers) = &import.specifiers {
                    for spec in specifiers {
                        match spec {
                            ImportDeclarationSpecifier::ImportSpecifier(s) => {
                                out.imports.push(Import {
                                    source: source.to_string(),
                                    name: s.local.name.to_string(),
                                    kind: ImportKind::Named,
                                });
                            }
                            ImportDeclarationSpecifier::ImportDefaultSpecifier(s) => {
                                out.imports.push(Import {
                                    source: source.to_string(),
                                    name: s.local.name.to_string(),
                                    kind: ImportKind::Default,
                                });
                            }
                            ImportDeclarationSpecifier::ImportNamespaceSpecifier(s) => {
                                out.imports.push(Import {
                                    source: source.to_string(),
                                    name: s.local.name.to_string(),
                                    kind: ImportKind::Namespace,
                                });
                            }
                        }
                    }
                }
            }
            Statement::ExportNamedDeclaration(export) => {
                if let Some(decl) = &export.declaration {
                    match decl {
                        Declaration::FunctionDeclaration(f) => {
                            if let Some(id) = &f.id {
                                out.exports.push(Export {
                                    name: id.name.to_string(),
                                    kind: ExportKind::Function,
                                });
                            }
                        }
                        Declaration::VariableDeclaration(var) => {
                            for d in &var.declarations {
                                if let BindingPattern::BindingIdentifier(id) = &d.id {
                                    out.exports.push(Export {
                                        name: id.name.to_string(),
                                        kind: ExportKind::Variable,
                                    });
                                }
                            }
                        }
                        Declaration::ClassDeclaration(class) => {
                            if let Some(id) = &class.id {
                                out.exports.push(Export {
                                    name: id.name.to_string(),
                                    kind: ExportKind::Class,
                                });
                            }
                        }
                        Declaration::TSInterfaceDeclaration(iface) => {
                            out.exports.push(Export {
                                name: iface.id.name.to_string(),
                                kind: ExportKind::Interface,
                            });
                        }
                        Declaration::TSTypeAliasDeclaration(alias) => {
                            out.exports.push(Export {
                                name: alias.id.name.to_string(),
                                kind: ExportKind::TypeAlias,
                            });
                        }
                        Declaration::TSEnumDeclaration(e) => {
                            out.exports.push(Export {
                                name: e.id.name.to_string(),
                                kind: ExportKind::Enum,
                            });
                        }
                        _ => {}
                    }
                }
                if let Some(source) = &export.source {
                    for spec in &export.specifiers {
                        out.reexports.push(ReExport {
                            source: source.value.to_string(),
                            local: spec.local.to_string(),
                            exported: spec.exported.to_string(),
                        });
                    }
                }
            }
            Statement::ExportDefaultDeclaration(_) => {
                out.exports.push(Export {
                    name: "default".to_string(),
                    kind: ExportKind::Default,
                });
            }
            Statement::ExportAllDeclaration(star) => {
                out.reexports.push(ReExport {
                    source: star.source.value.to_string(),
                    local: "*".to_string(),
                    exported: star
                        .exported
                        .as_ref()
                        .map(|e| e.to_string())
                        .unwrap_or("*".to_string()),
                });
            }
            _ => {}
        }
    }
}

fn extract_symbols(semantic: &oxc::semantic::Semantic, out: &mut FileAnalysis) {
    let scoping = semantic.scoping();
    // Collect exported names from the already-extracted exports
    let exported_names: std::collections::HashSet<&str> =
        out.exports.iter().map(|e| e.name.as_str()).collect();

    for symbol_id in scoping.symbol_ids() {
        let name = scoping.symbol_name(symbol_id).to_string();
        let scope_id = scoping.symbol_scope_id(symbol_id);

        let is_top_level = scope_id == scoping.root_scope_id();
        let is_exported = exported_names.contains(name.as_str());

        if is_exported || is_top_level {
            out.symbols.push(SymbolInfo {
                name,
                is_exported,
                reference_count: scoping.get_resolved_reference_ids(symbol_id).len(),
            });
        }
    }
}

/// Extract signatures from source for the code map output.
/// Returns lines like "export function foo(a: string): void"
pub fn extract_signatures(path: &Path, source: &str) -> Vec<String> {
    let allocator = Allocator::default();
    let source_type = match SourceType::from_path(path) {
        Ok(st) => st,
        Err(_) => return vec![],
    };

    let ParserReturn { program, .. } = Parser::new(&allocator, source, source_type).parse();
    let lines: Vec<&str> = source.lines().collect();
    let mut signatures = Vec::new();

    for stmt in &program.body {
        match stmt {
            Statement::ExportNamedDeclaration(export) => {
                if let Some(decl) = &export.declaration {
                    for sig in declaration_signatures(decl, &lines, true) {
                        signatures.push(sig);
                    }
                }
            }
            Statement::ExportDefaultDeclaration(export) => match &export.declaration {
                ExportDefaultDeclarationKind::FunctionDeclaration(f) => {
                    let name =
                        f.id.as_ref()
                            .map(|id| id.name.to_string())
                            .unwrap_or("default".to_string());
                    let line_idx = f.span.start as usize;
                    let sig = extract_function_sig(&lines, line_idx, &name, true);
                    signatures.push(sig);
                }
                ExportDefaultDeclarationKind::ClassDeclaration(c) => {
                    let name =
                        c.id.as_ref()
                            .map(|id| id.name.to_string())
                            .unwrap_or("default".to_string());
                    signatures.push(format!("export default class {name}"));
                }
                _ => {
                    signatures.push("export default ...".to_string());
                }
            },
            Statement::FunctionDeclaration(f) => {
                if let Some(id) = &f.id {
                    let line_idx = f.span.start as usize;
                    let sig = extract_function_sig(&lines, line_idx, &id.name, false);
                    signatures.push(sig);
                }
            }
            Statement::ClassDeclaration(c) => {
                if let Some(id) = &c.id {
                    signatures.push(format!("class {}", id.name));
                }
            }
            Statement::ExportAllDeclaration(star) => {
                let exported = star
                    .exported
                    .as_ref()
                    .map(|e| format!(" as {e}"))
                    .unwrap_or_default();
                signatures.push(format!(
                    "export *{} from \"{}\"",
                    exported, star.source.value
                ));
            }
            Statement::VariableDeclaration(var) => {
                for d in &var.declarations {
                    if let BindingPattern::BindingIdentifier(id) = &d.id {
                        signatures.push(format!("{} {}", var.kind.as_str(), id.name));
                    }
                }
            }
            Statement::TSInterfaceDeclaration(iface) => {
                signatures.push(format!("interface {}", iface.id.name));
            }
            Statement::TSTypeAliasDeclaration(alias) => {
                let line_num = byte_offset_to_line(source, alias.span.start as usize);
                if let Some(line) = lines.get(line_num) {
                    let truncated = truncate(line.trim(), 100);
                    signatures.push(truncated.to_string());
                } else {
                    signatures.push(format!("type {}", alias.id.name));
                }
            }
            Statement::TSEnumDeclaration(e) => {
                signatures.push(format!("enum {}", e.id.name));
            }
            _ => {}
        }
    }
    signatures
}

fn declaration_signatures(decl: &Declaration, lines: &[&str], exported: bool) -> Vec<String> {
    let prefix = if exported { "export " } else { "" };
    match decl {
        Declaration::FunctionDeclaration(f) => {
            if let Some(id) = &f.id {
                let sig = extract_function_sig(lines, f.span.start as usize, &id.name, exported);
                vec![sig]
            } else {
                vec![]
            }
        }
        Declaration::VariableDeclaration(var) => {
            let mut sigs = Vec::new();
            for d in &var.declarations {
                if let BindingPattern::BindingIdentifier(id) = &d.id {
                    sigs.push(format!("{prefix}{} {}", var.kind.as_str(), id.name));
                }
            }
            sigs
        }
        Declaration::ClassDeclaration(c) => {
            if let Some(id) = &c.id {
                vec![format!("{prefix}class {}", id.name)]
            } else {
                vec![]
            }
        }
        Declaration::TSInterfaceDeclaration(iface) => {
            vec![format!("{prefix}interface {}", iface.id.name)]
        }
        Declaration::TSTypeAliasDeclaration(alias) => {
            vec![format!("{prefix}type {}", alias.id.name)]
        }
        Declaration::TSEnumDeclaration(e) => {
            vec![format!("{prefix}enum {}", e.id.name)]
        }
        _ => vec![],
    }
}

fn extract_function_sig(lines: &[&str], byte_offset: usize, name: &str, exported: bool) -> String {
    let prefix = if exported { "export " } else { "" };

    // Use byte_offset to jump directly to the right line instead of scanning all lines
    let line_num = byte_offset_to_line_from_lines(lines, byte_offset);
    // Check a small window around the target line (the span start may point to a decorator or `export` keyword)
    let start = line_num.saturating_sub(1);
    let end = (line_num + 3).min(lines.len());
    for &line in &lines[start..end] {
        let trimmed = line.trim();
        if trimmed.contains(&format!("function {name}"))
            || trimmed.contains(&format!("function* {name}"))
        {
            let sig = if let Some(brace_pos) = trimmed.find('{') {
                trimmed[..brace_pos].trim()
            } else {
                trimmed
            };
            let sig = truncate(sig, 100);
            if exported && !sig.starts_with("export") {
                return format!("export {sig}");
            }
            return sig.to_string();
        }
    }
    format!("{prefix}function {name}(...)")
}

/// Convert a byte offset to a line number using pre-split lines (avoids needing the original source).
fn byte_offset_to_line_from_lines(lines: &[&str], byte_offset: usize) -> usize {
    let mut bytes_seen = 0usize;
    for (i, line) in lines.iter().enumerate() {
        bytes_seen += line.len() + 1; // +1 for newline
        if bytes_seen > byte_offset {
            return i;
        }
    }
    lines.len().saturating_sub(1)
}

fn byte_offset_to_line(source: &str, offset: usize) -> usize {
    source[..offset.min(source.len())]
        .chars()
        .filter(|&c| c == '\n')
        .count()
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        // Find a valid char boundary at or before `max` to avoid panicking on multi-byte UTF-8
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    // --- analyze_file: imports ---

    #[test]
    fn named_import() {
        let src = r#"import { foo, bar } from './utils';"#;
        let a = analyze_file(Path::new("test.ts"), src).unwrap();
        assert_eq!(a.imports.len(), 2);
        assert_eq!(a.imports[0].name, "foo");
        assert_eq!(a.imports[0].kind, ImportKind::Named);
        assert_eq!(a.imports[1].name, "bar");
        assert_eq!(a.imports[1].source, "./utils");
    }

    #[test]
    fn default_import() {
        let src = r#"import React from 'react';"#;
        let a = analyze_file(Path::new("test.tsx"), src).unwrap();
        assert_eq!(a.imports.len(), 1);
        assert_eq!(a.imports[0].name, "React");
        assert_eq!(a.imports[0].kind, ImportKind::Default);
    }

    #[test]
    fn namespace_import() {
        let src = r#"import * as fs from 'fs';"#;
        let a = analyze_file(Path::new("test.ts"), src).unwrap();
        assert_eq!(a.imports.len(), 1);
        assert_eq!(a.imports[0].name, "fs");
        assert_eq!(a.imports[0].kind, ImportKind::Namespace);
    }

    // --- analyze_file: exports ---

    #[test]
    fn export_function() {
        let src = r#"export function doThing() { return 1; }"#;
        let a = analyze_file(Path::new("test.ts"), src).unwrap();
        assert_eq!(a.exports.len(), 1);
        assert_eq!(a.exports[0].name, "doThing");
        assert_eq!(a.exports[0].kind, ExportKind::Function);
    }

    #[test]
    fn export_variable() {
        let src = r#"export const x = 1;"#;
        let a = analyze_file(Path::new("test.ts"), src).unwrap();
        assert_eq!(a.exports.len(), 1);
        assert_eq!(a.exports[0].name, "x");
        assert_eq!(a.exports[0].kind, ExportKind::Variable);
    }

    #[test]
    fn export_class() {
        let src = r#"export class Foo { bar() {} }"#;
        let a = analyze_file(Path::new("test.ts"), src).unwrap();
        assert_eq!(a.exports.len(), 1);
        assert_eq!(a.exports[0].name, "Foo");
        assert_eq!(a.exports[0].kind, ExportKind::Class);
    }

    #[test]
    fn export_interface() {
        let src = r#"export interface Props { name: string; }"#;
        let a = analyze_file(Path::new("test.ts"), src).unwrap();
        assert_eq!(a.exports.len(), 1);
        assert_eq!(a.exports[0].name, "Props");
        assert_eq!(a.exports[0].kind, ExportKind::Interface);
    }

    #[test]
    fn export_type_alias() {
        let src = r#"export type ID = string;"#;
        let a = analyze_file(Path::new("test.ts"), src).unwrap();
        assert_eq!(a.exports.len(), 1);
        assert_eq!(a.exports[0].name, "ID");
        assert_eq!(a.exports[0].kind, ExportKind::TypeAlias);
    }

    #[test]
    fn export_enum() {
        let src = r#"export enum Color { Red, Green, Blue }"#;
        let a = analyze_file(Path::new("test.ts"), src).unwrap();
        assert_eq!(a.exports.len(), 1);
        assert_eq!(a.exports[0].name, "Color");
        assert_eq!(a.exports[0].kind, ExportKind::Enum);
    }

    #[test]
    fn export_default() {
        let src = r#"export default function() { return 1; }"#;
        let a = analyze_file(Path::new("test.ts"), src).unwrap();
        assert_eq!(a.exports.len(), 1);
        assert_eq!(a.exports[0].name, "default");
        assert_eq!(a.exports[0].kind, ExportKind::Default);
    }

    // --- analyze_file: re-exports ---

    #[test]
    fn reexport_named() {
        let src = r#"export { foo } from './bar';"#;
        let a = analyze_file(Path::new("test.ts"), src).unwrap();
        assert_eq!(a.reexports.len(), 1);
        assert_eq!(a.reexports[0].source, "./bar");
        assert_eq!(a.reexports[0].local, "foo");
        assert_eq!(a.reexports[0].exported, "foo");
    }

    #[test]
    fn reexport_star() {
        let src = r#"export * from './bar';"#;
        let a = analyze_file(Path::new("test.ts"), src).unwrap();
        assert_eq!(a.reexports.len(), 1);
        assert_eq!(a.reexports[0].local, "*");
        assert_eq!(a.reexports[0].exported, "*");
    }

    #[test]
    fn reexport_star_as() {
        let src = r#"export * as utils from './bar';"#;
        let a = analyze_file(Path::new("test.ts"), src).unwrap();
        assert_eq!(a.reexports.len(), 1);
        assert_eq!(a.reexports[0].local, "*");
        assert_eq!(a.reexports[0].exported, "utils");
    }

    // --- analyze_file: edge cases ---

    #[test]
    fn empty_file() {
        let a = analyze_file(Path::new("test.ts"), "").unwrap();
        assert!(a.imports.is_empty());
        assert!(a.exports.is_empty());
        assert!(a.symbols.is_empty());
    }

    #[test]
    fn no_imports_or_exports() {
        let src = r#"const x = 1; function foo() {}"#;
        let a = analyze_file(Path::new("test.ts"), src).unwrap();
        assert!(a.imports.is_empty());
        assert!(a.exports.is_empty());
        // Top-level symbols should still be captured
        assert!(!a.symbols.is_empty());
    }

    #[test]
    fn unsupported_file_type() {
        let result = analyze_file(Path::new("test.py"), "x = 1");
        assert!(result.is_err());
    }

    #[test]
    fn combined_imports_and_exports() {
        let src = r#"
import { useState } from 'react';
import type { FC } from 'react';
export const App: FC = () => null;
export function helper() {}
"#;
        let a = analyze_file(Path::new("test.tsx"), src).unwrap();
        assert_eq!(a.imports.len(), 2);
        assert_eq!(a.exports.len(), 2);
    }

    #[test]
    fn symbol_is_exported_flag() {
        let src = r#"
export function exported() {}
function internal() {}
"#;
        let a = analyze_file(Path::new("test.ts"), src).unwrap();
        let exported_sym = a.symbols.iter().find(|s| s.name == "exported");
        let internal_sym = a.symbols.iter().find(|s| s.name == "internal");
        assert!(exported_sym.is_some_and(|s| s.is_exported));
        assert!(internal_sym.is_some_and(|s| !s.is_exported));
    }

    // --- extract_signatures ---

    #[test]
    fn signature_export_function() {
        let src = r#"export function greet(name: string): string { return name; }"#;
        let sigs = extract_signatures(Path::new("test.ts"), src);
        assert_eq!(sigs.len(), 1);
        assert!(sigs[0].contains("export"));
        assert!(sigs[0].contains("greet"));
        // Should not include function body
        assert!(!sigs[0].contains("return"));
    }

    #[test]
    fn signature_export_class() {
        let src = r#"export class Foo { bar() {} }"#;
        let sigs = extract_signatures(Path::new("test.ts"), src);
        assert_eq!(sigs.len(), 1);
        assert_eq!(sigs[0], "export class Foo");
    }

    #[test]
    fn signature_export_interface() {
        let src = r#"export interface Props { name: string; }"#;
        let sigs = extract_signatures(Path::new("test.ts"), src);
        assert_eq!(sigs.len(), 1);
        assert_eq!(sigs[0], "export interface Props");
    }

    #[test]
    fn signature_export_enum() {
        let src = r#"export enum Color { Red, Green }"#;
        let sigs = extract_signatures(Path::new("test.ts"), src);
        assert_eq!(sigs.len(), 1);
        assert_eq!(sigs[0], "export enum Color");
    }

    #[test]
    fn signature_star_reexport() {
        let src = r#"export * from './utils';"#;
        let sigs = extract_signatures(Path::new("test.ts"), src);
        assert_eq!(sigs.len(), 1);
        assert_eq!(sigs[0], r#"export * from "./utils""#);
    }

    #[test]
    fn signature_export_default_class() {
        let src = r#"export default class Foo {}"#;
        let sigs = extract_signatures(Path::new("test.ts"), src);
        assert_eq!(sigs.len(), 1);
        assert_eq!(sigs[0], "export default class Foo");
    }

    #[test]
    fn signature_unsupported_extension() {
        let sigs = extract_signatures(Path::new("test.py"), "x = 1");
        assert!(sigs.is_empty());
    }

    #[test]
    fn signature_empty_file() {
        let sigs = extract_signatures(Path::new("test.ts"), "");
        assert!(sigs.is_empty());
    }

    // --- truncate ---

    #[test]
    fn truncate_short_string() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_at_limit() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn truncate_long_string() {
        assert_eq!(truncate("hello world", 5), "hello");
    }

    #[test]
    fn truncate_multibyte_utf8() {
        // "héllo" — é is 2 bytes in UTF-8
        let s = "héllo";
        let result = truncate(s, 3);
        // Should not panic, should stop at char boundary
        assert!(result.len() <= 3);
        assert!(result.is_char_boundary(result.len()));
    }

    // --- byte_offset_to_line ---

    #[test]
    fn byte_offset_first_line() {
        assert_eq!(byte_offset_to_line("hello\nworld\n", 3), 0);
    }

    #[test]
    fn byte_offset_second_line() {
        assert_eq!(byte_offset_to_line("hello\nworld\n", 8), 1);
    }

    #[test]
    fn byte_offset_beyond_source() {
        // Should clamp to source length, not panic
        assert_eq!(byte_offset_to_line("hi\n", 999), 1);
    }

    // --- byte_offset_to_line_from_lines ---

    #[test]
    fn byte_offset_from_lines_basic() {
        let lines: Vec<&str> = "hello\nworld\nfoo".lines().collect();
        assert_eq!(byte_offset_to_line_from_lines(&lines, 0), 0);
        assert_eq!(byte_offset_to_line_from_lines(&lines, 6), 1);
        assert_eq!(byte_offset_to_line_from_lines(&lines, 12), 2);
    }
}

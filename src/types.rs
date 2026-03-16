/// Shared data structures for codemap.

#[derive(Debug, Default, Clone)]
pub struct FileAnalysis {
    pub imports: Vec<Import>,
    pub exports: Vec<Export>,
    pub reexports: Vec<ReExport>,
    pub symbols: Vec<SymbolInfo>,
}

#[derive(Debug, Clone)]
pub struct Import {
    pub source: String,
    pub name: String,
    pub kind: ImportKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportKind {
    Named,
    Default,
    Namespace,
}

#[derive(Debug, Clone)]
pub struct Export {
    pub name: String,
    pub kind: ExportKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportKind {
    Function,
    Variable,
    Class,
    Interface,
    TypeAlias,
    Enum,
    Default,
}

#[derive(Debug, Clone)]
pub struct ReExport {
    pub source: String,
    pub local: String,
    pub exported: String,
}

#[derive(Debug, Clone)]
pub struct SymbolInfo {
    pub name: String,
    pub is_exported: bool,
    pub reference_count: usize,
}

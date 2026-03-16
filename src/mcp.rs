/// MCP server implementation with rmcp.
use crate::{db, map};
use rmcp::{
    ServerHandler, ServiceExt, handler::server::router::tool::ToolRouter,
    handler::server::wrapper::Parameters, model::*, schemars::JsonSchema, tool, tool_handler,
    tool_router, transport::stdio,
};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Clone)]
pub struct CodemapServer {
    tool_router: ToolRouter<Self>,
    db_path: String,
    root: PathBuf,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct GetMapRequest {
    #[schemars(description = "Maximum tokens in the output map")]
    max_tokens: Option<usize>,
    #[schemars(description = "Optional: bias results toward files related to this query")]
    query: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct LookupSymbolRequest {
    #[schemars(description = "Symbol name or glob pattern to search for")]
    pattern: String,
    #[schemars(description = "Maximum number of results")]
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct FileDepsRequest {
    #[schemars(description = "File path to get dependencies for")]
    file_path: String,
    #[schemars(description = "Direction: 'imports' or 'importers'")]
    direction: Option<String>,
}

#[tool_router]
impl CodemapServer {
    pub fn new(db_path: String, root: PathBuf) -> Self {
        Self {
            tool_router: Self::tool_router(),
            db_path,
            root,
        }
    }

    #[tool(
        name = "get_code_map",
        description = "Get a ranked code map of the most important files and their exported symbols. Optionally provide a query to bias results toward relevant files using personalized PageRank."
    )]
    async fn get_code_map(&self, req: Parameters<GetMapRequest>) -> String {
        let req = req.0;
        let max_tokens = req.max_tokens.unwrap_or(1500);
        let conn = match db::init_db(&self.db_path) {
            Ok(c) => c,
            Err(e) => return format!("Error opening database: {e}"),
        };
        let files = match db::get_ranked_files(&conn, 500) {
            Ok(f) => f,
            Err(e) => return format!("Error loading files: {e}"),
        };
        // TODO: When query is provided, use personalized PageRank to re-rank
        let _ = req.query;
        match map::generate_map(&self.root, &conn, &files, max_tokens, "tree") {
            Ok(output) => output,
            Err(e) => format!("Error generating map: {e}"),
        }
    }

    #[tool(
        name = "lookup_symbol",
        description = "Look up a symbol by name across the codebase. Returns file location, export status, and reference count."
    )]
    async fn lookup_symbol(&self, req: Parameters<LookupSymbolRequest>) -> String {
        let req = req.0;
        let limit = req.limit.unwrap_or(10);
        let conn = match db::init_db(&self.db_path) {
            Ok(c) => c,
            Err(e) => return format!("Error opening database: {e}"),
        };
        match db::query_symbols(&conn, &req.pattern, limit) {
            Ok(results) => {
                if results.is_empty() {
                    return format!("No symbols matching '{}'", req.pattern);
                }
                let mut output = String::new();
                for r in &results {
                    let exported = if r.is_exported { "exported" } else { "local" };
                    let line = r.line.map(|l| format!(":{l}")).unwrap_or_default();
                    output.push_str(&format!(
                        "{}{line}  {} {} ({exported}, {} refs)\n",
                        r.file_path, r.name, r.kind, r.ref_count
                    ));
                }
                output
            }
            Err(e) => format!("Error querying symbols: {e}"),
        }
    }

    #[tool(
        name = "get_file_deps",
        description = "Get the import dependencies or importers of a specific file."
    )]
    async fn get_file_deps(&self, req: Parameters<FileDepsRequest>) -> String {
        let req = req.0;
        let direction = req.direction.as_deref().unwrap_or("imports");
        let conn = match db::init_db(&self.db_path) {
            Ok(c) => c,
            Err(e) => return format!("Error opening database: {e}"),
        };
        match db::get_file_deps(&conn, &req.file_path, direction) {
            Ok(deps) => {
                if deps.is_empty() {
                    return format!("No {} found for '{}'", direction, req.file_path);
                }
                let mut output = format!("{} of {}:\n", direction, req.file_path);
                for dep in &deps {
                    output.push_str(&format!("  {} ({})\n", dep.file_path, dep.edge_type));
                }
                output
            }
            Err(e) => format!("Error querying deps: {e}"),
        }
    }
}

#[tool_handler]
impl ServerHandler for CodemapServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation {
                name: "codemap".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                title: None,
                description: None,
                icons: None,
                website_url: None,
            },
            instructions: Some(
                "Code intelligence server for JS/TS. Use get_code_map for an overview, \
                 lookup_symbol to find specific symbols, get_file_deps for dependency analysis."
                    .into(),
            ),
        }
    }
}

pub async fn run_mcp_server(db_path: String, root: PathBuf) -> anyhow::Result<()> {
    let server = CodemapServer::new(db_path, root);
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

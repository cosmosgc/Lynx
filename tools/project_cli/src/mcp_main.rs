#![allow(dead_code)] // Shared CLI command modules bring along unused read-only and write commands.

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, ensure};
use clap::Parser;
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerConfig},
    schemars::JsonSchema,
    tool, tool_handler, tool_router,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

mod commands;

const MAX_OUTPUT_BYTES: usize = 256 * 1024;
const MAX_JSON_DEPTH: usize = 16;

#[derive(Parser)]
#[command(name = "lynx-mcp", about = "Read-only workspace MCP server")]
struct Args {
    /// Workspace root exposed to MCP tools. The server is disabled unless this
    /// binary is explicitly launched by an MCP client.
    #[arg(long, default_value = ".")]
    workspace: PathBuf,
}

#[derive(Debug, Clone)]
struct Workspace {
    root: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct PathInput {
    /// Workspace-relative path. Absolute paths and parent traversal are rejected.
    path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct DiffInput {
    /// Workspace-relative base manifest path.
    base: String,
    /// Workspace-relative candidate manifest path.
    next: String,
}

#[derive(Debug, Clone)]
struct McpServer {
    workspace: Workspace,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl McpServer {
    fn new(workspace: Workspace) -> Self {
        Self {
            workspace,
            tool_router: Self::tool_router(),
        }
    }

    /// Inspect the configured project and its checked-in content manifest.
    #[tool(
        name = "project.inspect",
        description = "Inspect the configured Lynx project and content manifest."
    )]
    async fn project_inspect(&self) -> String {
        self.invoke(|| {
            let validation = commands::project::validate_project(&ValidateArgs {
                project: self.workspace.root.clone(),
            })?;
            let manifest_path = self.workspace.root.join("content/manifest.json");
            let manifest = if manifest_path.exists() {
                let manifest_path = self.resolve("content/manifest.json")?;
                let bytes = std::fs::read(&manifest_path)?;
                let parsed: engine_content::ContentManifest = serde_json::from_slice(&bytes)?;
                json!({
                    "content_version": parsed.content_version,
                    "engine": parsed.engine,
                    "definitions": parsed.definitions.len(),
                    "locales": parsed.locales.len(),
                    "sha256": engine_content::sha256_hex(&bytes),
                })
            } else {
                Value::Null
            };
            Ok(json!({
                "ok": validation["ok"],
                "command": "project.inspect",
                "project": self.workspace.root,
                "validation": validation,
                "manifest": manifest,
            }))
        })
    }

    /// Validate definitions and references using the CLI's canonical validator.
    #[tool(
        name = "content.validate",
        description = "Validate content in the configured Lynx workspace."
    )]
    async fn content_validate(&self) -> String {
        self.invoke(|| {
            commands::project::validate_project(&ValidateArgs {
                project: self.workspace.root.clone(),
            })
        })
    }

    /// Compare two workspace manifests using the CLI's compatibility rules.
    #[tool(
        name = "content.diff",
        description = "Compare two content manifests inside the configured workspace."
    )]
    async fn content_diff(&self, input: Parameters<DiffInput>) -> String {
        self.invoke(|| {
            let base = self.resolve(&input.0.base)?;
            let next = self.resolve(&input.0.next)?;
            commands::content::content(&ContentArgs {
                command: ContentCommand::Diff(DiffArgs { base, next }),
            })
        })
    }

    /// Run project diagnostics without making backend calls or executing commands.
    #[tool(
        name = "doctor",
        description = "Validate the workspace project without invoking external services."
    )]
    async fn doctor(&self) -> String {
        self.invoke(|| {
            commands::doctor::doctor(&DoctorArgs {
                project: Some(self.workspace.root.clone()),
                backend: false,
            })
        })
    }

    /// Compute a SHA-256 digest for a workspace file.
    #[tool(
        name = "content.hash",
        description = "Hash a regular file confined to the configured workspace."
    )]
    async fn content_hash(&self, input: Parameters<PathInput>) -> String {
        self.invoke(|| {
            let path = self.resolve(&input.0.path)?;
            commands::content::content(&ContentArgs {
                command: ContentCommand::Hash(HashArgs { path }),
            })
        })
    }
}

#[tool_handler(router = self.tool_router)]
#[allow(
    clippy::unused_async_trait_impl,
    reason = "the MCP SDK macro implements its async handler contract"
)]
impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            rmcp::model::Implementation::new("lynx-readonly-workspace", env!("CARGO_PKG_VERSION")),
        )
    }
}

impl McpServer {
    fn invoke(&self, operation: impl FnOnce() -> Result<Value>) -> String {
        match self
            .guard_content_tree()
            .and_then(|()| operation())
            .and_then(bounded_result)
        {
            Ok(value) => serde_json::to_string(&value)
                .unwrap_or_else(|_| "{\"ok\":false,\"error\":\"serialization failed\"}".to_owned()),
            Err(error) => serde_json::to_string(
                &json!({"ok": false, "error": bounded_error(&error.to_string())}),
            )
            .unwrap_or_else(|_| "{\"ok\":false,\"error\":\"tool failed\"}".to_owned()),
        }
    }

    fn resolve(&self, relative: &str) -> Result<PathBuf> {
        ensure!(
            relative.len() <= 4096,
            "tool paths are limited to 4096 bytes"
        );
        let path = Path::new(relative);
        ensure!(!path.is_absolute(), "tool paths must be workspace-relative");
        ensure!(
            path.components().all(|part| !matches!(
                part,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )),
            "tool paths cannot traverse outside the workspace"
        );
        let candidate = self.workspace.root.join(path);
        let canonical = candidate
            .canonicalize()
            .with_context(|| format!("workspace path does not exist: {relative}"))?;
        ensure!(
            canonical.starts_with(&self.workspace.root),
            "tool path resolves outside the workspace"
        );
        ensure!(
            canonical.is_file(),
            "tool path must identify a regular file"
        );
        Ok(canonical)
    }

    fn guard_content_tree(&self) -> Result<()> {
        let content = self.workspace.root.join("content");
        if !content.exists() {
            return Ok(());
        }
        ensure!(
            !std::fs::symlink_metadata(&content)?
                .file_type()
                .is_symlink(),
            "content directory cannot be a symlink"
        );
        let mut pending = vec![content];
        let mut entries_seen = 0usize;
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(directory)? {
                entries_seen += 1;
                ensure!(
                    entries_seen <= 50_000,
                    "content tree exceeds the 50000-entry scan limit"
                );
                let entry = entry?;
                let kind = entry.file_type()?;
                ensure!(!kind.is_symlink(), "content trees cannot contain symlinks");
                if kind.is_dir() {
                    pending.push(entry.path());
                } else {
                    ensure!(kind.is_file(), "content trees cannot contain special files");
                }
            }
        }
        Ok(())
    }
}

fn bounded_result(value: Value) -> Result<Value> {
    ensure!(
        json_depth(&value) <= MAX_JSON_DEPTH,
        "tool result exceeds the maximum JSON depth"
    );
    let encoded = serde_json::to_vec(&value)?;
    ensure!(
        encoded.len() <= MAX_OUTPUT_BYTES,
        "tool result exceeds the 256 KiB output limit"
    );
    Ok(value)
}

fn json_depth(value: &Value) -> usize {
    match value {
        Value::Array(items) => 1 + items.iter().map(json_depth).max().unwrap_or(0),
        Value::Object(items) => 1 + items.values().map(json_depth).max().unwrap_or(0),
        _ => 0,
    }
}

fn bounded_error(message: &str) -> String {
    message.chars().take(1024).collect()
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let root = args
        .workspace
        .canonicalize()
        .context("workspace root must exist")?;
    ensure!(root.is_dir(), "workspace root must be a directory");
    McpServer::new(Workspace { root })
        .serve(rmcp::transport::stdio())
        .await?
        .waiting()
        .await?;
    Ok(())
}

// Reuse the project CLI command implementations without shelling out to Cargo
// or a second `lynx` process. Only read-only command shapes are present here.
#[allow(dead_code)]
struct DoctorArgs {
    project: Option<PathBuf>,
    backend: bool,
}

#[allow(dead_code)]
struct ContentArgs {
    command: ContentCommand,
}

enum ContentCommand {
    Build(ContentBuildArgs),
    Hash(HashArgs),
    Diff(DiffArgs),
    PublishCheck(PublishCheckArgs),
}

struct HashArgs {
    path: PathBuf,
}

struct DiffArgs {
    base: PathBuf,
    next: PathBuf,
}

// The shared content module needs these argument types even though this
// transport intentionally does not expose build or publishing operations.
#[allow(dead_code)]
struct ContentBuildArgs {
    project: PathBuf,
    output: PathBuf,
    content_version: String,
    engine: String,
}

#[allow(dead_code)]
struct PublishCheckArgs {
    manifest: PathBuf,
    base: PathBuf,
    skip_review: bool,
    allow_self_review: bool,
    license_optional: bool,
}

#[allow(dead_code)]
struct NewArgs {
    name: String,
    template: Template,
    output: PathBuf,
}

#[derive(Clone, Copy)]
enum Template {
    Minimal,
    Mmorpg,
}

#[allow(dead_code)]
struct ValidateArgs {
    project: PathBuf,
}

#[allow(dead_code)]
struct ProjectCommandArgs {
    project: PathBuf,
}

#[allow(dead_code)]
struct MigrateArgs {
    command: MigrateCommand,
}

#[allow(dead_code)]
enum MigrateCommand {
    Apply(MigrationApplyArgs),
    Rollback(MigrationRollbackArgs),
}

#[allow(dead_code)]
struct MigrationApplyArgs {
    project: PathBuf,
    plan: PathBuf,
    apply: bool,
}

#[allow(dead_code)]
struct MigrationRollbackArgs {
    project: PathBuf,
    id: String,
}

#[cfg_attr(coverage_nightly, coverage(off))]
#[cfg(test)]
mod tests {
    use super::*;

    fn temp_workspace() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "lynx-mcp-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("content.json"), "{}").unwrap();
        root
    }

    #[test]
    fn workspace_paths_are_relative_and_confined() {
        let root = temp_workspace().canonicalize().unwrap();
        let server = McpServer::new(Workspace { root: root.clone() });
        assert_eq!(
            server.resolve("content.json").unwrap(),
            root.join("content.json")
        );
        assert!(server.resolve("../outside.json").is_err());
        assert!(
            server
                .resolve(root.join("content.json").to_str().unwrap())
                .is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tool_results_enforce_depth_and_byte_limits() {
        assert!(bounded_result(json!({"ok": true})).is_ok());
        assert!(bounded_result(json!("x".repeat(MAX_OUTPUT_BYTES + 1))).is_err());
        let nested = (0..MAX_JSON_DEPTH + 1).fold(json!(null), |value, _| json!([value]));
        assert!(bounded_result(nested).is_err());
    }

    #[tokio::test]
    async fn validate_tool_returns_the_cli_report() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .canonicalize()
            .unwrap();
        let server = McpServer::new(Workspace { root: root.clone() });
        let tool_report: Value = serde_json::from_str(&server.content_validate().await).unwrap();
        let cli_report =
            commands::project::validate_project(&ValidateArgs { project: root }).unwrap();
        assert_eq!(tool_report, cli_report);
    }
}

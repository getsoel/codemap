/// Workspace isolation: create temp directories for each eval session.
///
/// Each session runs in a fresh copy of the fixture repo to prevent
/// cross-contamination between control and treatment runs.
use anyhow::{Context, Result, ensure};
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

const CODEMAP_INSTRUCTIONS: &str = "\
## codemap — codebase intelligence
Use these commands in Bash for structural codebase queries:
- `codemap context \"<task>\"` — find the most relevant files for a task (start here)
- `codemap symbol <name>` — find where a symbol is defined and who uses it
- `codemap deps <file>` — imports and importers of a file
- `codemap map` — ranked overview of top files with signatures";

pub struct TempWorkspace {
    dir: TempDir,
    pub system_prompt: Option<String>,
}

impl TempWorkspace {
    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

/// Create an isolated workspace for a single eval session.
///
/// Copies the repo into a temp directory. When `is_treatment` is true, also sets
/// up `.codemap/index.db` and generates the system prompt (map + instructions).
pub fn create_workspace(
    repo_dir: &Path,
    is_treatment: bool,
    index_db: Option<&Path>,
    codemap_bin: &Path,
) -> Result<TempWorkspace> {
    let tmp = TempDir::new().context("failed to create temp directory")?;

    copy_repo(repo_dir, tmp.path())?;

    let mut system_prompt = None;

    if is_treatment {
        if let Some(db_path) = index_db {
            let codemap_dir = tmp.path().join(".codemap");
            std::fs::create_dir_all(&codemap_dir)?;
            std::fs::copy(db_path, codemap_dir.join("index.db"))
                .context("failed to copy index.db into workspace")?;
        }

        let map_output = run_codemap_map(codemap_bin, tmp.path())?;
        system_prompt = Some(format!("{map_output}\n\n{CODEMAP_INSTRUCTIONS}"));
    }

    Ok(TempWorkspace {
        dir: tmp,
        system_prompt,
    })
}

/// Copy repo contents using rsync, excluding .git, .codemap, and node_modules.
fn copy_repo(src: &Path, dst: &Path) -> Result<()> {
    let status = Command::new("rsync")
        .args([
            "-a",
            "--exclude",
            ".git",
            "--exclude",
            ".codemap",
            "--exclude",
            "node_modules",
        ])
        .arg(format!("{}/", src.display()))
        .arg(format!("{}/", dst.display()))
        .status()
        .context("failed to run rsync — is it installed?")?;
    ensure!(status.success(), "rsync failed to copy repo");
    Ok(())
}

/// Run `codemap map --tokens 1500 --no-instructions` and capture output.
fn run_codemap_map(codemap_bin: &Path, working_dir: &Path) -> Result<String> {
    let output = Command::new(codemap_bin)
        .current_dir(working_dir)
        .args(["map", "--tokens", "1500", "--no-instructions"])
        .output()
        .context("failed to run codemap map")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("codemap map failed: {stderr}");
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Ensure a repo checkout exists in `eval/repos/<name>`, cloning if necessary.
///
/// Returns the path to the checkout directory.
pub fn ensure_repo(eval_dir: &Path, name: &str, repo_url: &str) -> Result<PathBuf> {
    let repos_dir = eval_dir.join("repos");
    std::fs::create_dir_all(&repos_dir)?;
    let repo_dir = repos_dir.join(name);

    if repo_dir.exists() {
        eprintln!("Using cached repo: {}", repo_dir.display());
    } else {
        ensure!(
            !repo_url.is_empty(),
            "No repo_url in dataset and no cached checkout at {}.\n\
             Add \"repo_url\": \"https://github.com/...\" to the dataset JSON.",
            repo_dir.display()
        );
        eprintln!("Cloning {repo_url} into {}...", repo_dir.display());
        let status = Command::new("git")
            .args(["clone", "--depth", "1"])
            .arg(repo_url)
            .arg(&repo_dir)
            .status()
            .context("failed to run git clone")?;
        ensure!(status.success(), "git clone failed for {repo_url}");
    }

    Ok(repo_dir)
}

/// Find the codemap binary next to the current executable, or in PATH.
pub fn find_codemap_bin() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("cannot determine executable directory"))?;
    let codemap = dir.join("codemap");
    if codemap.exists() {
        return Ok(codemap);
    }
    // Fallback: check PATH
    if let Ok(output) = Command::new("which").arg("codemap").output()
        && output.status.success()
    {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !path.is_empty() {
            return Ok(PathBuf::from(path));
        }
    }
    anyhow::bail!(
        "codemap binary not found. Build it first: cargo build\n\
         Expected at: {}",
        codemap.display()
    )
}

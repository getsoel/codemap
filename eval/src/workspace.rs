/// Workspace isolation: create temp directories for each eval session.
///
/// Each session runs in a fresh copy of the fixture repo to prevent
/// cross-contamination between control and treatment runs.
use anyhow::{Context, Result, ensure};
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

pub struct TempWorkspace {
    dir: TempDir,
}

impl TempWorkspace {
    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

/// Create an isolated workspace for a single eval session.
///
/// Copies the repo into a temp directory. When `is_treatment` is true, runs
/// `codemap setup --no-post-hook` which indexes the codebase and writes the
/// SessionStart hook to `.claude/settings.local.json`.
/// Claude Code then picks up the hook naturally at session start.
pub fn create_workspace(
    repo_dir: &Path,
    is_treatment: bool,
    codemap_bin: &Path,
) -> Result<TempWorkspace> {
    let tmp = TempDir::new().context("failed to create temp directory")?;

    copy_repo(repo_dir, tmp.path())?;

    if is_treatment {
        // Run `codemap setup --no-post-hook` — indexes the repo and writes the
        // SessionStart hook config. This is the same path real users take.
        run_codemap_setup(codemap_bin, tmp.path())?;
    }

    Ok(TempWorkspace { dir: tmp })
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

/// Run `codemap setup --no-post-hook` to install the SessionStart hook.
///
/// This writes `.claude/settings.local.json` with a hook that runs
/// `codemap instructions` at session start — the same config real users get.
fn run_codemap_setup(codemap_bin: &Path, working_dir: &Path) -> Result<()> {
    let output = Command::new(codemap_bin)
        .current_dir(working_dir)
        .args(["setup", "--no-post-hook"])
        .output()
        .context("failed to run codemap setup")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("codemap setup failed: {stderr}");
    }

    // Verify the hook config was written and contains a SessionStart hook
    let settings = working_dir.join(".claude/settings.local.json");
    let content = std::fs::read_to_string(&settings)
        .with_context(|| format!("codemap setup did not write {}", settings.display()))?;
    ensure!(
        content.contains("SessionStart"),
        "codemap setup did not configure a SessionStart hook"
    );

    Ok(())
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

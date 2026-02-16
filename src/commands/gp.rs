use std::ffi::OsStr;
use std::io::{IsTerminal as _, stderr};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context as _, Result, anyhow};
use clap::Args;
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use rayon::prelude::*;
use walkdir::{DirEntry, WalkDir};

use crate::context::Context;

#[derive(Args, Debug, Clone)]
pub struct GpArgs {
    #[arg(default_value = ".", help = "Root path to search for git repositories")]
    pub path: PathBuf,

    #[arg(long, help = "Disable progress bar")]
    pub no_progress: bool,

    #[arg(long, help = "Also descend into repositories to find nested repos")]
    pub include_nested: bool,

    #[arg(long, help = "Allow pulling repositories with uncommitted changes")]
    pub allow_dirty: bool,

    #[arg(long, conflicts_with = "rebase", help = "Use `git pull --ff-only`")]
    pub ff_only: bool,

    #[arg(long, conflicts_with = "ff_only", help = "Use `git pull --rebase`")]
    pub rebase: bool,

    #[arg(
        long,
        short = 'j',
        help = "Number of concurrent pulls (default: CPU cores)"
    )]
    pub jobs: Option<usize>,
}

#[derive(Debug)]
struct RepoResult {
    repo_root: PathBuf,
    outcome: Outcome,
}

#[derive(Debug)]
enum Outcome {
    Skipped(String),
    Ok {
        stdout: String,
        stderr: String,
    },
    Err {
        message: String,
        stdout: String,
        stderr: String,
    },
}

pub fn run(args: &GpArgs, ctx: &Context) -> Result<()> {
    let root = canonicalize_soft(&args.path)?;
    if !root.is_dir() {
        return Err(anyhow!("path is not a directory: {}", root.display()));
    }

    let repos = discover_git_repos(&root, args.include_nested)?;
    if repos.is_empty() {
        println!("未发现 Git 仓库：{}", root.display());
        return Ok(());
    }

    let jobs = args.jobs.unwrap_or_else(default_jobs);
    if ctx.verbose > 0 {
        println!(
            "发现 {} 个仓库，jobs={}，root={}",
            repos.len(),
            jobs,
            root.display()
        );
    }

    let progress = build_progress_bar(repos.len(), args, ctx);
    let results = if jobs <= 1 {
        let mut results = Vec::with_capacity(repos.len());
        for repo_root in &repos {
            let r = pull_one(repo_root, args, ctx);
            if let Some(pb) = &progress {
                pb.inc(1);
            }
            results.push(r);
        }
        results
    } else {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(jobs)
            .build()
            .context("failed to create thread pool")?;

        let progress = progress.clone();
        pool.install(|| {
            repos
                .par_iter()
                .map(|repo_root| {
                    let r = pull_one(repo_root, args, ctx);
                    if let Some(pb) = &progress {
                        pb.inc(1);
                    }
                    r
                })
                .collect::<Vec<_>>()
        })
    };

    if let Some(pb) = &progress {
        pb.finish_and_clear();
    }

    let mut ok = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;

    let mut results = results;
    results.sort_by(|a, b| a.repo_root.cmp(&b.repo_root));

    for r in results {
        match r.outcome {
            Outcome::Skipped(reason) => {
                skipped += 1;
                println!("[SKIP] {} ({})", r.repo_root.display(), reason);
            }
            Outcome::Ok { stdout, stderr } => {
                ok += 1;
                println!("[OK]   {}", r.repo_root.display());
                if ctx.dry_run {
                    if !stdout.trim().is_empty() {
                        println!("{}", stdout.trim_end());
                    }
                    if !stderr.trim().is_empty() {
                        eprintln!("{}", stderr.trim_end());
                    }
                } else {
                    print_output_if_verbose(ctx, &stdout, &stderr);
                }
            }
            Outcome::Err {
                message,
                stdout,
                stderr,
            } => {
                failed += 1;
                println!("[FAIL] {} ({})", r.repo_root.display(), message);
                print_output_always(&stdout, &stderr);
            }
        }
    }

    println!("汇总：ok={} skip={} fail={}", ok, skipped, failed);
    if failed > 0 {
        return Err(anyhow!("some repositories failed to pull"));
    }
    Ok(())
}

fn pull_one(repo_root: &Path, args: &GpArgs, ctx: &Context) -> RepoResult {
    if !args.allow_dirty {
        match is_dirty(repo_root) {
            Ok(true) => {
                return RepoResult {
                    repo_root: repo_root.to_path_buf(),
                    outcome: Outcome::Skipped("dirty".to_string()),
                };
            }
            Ok(false) => {}
            Err(e) => {
                return RepoResult {
                    repo_root: repo_root.to_path_buf(),
                    outcome: Outcome::Err {
                        message: "git status failed".to_string(),
                        stdout: String::new(),
                        stderr: e.to_string(),
                    },
                };
            }
        }
    }

    if ctx.dry_run {
        return RepoResult {
            repo_root: repo_root.to_path_buf(),
            outcome: Outcome::Ok {
                stdout: format!(
                    "dry-run: git -C {} pull{}{}",
                    repo_root.display(),
                    if args.ff_only { " --ff-only" } else { "" },
                    if args.rebase { " --rebase" } else { "" }
                ),
                stderr: String::new(),
            },
        };
    }

    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo_root).arg("pull");
    if args.ff_only {
        cmd.arg("--ff-only");
    }
    if args.rebase {
        cmd.arg("--rebase");
    }

    let output = match cmd.output() {
        Ok(o) => o,
        Err(e) => {
            return RepoResult {
                repo_root: repo_root.to_path_buf(),
                outcome: Outcome::Err {
                    message: "spawn git failed".to_string(),
                    stdout: String::new(),
                    stderr: e.to_string(),
                },
            };
        }
    };

    RepoResult {
        repo_root: repo_root.to_path_buf(),
        outcome: classify_output(output),
    }
}

fn classify_output(output: Output) -> Outcome {
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if output.status.success() {
        Outcome::Ok { stdout, stderr }
    } else {
        Outcome::Err {
            message: match output.status.code() {
                Some(code) => format!("exit={code}"),
                None => "terminated".to_string(),
            },
            stdout,
            stderr,
        }
    }
}

fn print_output_if_verbose(ctx: &Context, stdout: &str, stderr: &str) {
    if ctx.verbose == 0 {
        return;
    }
    print_output_always(stdout, stderr);
}

fn print_output_always(stdout: &str, stderr: &str) {
    if !stdout.trim().is_empty() {
        println!("{}", stdout.trim_end());
    }
    if !stderr.trim().is_empty() {
        eprintln!("{}", stderr.trim_end());
    }
}

fn is_dirty(repo_root: &Path) -> Result<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("status")
        .arg("--porcelain")
        .output()
        .with_context(|| format!("failed to execute git status in {}", repo_root.display()))?;

    if !output.status.success() {
        return Ok(true);
    }
    Ok(!output.stdout.is_empty())
}

pub fn discover_git_repos(root: &Path, include_nested: bool) -> Result<Vec<PathBuf>> {
    let mut repos = Vec::<PathBuf>::new();
    let mut it = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| should_descend(e));

    while let Some(entry) = it.next() {
        let entry = entry?;
        if !entry.file_type().is_dir() {
            continue;
        }

        let path = entry.path();
        if is_git_root(path) {
            repos.push(path.to_path_buf());
            if !include_nested {
                it.skip_current_dir();
            }
        }
    }

    repos.sort();
    repos.dedup();
    Ok(repos)
}

fn should_descend(entry: &DirEntry) -> bool {
    if !entry.file_type().is_dir() {
        return true;
    }
    let name = entry.file_name();
    if name == OsStr::new(".git") {
        return false;
    }
    if name == OsStr::new("target") {
        return false;
    }
    if name == OsStr::new("node_modules") {
        return false;
    }
    true
}

fn is_git_root(dir: &Path) -> bool {
    let dot_git = dir.join(".git");
    dot_git.is_dir() || dot_git.is_file()
}

fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

fn canonicalize_soft(path: &Path) -> Result<PathBuf> {
    match path.canonicalize() {
        Ok(p) => Ok(p),
        Err(_) => Ok(path.to_path_buf()),
    }
}

fn build_progress_bar(repo_count: usize, args: &GpArgs, ctx: &Context) -> Option<ProgressBar> {
    if args.no_progress {
        return None;
    }
    if ctx.verbose > 0 {
        return None;
    }
    if repo_count <= 1 {
        return None;
    }
    if !stderr().is_terminal() {
        return None;
    }

    let pb = ProgressBar::with_draw_target(
        Some(repo_count as u64),
        ProgressDrawTarget::stderr_with_hz(10),
    );

    let style =
        ProgressStyle::with_template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} {msg}")
            .ok()?
            .progress_chars("=> ");
    pb.set_style(style);
    pb.set_message("git pull");
    pb.enable_steady_tick(std::time::Duration::from_millis(120));
    Some(pb)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn discovers_git_repos_and_prunes_when_not_nested() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();

        let a = root.join("a");
        fs::create_dir_all(a.join(".git")).unwrap();
        fs::create_dir_all(a.join("nested").join(".git")).unwrap();

        let b = root.join("b");
        fs::create_dir_all(&b).unwrap();
        fs::write(b.join(".git"), "gitdir: ../.git/worktrees/b").unwrap();

        let repos = discover_git_repos(root, false).unwrap();
        assert!(repos.contains(&a));
        assert!(repos.contains(&b));
        assert!(!repos.contains(&a.join("nested")));
    }

    #[test]
    fn discovers_nested_when_enabled() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();

        let a = root.join("a");
        fs::create_dir_all(a.join(".git")).unwrap();
        fs::create_dir_all(a.join("nested").join(".git")).unwrap();

        let repos = discover_git_repos(root, true).unwrap();
        assert!(repos.contains(&a));
        assert!(repos.contains(&a.join("nested")));
    }
}

use std::path::{Path, PathBuf};

pub(crate) fn has_any_file(root: &Path, candidates: &[&str]) -> bool {
    candidates.iter().any(|name| root.join(name).exists())
}

/// Walk up from `cwd` to find the project root.
/// First tries an unbounded walk to find a `.git` directory.
/// If none found, walks up `max_depth` levels looking for project files.
pub fn find_project_root(cwd: &Path, max_depth: usize) -> Option<PathBuf> {
    let mut current = cwd.to_path_buf();
    loop {
        if current.join(".git").exists() {
            return Some(current);
        }
        if !current.pop() {
            break;
        }
    }

    let mut current = cwd.to_path_buf();
    const PROJECT_MARKERS: &[&str] = &[
        "Makefile",
        "package.json",
        "Cargo.toml",
        "pyproject.toml",
        "docker-compose.yml",
    ];
    for _ in 0..max_depth {
        if has_any_file(&current, PROJECT_MARKERS) {
            return Some(current);
        }
        if !current.pop() {
            break;
        }
    }

    None
}

pub fn detect_project_type(root: &Path) -> Option<String> {
    for (kind, markers) in [
        ("rust", &["Cargo.toml"][..]),
        ("node", &["package.json"][..]),
        ("python", &["pyproject.toml", "setup.py"][..]),
        ("go", &["go.mod"][..]),
        ("make", &["Makefile"][..]),
    ] {
        if has_any_file(root, markers) {
            return Some(kind.to_string());
        }
    }
    None
}

pub fn read_git_branch_for_path(path: &Path) -> Option<String> {
    let mut current = path.to_path_buf();
    loop {
        let git_path = current.join(".git");
        if git_path.exists() {
            // Handle .git as file (worktrees) or directory
            let git_dir = if git_path.is_file() {
                let content = std::fs::read_to_string(&git_path).ok()?;
                let target = content.trim().strip_prefix("gitdir: ")?;
                PathBuf::from(target)
            } else {
                git_path
            };
            return read_git_branch(&git_dir);
        }
        if !current.pop() {
            break;
        }
    }
    None
}

fn read_git_branch(git_dir: &Path) -> Option<String> {
    let head_path = git_dir.join("HEAD");
    let content = std::fs::read_to_string(head_path).ok()?;
    let trimmed = content.trim();
    if let Some(branch) = trimmed.strip_prefix("ref: refs/heads/") {
        Some(branch.to_string())
    } else {
        Some(trimmed[..8.min(trimmed.len())].to_string())
    }
}

pub fn detect_package_manager(root: &Path) -> &'static str {
    if root.join("pnpm-lock.yaml").exists() {
        "pnpm"
    } else if root.join("yarn.lock").exists() {
        "yarn"
    } else if root.join("bun.lockb").exists() || root.join("bun.lock").exists() {
        "bun"
    } else {
        "npm"
    }
}

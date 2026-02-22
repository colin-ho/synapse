use std::path::{Path, PathBuf};

/// Information about a CLI tool built by the current project.
#[derive(Debug, Clone)]
pub struct ProjectCliTool {
    pub name: String,
    pub binary_path: Option<PathBuf>,
}

pub(crate) fn has_any_file(root: &Path, candidates: &[&str]) -> bool {
    candidates.iter().any(|name| root.join(name).exists())
}

/// Walk up from `cwd` to find the project root.
/// First tries an unbounded walk to find a `.git` directory.
/// If none found, walks up `max_depth` levels looking for project files.
pub fn find_project_root(cwd: &Path, max_depth: usize) -> Option<PathBuf> {
    if let Some((_, root)) = discover_git_repository(cwd) {
        return Some(root);
    }

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
    if let Some((git_dir, _)) = discover_git_repository(path) {
        return read_git_branch(&git_dir);
    }

    let mut current = path.to_path_buf();
    loop {
        let git_dir = current.join(".git");
        if git_dir.exists() {
            return read_git_branch(&git_dir);
        }
        if !current.pop() {
            break;
        }
    }
    None
}

fn discover_git_repository(path: &Path) -> Option<(PathBuf, PathBuf)> {
    let (repo_path, _) = gix_discover::upwards(path).ok()?;
    let (git_dir, work_tree) = repo_path.into_repository_and_work_tree_directories();
    let root = work_tree.unwrap_or_else(|| git_dir.clone());
    Some((git_dir, root))
}

fn read_git_branch_with_gix(git_dir: &Path) -> Option<String> {
    let store = gix_ref::file::Store::at(
        git_dir.to_path_buf(),
        gix_ref::store::init::Options::default(),
    );
    let head = store.try_find_loose("HEAD").ok().flatten()?;
    match head.target {
        gix_ref::Target::Symbolic(name) => {
            let full = name.to_string();
            Some(
                full.strip_prefix("refs/heads/")
                    .unwrap_or(full.as_str())
                    .to_string(),
            )
        }
        gix_ref::Target::Object(id) => {
            let hex = id.to_string();
            Some(hex[..8.min(hex.len())].to_string())
        }
    }
}

fn read_git_branch(git_dir: &Path) -> Option<String> {
    if let Some(branch) = read_git_branch_with_gix(git_dir) {
        return Some(branch);
    }

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

fn parse_cargo_manifest(path: &Path) -> Option<cargo_toml::Manifest> {
    let content = std::fs::read_to_string(path).ok()?;
    cargo_toml::Manifest::from_slice(content.as_bytes()).ok()
}

/// Detect CLI tools built by the current project.
/// Returns a list of binary names and their paths if the binaries have been built.
pub fn detect_project_cli_tools(root: &Path) -> Vec<ProjectCliTool> {
    let mut tools = Vec::new();

    if let Some(manifest) = parse_cargo_manifest(&root.join("Cargo.toml")) {
        let has_clap = manifest
            .dependencies
            .keys()
            .any(|k| k == "clap" || k == "structopt");
        if has_clap {
            let bins: Vec<String> = manifest.bin.iter().filter_map(|b| b.name.clone()).collect();

            if bins.is_empty() {
                if let Some(ref pkg) = manifest.package {
                    if root.join("src").join("main.rs").exists() {
                        let name = pkg.name.clone();
                        let binary_path = find_rust_binary(root, &name);
                        tools.push(ProjectCliTool { name, binary_path });
                    }
                }
            } else {
                for name in bins {
                    let binary_path = find_rust_binary(root, &name);
                    tools.push(ProjectCliTool { name, binary_path });
                }
            }
        }
    }

    if root.join("go.mod").exists() {
        if let Ok(content) = std::fs::read_to_string(root.join("go.mod")) {
            if content.contains("github.com/spf13/cobra")
                || content.contains("github.com/urfave/cli")
            {
                if let Some(name) = root.file_name().and_then(|n| n.to_str()) {
                    let binary_path = root.join(name);
                    let path = if binary_path.exists() {
                        Some(binary_path)
                    } else {
                        None
                    };
                    tools.push(ProjectCliTool {
                        name: name.to_string(),
                        binary_path: path,
                    });
                }
            }
        }
    }

    if root.join("pyproject.toml").exists() {
        if let Ok(content) = std::fs::read_to_string(root.join("pyproject.toml")) {
            if content.contains("click") || content.contains("typer") {
                for line in content.lines() {
                    let trimmed = line.trim();
                    if trimmed.contains('=') && trimmed.contains(':') {
                        if let Some(name) = trimmed.split('=').next() {
                            let name = name.trim().trim_matches('"');
                            if !name.is_empty() && !name.starts_with('[') && !name.contains('.') {
                                let venv_bin = root.join(".venv").join("bin").join(name);
                                let path = if venv_bin.exists() {
                                    Some(venv_bin)
                                } else {
                                    None
                                };
                                tools.push(ProjectCliTool {
                                    name: name.to_string(),
                                    binary_path: path,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    tools
}

fn find_rust_binary(root: &Path, name: &str) -> Option<PathBuf> {
    let debug = root.join("target").join("debug").join(name);
    if debug.exists() {
        return Some(debug);
    }
    let release = root.join("target").join("release").join(name);
    if release.exists() {
        return Some(release);
    }
    None
}

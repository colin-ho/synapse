use std::path::{Path, PathBuf};

/// Information about a CLI tool built by the current project.
#[derive(Debug, Clone)]
pub struct ProjectCliTool {
    pub name: String,
    pub binary_path: Option<PathBuf>,
}

// --- Project root discovery ---

/// Walk up from `cwd` to find the project root.
/// First tries an unbounded walk to find a `.git` directory.
/// If none found, walks up `max_depth` levels looking for project files.
pub fn find_project_root(cwd: &Path, max_depth: usize) -> Option<PathBuf> {
    if let Some((_, root)) = discover_git_repository(cwd) {
        return Some(root);
    }

    // Unbounded walk to find git root
    let mut current = cwd.to_path_buf();
    loop {
        if current.join(".git").exists() {
            return Some(current);
        }
        if !current.pop() {
            break;
        }
    }

    // No git root found — walk up max_depth levels looking for project files
    let mut current = cwd.to_path_buf();
    for _ in 0..max_depth {
        let has_project_file = current.join("Makefile").exists()
            || current.join("package.json").exists()
            || current.join("Cargo.toml").exists()
            || current.join("pyproject.toml").exists()
            || current.join("docker-compose.yml").exists();
        if has_project_file {
            return Some(current);
        }
        if !current.pop() {
            break;
        }
    }

    None
}

// --- Project metadata ---

pub fn detect_project_type(root: &Path) -> Option<String> {
    if root.join("Cargo.toml").exists() {
        Some("rust".into())
    } else if root.join("package.json").exists() {
        Some("node".into())
    } else if root.join("pyproject.toml").exists() || root.join("setup.py").exists() {
        Some("python".into())
    } else if root.join("go.mod").exists() {
        Some("go".into())
    } else if root.join("Makefile").exists() {
        Some("make".into())
    } else {
        None
    }
}

pub fn read_git_branch_for_path(path: &Path) -> Option<String> {
    if let Some((git_dir, _)) = discover_git_repository(path) {
        return read_git_branch(&git_dir);
    }

    // Fallback for minimal synthetic repositories used in tests.
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
        // Detached HEAD — return short hash
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

// --- Parsers ---

/// Parse Makefile targets. Returns empty vec if no Makefile or no valid targets.
pub fn parse_makefile_targets(root: &Path) -> Vec<String> {
    let content = match std::fs::read_to_string(root.join("Makefile")) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    content
        .lines()
        .filter_map(|line| {
            let target = line.split(':').next()?.trim();
            if !target.is_empty()
                && !target.starts_with('#')
                && !target.starts_with('.')
                && !target.starts_with('\t')
                && !target.contains(' ')
                && !target.contains('$')
                && !target.contains('=')
            {
                Some(target.to_string())
            } else {
                None
            }
        })
        .collect()
}

/// Parse package.json scripts. Returns script names with their command descriptions.
pub fn parse_npm_scripts(root: &Path) -> Option<Vec<(String, Option<String>)>> {
    let content = std::fs::read_to_string(root.join("package.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    let scripts = json.get("scripts")?.as_object()?;

    if scripts.is_empty() {
        return None;
    }

    Some(
        scripts
            .iter()
            .map(|(name, value)| (name.clone(), value.as_str().map(String::from)))
            .collect(),
    )
}

/// Parse Cargo.toml for workspace and binary target info.
/// Returns (is_workspace, has_bin_targets).
pub fn parse_cargo_info(root: &Path) -> Option<(bool, bool)> {
    let manifest = parse_cargo_manifest(&root.join("Cargo.toml"))?;
    Some((manifest.workspace.is_some(), !manifest.bin.is_empty()))
}

/// Parse docker-compose services. Returns None if no compose file exists.
pub fn parse_docker_services(root: &Path) -> Option<Vec<String>> {
    let compose_path = [
        "docker-compose.yml",
        "docker-compose.yaml",
        "compose.yml",
        "compose.yaml",
    ]
    .iter()
    .map(|f| root.join(f))
    .find(|p| p.exists())?;

    let content = std::fs::read_to_string(compose_path).ok()?;
    let yaml: serde_yml::Value = serde_yml::from_str(&content).ok()?;
    let service_map = yaml.get("services")?.as_mapping()?;
    Some(
        service_map
            .keys()
            .filter_map(|key| key.as_str().map(ToString::to_string))
            .collect(),
    )
}

/// Parse justfile recipes. Returns empty vec if no justfile or no valid recipes.
pub fn parse_justfile_recipes(root: &Path) -> Vec<String> {
    let path = if root.join("Justfile").exists() {
        root.join("Justfile")
    } else if root.join("justfile").exists() {
        root.join("justfile")
    } else {
        return Vec::new();
    };

    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    content
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty()
                || trimmed.starts_with('#')
                || trimmed.starts_with(' ')
                || trimmed.starts_with('\t')
                || trimmed.starts_with("set ")
                || trimmed.starts_with("export ")
                || trimmed.starts_with("alias ")
            {
                return None;
            }
            let name = trimmed.split(':').next()?;
            let name = name.split_whitespace().next().unwrap_or(name).trim();
            if !name.is_empty() && !name.contains('=') {
                Some(name.to_string())
            } else {
                None
            }
        })
        .collect()
}

fn parse_cargo_manifest(path: &Path) -> Option<cargo_toml::Manifest> {
    let content = std::fs::read_to_string(path).ok()?;
    cargo_toml::Manifest::from_slice(content.as_bytes()).ok()
}

#[derive(Debug, Clone)]
pub struct PythonInfo {
    pub has_venv: bool,
    pub has_poetry: bool,
    pub has_ruff: bool,
}

/// Detect Python project info. Returns None if no pyproject.toml or setup.py.
pub fn parse_python_info(root: &Path) -> Option<PythonInfo> {
    if !root.join("pyproject.toml").exists() && !root.join("setup.py").exists() {
        return None;
    }

    let has_venv = root.join(".venv").exists() || root.join("venv").exists();
    let (has_poetry, has_ruff) = root
        .join("pyproject.toml")
        .exists()
        .then(|| std::fs::read_to_string(root.join("pyproject.toml")).ok())
        .flatten()
        .map(|content| {
            (
                content.contains("[tool.poetry]"),
                content.contains("[tool.ruff]"),
            )
        })
        .unwrap_or((false, false));

    Some(PythonInfo {
        has_venv,
        has_poetry,
        has_ruff,
    })
}

// --- CLI tool detection ---

/// Detect CLI tools built by the current project.
/// Returns a list of binary names and their paths if the binaries have been built.
pub fn detect_project_cli_tools(root: &Path) -> Vec<ProjectCliTool> {
    let mut tools = Vec::new();

    // Rust/clap detection
    if let Some(manifest) = parse_cargo_manifest(&root.join("Cargo.toml")) {
        let has_clap = manifest
            .dependencies
            .keys()
            .any(|k| k == "clap" || k == "structopt");
        if has_clap {
            // Check [[bin]] targets
            let bins: Vec<String> = manifest.bin.iter().filter_map(|b| b.name.clone()).collect();

            if bins.is_empty() {
                // Fall back to package name with src/main.rs
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

    // Go/cobra detection
    if root.join("go.mod").exists() {
        if let Ok(content) = std::fs::read_to_string(root.join("go.mod")) {
            if content.contains("github.com/spf13/cobra")
                || content.contains("github.com/urfave/cli")
            {
                // Go binary name is typically the directory name
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

    // Python/click detection
    if root.join("pyproject.toml").exists() {
        if let Ok(content) = std::fs::read_to_string(root.join("pyproject.toml")) {
            if content.contains("click") || content.contains("typer") {
                // Look for [project.scripts] entries
                for line in content.lines() {
                    let trimmed = line.trim();
                    // Pattern: `name = "module:function"`
                    if trimmed.contains('=') && trimmed.contains(':') {
                        if let Some(name) = trimmed.split('=').next() {
                            let name = name.trim().trim_matches('"');
                            if !name.is_empty() && !name.starts_with('[') && !name.contains('.') {
                                // Check if it's in a venv
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

#[cfg(test)]
mod tests {
    use super::*;

    // --- Git branch tests ---

    #[test]
    fn test_read_git_branch_from_ref() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/feature/auth\n").unwrap();

        let branch = read_git_branch_for_path(dir.path());
        assert_eq!(branch.as_deref(), Some("feature/auth"));
    }

    #[test]
    fn test_read_git_branch_detached_head() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(
            git_dir.join("HEAD"),
            "d34db33fd34db33fd34db33fd34db33fd34db33f\n",
        )
        .unwrap();

        let branch = read_git_branch_for_path(dir.path());
        assert_eq!(branch.as_deref(), Some("d34db33f"));
    }

    #[test]
    fn test_read_git_branch_walks_up_to_repo_root() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("src").join("providers");
        std::fs::create_dir_all(&nested).unwrap();

        let git_dir = dir.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();

        let branch = read_git_branch_for_path(&nested);
        assert_eq!(branch.as_deref(), Some("main"));
    }

    // --- find_project_root tests ---

    #[test]
    fn test_find_project_root_via_git() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();

        let root = find_project_root(&nested, 3);
        assert_eq!(root.as_deref(), Some(dir.path()));
    }

    #[test]
    fn test_find_project_root_via_project_files() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("sub");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();

        let root = find_project_root(&nested, 3);
        assert_eq!(root.as_deref(), Some(dir.path()));
    }

    #[test]
    fn test_find_project_root_respects_max_depth() {
        let dir = tempfile::tempdir().unwrap();
        let deep = dir.path().join("a").join("b").join("c").join("d");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();

        assert!(find_project_root(&deep, 2).is_none());
        assert_eq!(find_project_root(&deep, 5).as_deref(), Some(dir.path()));
    }

    // --- detect_project_type tests ---

    #[test]
    fn test_detect_project_type() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(detect_project_type(dir.path()), None);

        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        assert_eq!(detect_project_type(dir.path()).as_deref(), Some("rust"));
    }

    // --- detect_package_manager tests ---

    #[test]
    fn test_detect_package_manager() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(detect_package_manager(dir.path()), "npm");

        std::fs::write(dir.path().join("yarn.lock"), "").unwrap();
        assert_eq!(detect_package_manager(dir.path()), "yarn");
    }

    #[test]
    fn test_detect_package_manager_pnpm() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pnpm-lock.yaml"), "").unwrap();
        assert_eq!(detect_package_manager(dir.path()), "pnpm");
    }

    #[test]
    fn test_detect_package_manager_bun() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("bun.lock"), "").unwrap();
        assert_eq!(detect_package_manager(dir.path()), "bun");
    }

    // --- Parser tests ---

    #[test]
    fn test_parse_makefile_targets() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Makefile"),
            "build:\n\tgo build\n\ntest:\n\tgo test\n\n.PHONY: build test\n",
        )
        .unwrap();

        let targets = parse_makefile_targets(dir.path());
        assert!(targets.contains(&"build".to_string()));
        assert!(targets.contains(&"test".to_string()));
        assert!(!targets.contains(&".PHONY".to_string()));
    }

    #[test]
    fn test_parse_makefile_filters_variables() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Makefile"),
            "CC=gcc\nbuild:\n\t$(CC) main.c\n",
        )
        .unwrap();

        let targets = parse_makefile_targets(dir.path());
        assert_eq!(targets, vec!["build"]);
    }

    #[test]
    fn test_parse_makefile_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(parse_makefile_targets(dir.path()).is_empty());
    }

    #[test]
    fn test_parse_npm_scripts() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"scripts": {"dev": "vite", "build": "tsc && vite build"}}"#,
        )
        .unwrap();

        let scripts = parse_npm_scripts(dir.path()).unwrap();
        let names: Vec<&str> = scripts.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"dev"));
        assert!(names.contains(&"build"));

        // Check descriptions
        let dev = scripts.iter().find(|(n, _)| n == "dev").unwrap();
        assert_eq!(dev.1.as_deref(), Some("vite"));
    }

    #[test]
    fn test_parse_npm_scripts_empty() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), r#"{"scripts": {}}"#).unwrap();
        assert!(parse_npm_scripts(dir.path()).is_none());
    }

    #[test]
    fn test_parse_cargo_info_workspace() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crate-a\"]\n",
        )
        .unwrap();

        let (is_workspace, has_bin) = parse_cargo_info(dir.path()).unwrap();
        assert!(is_workspace);
        assert!(!has_bin);
    }

    #[test]
    fn test_parse_cargo_info_bin_targets() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"x\"\n\n[[bin]]\nname = \"cli\"\n",
        )
        .unwrap();

        let (is_workspace, has_bin) = parse_cargo_info(dir.path()).unwrap();
        assert!(!is_workspace);
        assert!(has_bin);
    }

    #[test]
    fn test_parse_docker_services() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("docker-compose.yml"),
            "services:\n  web:\n    image: nginx\n  db:\n    image: postgres\n",
        )
        .unwrap();

        let services = parse_docker_services(dir.path()).unwrap();
        assert_eq!(services, vec!["web", "db"]);
    }

    #[test]
    fn test_parse_docker_services_alternate_filenames() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("compose.yaml"),
            "services:\n  api:\n    build: .\n",
        )
        .unwrap();

        let services = parse_docker_services(dir.path()).unwrap();
        assert_eq!(services, vec!["api"]);
    }

    #[test]
    fn test_parse_docker_services_no_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(parse_docker_services(dir.path()).is_none());
    }

    #[test]
    fn test_parse_justfile_recipes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("justfile"),
            "set shell := [\"bash\"]\n\nbuild:\n  cargo build\n\ntest arg:\n  cargo test {{arg}}\n\nalias b := build\n",
        )
        .unwrap();

        let recipes = parse_justfile_recipes(dir.path());
        assert!(recipes.contains(&"build".to_string()));
        assert!(recipes.contains(&"test".to_string()));
        assert!(!recipes
            .iter()
            .any(|n| n.contains("set") || n.contains("alias")));
    }

    #[test]
    fn test_parse_justfile_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(parse_justfile_recipes(dir.path()).is_empty());
    }

    #[test]
    fn test_parse_python_info_poetry() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("pyproject.toml"),
            "[tool.poetry]\nname = \"myproject\"\n[tool.ruff]\nline-length = 88\n",
        )
        .unwrap();

        let info = parse_python_info(dir.path()).unwrap();
        assert!(info.has_poetry);
        assert!(info.has_ruff);
        assert!(!info.has_venv);
    }

    #[test]
    fn test_parse_python_info_no_project() {
        let dir = tempfile::tempdir().unwrap();
        assert!(parse_python_info(dir.path()).is_none());
    }
}

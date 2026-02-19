use std::path::Path;
use std::time::Duration;

use crate::spec::{ArgSpec, ArgTemplate, CommandSpec, GeneratorSpec, OptionSpec, SubcommandSpec};

/// Auto-generate specs from project files.
///
/// `root` is the detected project/git root (used for cargo and python which
/// are inherently project-root-scoped).
///
/// `cwd` is the actual working directory of the session.  Tools like `make`,
/// `npm run`, `docker compose`, and `just` resolve their config relative to
/// CWD, so we parse from there to match what the user would actually see
/// (important in monorepos where subdirectories have their own config files).
pub fn generate_specs(root: &Path, cwd: &Path) -> Vec<CommandSpec> {
    let mut specs = Vec::new();
    const MAKEFILES: &[&str] = &["Makefile", "makefile", "GNUmakefile"];
    const COMPOSE_FILES: &[&str] = &[
        "docker-compose.yml",
        "docker-compose.yaml",
        "compose.yml",
        "compose.yaml",
    ];
    const JUSTFILES: &[&str] = &["justfile", "Justfile", ".justfile"];

    // Dynamic tools: detect file existence only, completions use generators
    // that run at completion time (always current for the cwd).
    if crate::project::has_any_file(cwd, MAKEFILES) {
        specs.push(make_spec());
    }

    if cwd.join("package.json").exists() {
        let manager = crate::project::detect_package_manager(cwd);
        specs.push(package_json_spec(manager));
    }

    if crate::project::has_any_file(cwd, COMPOSE_FILES) {
        specs.push(docker_compose_spec());
    }

    if crate::project::has_any_file(cwd, JUSTFILES) {
        specs.push(justfile_spec());
    }

    // Static tools: parse project files for structure (not cwd-dynamic).
    if let Some((is_workspace, has_bin_targets)) = crate::project::parse_cargo_info(root) {
        specs.push(cargo_spec(is_workspace, has_bin_targets));
    }

    if let Some(py) = crate::project::parse_python_info(root) {
        if py.has_poetry {
            specs.push(poetry_spec());
        }
        if py.has_ruff {
            specs.push(ruff_spec());
        }
        if py.has_venv {
            specs.push(pytest_spec());
        }
    }

    specs
}

fn opt(short: Option<&str>, long: Option<&str>, description: &str, takes_arg: bool) -> OptionSpec {
    OptionSpec {
        short: short.map(str::to_string),
        long: long.map(str::to_string),
        description: Some(description.to_string()),
        takes_arg,
        ..Default::default()
    }
}

fn sub(name: &str, description: &str) -> SubcommandSpec {
    SubcommandSpec {
        name: name.to_string(),
        description: Some(description.to_string()),
        ..Default::default()
    }
}

fn generated_arg(name: &str, command: &str, variadic: bool) -> ArgSpec {
    ArgSpec {
        name: name.to_string(),
        variadic,
        generator: Some(GeneratorSpec {
            command: command.to_string(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn make_spec() -> CommandSpec {
    CommandSpec {
        name: "make".to_string(),
        options: vec![
            opt(
                Some("-j"),
                Some("--jobs"),
                "Parallel jobs",
                true,
            ),
            opt(
                Some("-n"),
                Some("--dry-run"),
                "Print commands without executing",
                false,
            ),
        ],
        args: vec![generated_arg(
            "target",
            "make -qp 2>/dev/null | awk -F: '/^[a-zA-Z][^$#\\/\\t=]*:([^=]|$)/{split($1,a,/ /);for(i in a)print a[i]}'",
            true,
        )],
        ..Default::default()
    }
}

fn package_json_spec(manager: &str) -> CommandSpec {
    let script_arg = generated_arg(
        "script",
        r#"node -e "Object.keys(require('./package.json').scripts||{}).forEach(s=>console.log(s))""#,
        true,
    );

    let subcommands = if manager == "npm" {
        let mut run = sub("run", "Run a script");
        run.args = vec![script_arg.clone()];
        vec![run]
    } else {
        // yarn/pnpm/bun: scripts are top-level args
        Vec::new()
    };

    let args = if manager != "npm" {
        vec![script_arg]
    } else {
        Vec::new()
    };

    CommandSpec {
        name: manager.to_string(),
        subcommands,
        args,
        ..Default::default()
    }
}

fn cargo_spec(is_workspace: bool, has_bin_targets: bool) -> CommandSpec {
    let mut subcommands: Vec<SubcommandSpec> = [
        ("build", vec!["b"], "Compile the current package"),
        ("test", vec!["t"], "Run tests"),
        ("run", vec!["r"], "Run a binary"),
        ("check", vec!["c"], "Analyze without building"),
        ("clippy", vec![], "Run Clippy lints"),
        ("fmt", vec![], "Format code"),
    ]
    .into_iter()
    .map(|(name, aliases, desc)| SubcommandSpec {
        name: name.into(),
        aliases: aliases.into_iter().map(String::from).collect(),
        description: Some(desc.into()),
        ..Default::default()
    })
    .collect();

    if is_workspace {
        for sub in &mut subcommands {
            if matches!(sub.name.as_str(), "build" | "test" | "check") {
                sub.options.push(opt(
                    None,
                    Some("--workspace"),
                    "Apply to all workspace members",
                    false,
                ));
            }
        }
    }

    if has_bin_targets {
        for sub in &mut subcommands {
            if sub.name == "run" {
                sub.options
                    .push(opt(None, Some("--bin"), "Run specific binary", true));
            }
        }
    }

    CommandSpec {
        name: "cargo".to_string(),
        subcommands,
        ..Default::default()
    }
}

fn docker_compose_spec() -> CommandSpec {
    let service_arg = || {
        generated_arg(
            "service",
            "docker compose config --services 2>/dev/null",
            true,
        )
    };
    let mut up = sub("up", "Start services");
    up.args = vec![service_arg()];
    up.options = vec![
        opt(Some("-d"), Some("--detach"), "Run in background", false),
        opt(None, Some("--build"), "Build before starting", false),
    ];

    let mut logs = sub("logs", "View logs");
    logs.args = vec![service_arg()];
    logs.options = vec![opt(Some("-f"), Some("--follow"), "Follow output", false)];

    let mut restart = sub("restart", "Restart services");
    restart.args = vec![service_arg()];

    let mut build = sub("build", "Build services");
    build.args = vec![service_arg()];

    let subcommands = vec![
        up,
        sub("down", "Stop services"),
        logs,
        restart,
        sub("ps", "List containers"),
        build,
    ];

    CommandSpec {
        name: "docker".to_string(),
        description: Some("Docker Compose (project-local)".into()),
        subcommands: vec![SubcommandSpec {
            name: "compose".into(),
            subcommands,
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn justfile_spec() -> CommandSpec {
    CommandSpec {
        name: "just".to_string(),
        args: vec![generated_arg(
            "recipe",
            "just --summary 2>/dev/null | tr ' ' '\\n'",
            true,
        )],
        ..Default::default()
    }
}

fn poetry_spec() -> CommandSpec {
    let subcommands = [
        ("install", "Install dependencies"),
        ("run", "Run a command in the venv"),
        ("build", "Build the package"),
        ("lock", "Lock dependencies"),
        ("update", "Update dependencies"),
        ("add", "Add a dependency"),
        ("remove", "Remove a dependency"),
        ("shell", "Activate the venv shell"),
    ]
    .into_iter()
    .map(|(name, desc)| SubcommandSpec {
        name: name.into(),
        description: Some(desc.into()),
        ..Default::default()
    })
    .collect();

    CommandSpec {
        name: "poetry".to_string(),
        subcommands,
        ..Default::default()
    }
}

fn ruff_spec() -> CommandSpec {
    let path_arg = ArgSpec {
        name: "path".into(),
        template: Some(ArgTemplate::FilePaths),
        ..Default::default()
    };

    let mut check = sub("check", "Run linting");
    check.args = vec![path_arg.clone()];
    check.options = vec![opt(
        None,
        Some("--fix"),
        "Fix auto-fixable violations",
        false,
    )];

    let mut format = sub("format", "Format code");
    format.args = vec![path_arg];

    CommandSpec {
        name: "ruff".to_string(),
        subcommands: vec![check, format],
        ..Default::default()
    }
}

/// Discover specs for CLI tools built by the current project by running `--help`.
/// Only runs if the binary has actually been built.
pub async fn discover_project_cli_specs(root: &Path, timeout_ms: u64) -> Vec<CommandSpec> {
    let tools = crate::project::detect_project_cli_tools(root);
    let mut specs = Vec::new();
    let timeout = Duration::from_millis(timeout_ms);

    for tool in tools {
        let Some(binary_path) = tool.binary_path else {
            tracing::debug!("Project CLI tool {} not built yet, skipping", tool.name);
            continue;
        };

        let result = tokio::time::timeout(timeout, async {
            tokio::process::Command::new(&binary_path)
                .arg("--help")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output()
                .await
        })
        .await;

        if let Ok(Ok(output)) = result {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let help_text = if stdout.trim().is_empty() {
                String::from_utf8_lossy(&output.stderr).to_string()
            } else {
                stdout.to_string()
            };

            if !help_text.trim().is_empty() {
                let spec = crate::spec_store::parse_help_basic(&tool.name, &help_text);
                if !spec.subcommands.is_empty() || !spec.options.is_empty() {
                    tracing::info!("Generated spec for project CLI tool: {}", tool.name);
                    specs.push(spec);
                }
            }
        }
    }

    specs
}

fn pytest_spec() -> CommandSpec {
    CommandSpec {
        name: "pytest".to_string(),
        description: Some("Run tests".into()),
        options: vec![
            OptionSpec {
                short: Some("-v".into()),
                long: Some("--verbose".into()),
                description: Some("Increase verbosity".into()),
                ..Default::default()
            },
            OptionSpec {
                short: Some("-x".into()),
                long: Some("--exitfirst".into()),
                description: Some("Stop on first failure".into()),
                ..Default::default()
            },
            OptionSpec {
                short: Some("-k".into()),
                takes_arg: true,
                description: Some("Filter by expression".into()),
                ..Default::default()
            },
            OptionSpec {
                long: Some("--tb".into()),
                takes_arg: true,
                description: Some("Traceback style (short/long/no)".into()),
                ..Default::default()
            },
        ],
        args: vec![ArgSpec {
            name: "path".into(),
            template: Some(ArgTemplate::FilePaths),
            ..Default::default()
        }],
        ..Default::default()
    }
}

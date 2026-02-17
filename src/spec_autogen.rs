use std::path::Path;
use std::time::Duration;

use crate::spec::{ArgSpec, ArgTemplate, CommandSpec, OptionSpec, SubcommandSpec};

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

    let make_targets = crate::project::parse_makefile_targets(cwd);
    if !make_targets.is_empty() {
        specs.push(make_spec(make_targets));
    }

    if let Some(scripts) = crate::project::parse_npm_scripts(cwd) {
        let manager = crate::project::detect_package_manager(cwd);
        specs.push(package_json_spec(manager, scripts));
    }

    if let Some((is_workspace, has_bin_targets)) = crate::project::parse_cargo_info(root) {
        specs.push(cargo_spec(is_workspace, has_bin_targets));
    }

    if let Some(services) = crate::project::parse_docker_services(cwd) {
        specs.push(docker_compose_spec(services));
    }

    let just_recipes = crate::project::parse_justfile_recipes(cwd);
    if !just_recipes.is_empty() {
        specs.push(justfile_spec(just_recipes));
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

fn make_spec(targets: Vec<String>) -> CommandSpec {
    let subcommands = targets
        .into_iter()
        .map(|name| SubcommandSpec {
            name,
            ..Default::default()
        })
        .collect();

    CommandSpec {
        name: "make".to_string(),
        subcommands,
        options: vec![
            OptionSpec {
                short: Some("-j".into()),
                long: Some("--jobs".into()),
                takes_arg: true,
                description: Some("Parallel jobs".into()),
                ..Default::default()
            },
            OptionSpec {
                short: Some("-n".into()),
                long: Some("--dry-run".into()),
                description: Some("Print commands without executing".into()),
                ..Default::default()
            },
        ],
        ..Default::default()
    }
}

fn package_json_spec(manager: &str, scripts: Vec<(String, Option<String>)>) -> CommandSpec {
    let script_subcmds: Vec<SubcommandSpec> = scripts
        .into_iter()
        .map(|(name, description)| SubcommandSpec {
            name,
            description,
            ..Default::default()
        })
        .collect();

    let subcommands = if manager == "npm" {
        vec![SubcommandSpec {
            name: "run".to_string(),
            description: Some("Run a script".into()),
            subcommands: script_subcmds,
            ..Default::default()
        }]
    } else {
        script_subcmds
    };

    CommandSpec {
        name: manager.to_string(),
        subcommands,
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
                sub.options.push(OptionSpec {
                    long: Some("--workspace".into()),
                    description: Some("Apply to all workspace members".into()),
                    ..Default::default()
                });
            }
        }
    }

    if has_bin_targets {
        for sub in &mut subcommands {
            if sub.name == "run" {
                sub.options.push(OptionSpec {
                    long: Some("--bin".into()),
                    takes_arg: true,
                    description: Some("Run specific binary".into()),
                    ..Default::default()
                });
            }
        }
    }

    CommandSpec {
        name: "cargo".to_string(),
        subcommands,
        ..Default::default()
    }
}

fn docker_compose_spec(services: Vec<String>) -> CommandSpec {
    let service_args = if services.is_empty() {
        Vec::new()
    } else {
        vec![ArgSpec {
            name: "service".into(),
            suggestions: services,
            variadic: true,
            ..Default::default()
        }]
    };

    let subcommands = vec![
        SubcommandSpec {
            name: "up".into(),
            description: Some("Start services".into()),
            args: service_args.clone(),
            options: vec![
                OptionSpec {
                    short: Some("-d".into()),
                    long: Some("--detach".into()),
                    description: Some("Run in background".into()),
                    ..Default::default()
                },
                OptionSpec {
                    long: Some("--build".into()),
                    description: Some("Build before starting".into()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        },
        SubcommandSpec {
            name: "down".into(),
            description: Some("Stop services".into()),
            ..Default::default()
        },
        SubcommandSpec {
            name: "logs".into(),
            description: Some("View logs".into()),
            args: service_args.clone(),
            options: vec![OptionSpec {
                short: Some("-f".into()),
                long: Some("--follow".into()),
                description: Some("Follow output".into()),
                ..Default::default()
            }],
            ..Default::default()
        },
        SubcommandSpec {
            name: "restart".into(),
            description: Some("Restart services".into()),
            args: service_args.clone(),
            ..Default::default()
        },
        SubcommandSpec {
            name: "ps".into(),
            description: Some("List containers".into()),
            ..Default::default()
        },
        SubcommandSpec {
            name: "build".into(),
            description: Some("Build services".into()),
            args: service_args,
            ..Default::default()
        },
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

fn justfile_spec(recipes: Vec<String>) -> CommandSpec {
    let subcommands = recipes
        .into_iter()
        .map(|name| SubcommandSpec {
            name,
            ..Default::default()
        })
        .collect();

    CommandSpec {
        name: "just".to_string(),
        subcommands,
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

    let subcommands = vec![
        SubcommandSpec {
            name: "check".into(),
            description: Some("Run linting".into()),
            args: vec![path_arg.clone()],
            options: vec![OptionSpec {
                long: Some("--fix".into()),
                description: Some("Fix auto-fixable violations".into()),
                ..Default::default()
            }],
            ..Default::default()
        },
        SubcommandSpec {
            name: "format".into(),
            description: Some("Format code".into()),
            args: vec![path_arg],
            ..Default::default()
        },
    ];

    CommandSpec {
        name: "ruff".to_string(),
        subcommands,
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

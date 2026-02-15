use std::path::Path;

use crate::spec::{ArgSpec, CommandSpec, OptionSpec, SubcommandSpec};

/// Auto-generate specs from project files at the given root.
pub fn generate_specs(root: &Path) -> Vec<CommandSpec> {
    let mut specs = Vec::new();

    if let Some(spec) = generate_makefile_spec(root) {
        specs.push(spec);
    }
    if let Some(spec) = generate_package_json_spec(root) {
        specs.push(spec);
    }
    if let Some(spec) = generate_cargo_spec(root) {
        specs.push(spec);
    }
    if let Some(spec) = generate_docker_compose_spec(root) {
        specs.push(spec);
    }
    if let Some(spec) = generate_justfile_spec(root) {
        specs.push(spec);
    }

    specs
}

fn generate_makefile_spec(root: &Path) -> Option<CommandSpec> {
    let path = root.join("Makefile");
    let content = std::fs::read_to_string(path).ok()?;
    let mut subcommands = Vec::new();

    for line in content.lines() {
        if let Some(target) = line.split(':').next() {
            let target = target.trim();
            if !target.is_empty()
                && !target.starts_with('#')
                && !target.starts_with('.')
                && !target.starts_with('\t')
                && !target.contains(' ')
                && !target.contains('$')
                && !target.contains('=')
            {
                subcommands.push(SubcommandSpec {
                    name: target.to_string(),
                    description: None,
                    ..Default::default()
                });
            }
        }
    }

    if subcommands.is_empty() {
        return None;
    }

    Some(CommandSpec {
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
    })
}

fn generate_package_json_spec(root: &Path) -> Option<CommandSpec> {
    let path = root.join("package.json");
    let content = std::fs::read_to_string(path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    let scripts = json.get("scripts")?.as_object()?;

    if scripts.is_empty() {
        return None;
    }

    // Detect package manager
    let pm = if root.join("pnpm-lock.yaml").exists() {
        "pnpm"
    } else if root.join("yarn.lock").exists() {
        "yarn"
    } else if root.join("bun.lockb").exists() || root.join("bun.lock").exists() {
        "bun"
    } else {
        "npm"
    };

    let mut subcommands = Vec::new();

    if pm == "npm" {
        // For npm, scripts go under a "run" subcommand
        let script_subcmds: Vec<SubcommandSpec> = scripts
            .keys()
            .map(|name| SubcommandSpec {
                name: name.clone(),
                description: scripts.get(name).and_then(|v| v.as_str()).map(String::from),
                ..Default::default()
            })
            .collect();

        subcommands.push(SubcommandSpec {
            name: "run".to_string(),
            description: Some("Run a script".into()),
            subcommands: script_subcmds,
            ..Default::default()
        });
    } else {
        // For pnpm/yarn/bun, scripts are direct subcommands
        for name in scripts.keys() {
            subcommands.push(SubcommandSpec {
                name: name.clone(),
                description: scripts.get(name).and_then(|v| v.as_str()).map(String::from),
                ..Default::default()
            });
        }
    }

    Some(CommandSpec {
        name: pm.to_string(),
        subcommands,
        ..Default::default()
    })
}

fn generate_cargo_spec(root: &Path) -> Option<CommandSpec> {
    let path = root.join("Cargo.toml");
    if !path.exists() {
        return None;
    }

    let content = std::fs::read_to_string(&path).ok()?;
    let mut subcommands = vec![
        SubcommandSpec {
            name: "build".into(),
            aliases: vec!["b".into()],
            description: Some("Compile the current package".into()),
            ..Default::default()
        },
        SubcommandSpec {
            name: "test".into(),
            aliases: vec!["t".into()],
            description: Some("Run tests".into()),
            ..Default::default()
        },
        SubcommandSpec {
            name: "run".into(),
            aliases: vec!["r".into()],
            description: Some("Run a binary".into()),
            ..Default::default()
        },
        SubcommandSpec {
            name: "check".into(),
            aliases: vec!["c".into()],
            description: Some("Analyze without building".into()),
            ..Default::default()
        },
        SubcommandSpec {
            name: "clippy".into(),
            description: Some("Run Clippy lints".into()),
            ..Default::default()
        },
        SubcommandSpec {
            name: "fmt".into(),
            description: Some("Format code".into()),
            ..Default::default()
        },
    ];

    if content.contains("[workspace]") {
        // Add workspace-specific options to build/test/check
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

    // Extract binary targets for `cargo run --bin`
    if content.contains("[[bin]]") {
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

    Some(CommandSpec {
        name: "cargo".to_string(),
        subcommands,
        ..Default::default()
    })
}

fn generate_docker_compose_spec(root: &Path) -> Option<CommandSpec> {
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

    // Extract service names
    let mut services = Vec::new();
    let mut in_services = false;
    for line in content.lines() {
        if line.trim() == "services:" {
            in_services = true;
            continue;
        }
        if in_services {
            if line.starts_with("  ") && !line.starts_with("    ") {
                if let Some(name) = line.trim().strip_suffix(':') {
                    let name = name.trim();
                    if !name.is_empty() && !name.starts_with('#') {
                        services.push(name.to_string());
                    }
                }
            }
            if !line.is_empty() && !line.starts_with(' ') && !line.starts_with('#') {
                in_services = false;
            }
        }
    }

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

    Some(CommandSpec {
        name: "docker".to_string(),
        description: Some("Docker Compose (project-local)".into()),
        subcommands: vec![SubcommandSpec {
            name: "compose".into(),
            subcommands,
            ..Default::default()
        }],
        ..Default::default()
    })
}

fn generate_justfile_spec(root: &Path) -> Option<CommandSpec> {
    let path = if root.join("Justfile").exists() {
        root.join("Justfile")
    } else if root.join("justfile").exists() {
        root.join("justfile")
    } else {
        return None;
    };

    let content = std::fs::read_to_string(path).ok()?;
    let mut subcommands = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty()
            && !trimmed.starts_with('#')
            && !trimmed.starts_with(' ')
            && !trimmed.starts_with('\t')
            && !trimmed.starts_with("set ")
            && !trimmed.starts_with("export ")
            && !trimmed.starts_with("alias ")
        {
            if let Some(name) = trimmed.split(':').next() {
                let name = name.split_whitespace().next().unwrap_or(name).trim();
                if !name.is_empty() && !name.contains('=') {
                    subcommands.push(SubcommandSpec {
                        name: name.to_string(),
                        ..Default::default()
                    });
                }
            }
        }
    }

    if subcommands.is_empty() {
        return None;
    }

    Some(CommandSpec {
        name: "just".to_string(),
        subcommands,
        ..Default::default()
    })
}

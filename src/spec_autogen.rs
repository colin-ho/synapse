use std::path::Path;

use crate::spec::{ArgSpec, CommandSpec, OptionSpec, SubcommandSpec};

/// Auto-generate specs from project files at the given root.
pub fn generate_specs(root: &Path) -> Vec<CommandSpec> {
    let mut specs = Vec::new();

    let make_targets = crate::project::parse_makefile_targets(root);
    if !make_targets.is_empty() {
        specs.push(make_spec(make_targets));
    }

    if let Some(scripts) = crate::project::parse_npm_scripts(root) {
        let manager = crate::project::detect_package_manager(root);
        specs.push(package_json_spec(manager, scripts));
    }

    if let Some((is_workspace, has_bin_targets)) = crate::project::parse_cargo_info(root) {
        specs.push(cargo_spec(is_workspace, has_bin_targets));
    }

    if let Some(services) = crate::project::parse_docker_services(root) {
        specs.push(docker_compose_spec(services));
    }

    let just_recipes = crate::project::parse_justfile_recipes(root);
    if !just_recipes.is_empty() {
        specs.push(justfile_spec(just_recipes));
    }

    // Python: no structured spec yet

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

fn package_json_spec(
    manager: &str,
    scripts: Vec<(String, Option<String>)>,
) -> CommandSpec {
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

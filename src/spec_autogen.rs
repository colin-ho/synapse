use std::path::Path;
use std::time::Duration;

use crate::spec::{ArgSpec, CommandSpec, GeneratorSpec, OptionSpec, SubcommandSpec};

/// Auto-generate specs from project files.
///
/// Only generates specs for dynamic tools that use generators to read
/// project-specific config at completion time (Makefile targets, npm scripts,
/// docker-compose services, just recipes). Static tools like cargo, pytest,
/// poetry, and ruff are better served by their own completion generators
/// or system zsh completion files, which are far more comprehensive than
/// any hardcoded spec we could maintain.
///
/// `cwd` is the actual working directory of the session.  Tools like `make`,
/// `npm run`, `docker compose`, and `just` resolve their config relative to
/// CWD, so we parse from there to match what the user would actually see
/// (important in monorepos where subdirectories have their own config files).
pub fn generate_specs(cwd: &Path) -> Vec<CommandSpec> {
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

/// Discover specs for CLI tools built by the current project by running `--help`.
/// Only runs if the binary has actually been built.
pub async fn discover_project_cli_specs(root: &Path, timeout_ms: u64) -> Vec<CommandSpec> {
    let tools = crate::project::detect_project_cli_tools(root);
    let mut specs = Vec::new();
    let timeout = Duration::from_millis(timeout_ms);

    let scratch = std::env::temp_dir().join("synapse-discovery");
    let _ = std::fs::create_dir_all(&scratch);

    for tool in tools {
        let Some(binary_path) = tool.binary_path else {
            continue;
        };

        let result = tokio::time::timeout(timeout, async {
            let mut cmd = tokio::process::Command::new(&binary_path);
            cmd.arg("--help");
            crate::spec_store::sandbox_command(&mut cmd, &scratch);
            cmd.output().await
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
                    specs.push(spec);
                }
            }
        }
    }

    specs
}

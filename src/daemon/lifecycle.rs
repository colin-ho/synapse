use std::path::PathBuf;
use std::sync::Arc;

use tokio::net::UnixListener;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

use crate::config::Config;
use crate::logging::InteractionLogger;
use crate::spec_store::SpecStore;

use super::server::run_server;
use super::state::RuntimeState;

fn resolve_completions_dir(config: &Config, output_dir: Option<PathBuf>) -> PathBuf {
    output_dir.unwrap_or_else(|| {
        config
            .completions
            .output_dir
            .as_ref()
            .map(|s| {
                PathBuf::from(
                    s.replace('~', &dirs::home_dir().unwrap_or_default().to_string_lossy()),
                )
            })
            .unwrap_or_else(crate::compsys_export::completions_dir)
    })
}

fn resolve_cwd(cwd: Option<PathBuf>) -> PathBuf {
    cwd.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")))
}

fn parse_complete_result_line(line: &str) -> Vec<(String, String)> {
    let fields: Vec<&str> = line.split('\t').collect();
    if fields.first() != Some(&"complete_result") {
        return Vec::new();
    }

    let count: usize = fields.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    (0..count)
        .map(|i| {
            (
                fields.get(2 + i * 2).unwrap_or(&"").to_string(),
                fields.get(3 + i * 2).unwrap_or(&"").to_string(),
            )
        })
        .collect()
}

pub(super) fn stop_daemon(socket_path: Option<PathBuf>) -> anyhow::Result<()> {
    let config = Config::load().with_socket_override(socket_path);
    let pid_path = config.pid_path();

    if !pid_path.exists() {
        println!("Daemon is not running (no PID file)");
        return Ok(());
    }

    let pid_str = std::fs::read_to_string(&pid_path)?;
    let pid: i32 = pid_str.trim().parse()?;

    match nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid),
        nix::sys::signal::Signal::SIGTERM,
    ) {
        Ok(()) => {
            println!("Sent SIGTERM to daemon (PID {pid})");
            let _ = std::fs::remove_file(&pid_path);
            let _ = std::fs::remove_file(config.socket_path());
        }
        Err(nix::errno::Errno::ESRCH) => {
            println!("Daemon not running (stale PID file), cleaning up");
            let _ = std::fs::remove_file(&pid_path);
            let _ = std::fs::remove_file(config.socket_path());
        }
        Err(e) => {
            eprintln!("Failed to stop daemon: {e}");
        }
    }

    Ok(())
}

pub(super) fn show_status(socket_path: Option<PathBuf>) -> anyhow::Result<()> {
    let config = Config::load().with_socket_override(socket_path);
    let pid_path = config.pid_path();
    let socket_path = config.socket_path();

    if !pid_path.exists() {
        println!("Daemon is not running (no PID file)");
        return Ok(());
    }

    let pid_str = std::fs::read_to_string(&pid_path)?;
    let pid: i32 = pid_str.trim().parse()?;

    match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) {
        Ok(()) => {
            println!("Daemon is running (PID {pid})");
            println!("Socket: {}", socket_path.display());
        }
        Err(_) => {
            println!("Daemon is not running (stale PID file for PID {pid})");
        }
    }

    Ok(())
}

pub(super) async fn start_daemon(
    verbose: u8,
    log_file: Option<PathBuf>,
    foreground: bool,
    socket_path: Option<PathBuf>,
) -> anyhow::Result<()> {
    let config = Config::load().with_socket_override(socket_path);

    let level = match verbose {
        0 => config.general.log_level.as_str(),
        1 => "info",
        2 => "debug",
        _ => "trace",
    };

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));

    let resolved_log = log_file.or_else(|| config.daemon_log_path());

    if let Some(log_path) = resolved_log {
        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)?;
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(file)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }

    if !foreground {
        tracing::info!("Starting daemon in foreground mode");
    }

    let pid_path = config.pid_path();
    if pid_path.exists() {
        if let Ok(pid_str) = std::fs::read_to_string(&pid_path) {
            if let Ok(pid) = pid_str.trim().parse::<i32>() {
                if nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok() {
                    eprintln!("Daemon already running (PID {pid})");
                    std::process::exit(1);
                }
            }
        }
        let _ = std::fs::remove_file(&pid_path);
    }

    let socket_path = config.socket_path();

    if socket_path.exists() {
        std::fs::remove_file(&socket_path)?;
    }

    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    std::fs::write(&pid_path, std::process::id().to_string())?;

    let listener = UnixListener::bind(&socket_path)?;
    tracing::info!("Listening on {}", socket_path.display());

    let llm_client = if let Some(mut client) =
        crate::llm::LlmClient::from_config(&config.llm, config.security.scrub_paths)
    {
        tracing::info!("LLM enabled (provider: {})", config.llm.provider);
        client.auto_detect_model().await;
        client.probe_health().await;
        Some(Arc::new(client))
    } else {
        if config.llm.enabled {
            tracing::warn!(
                "LLM enabled in config but API key env var not set, falling back to regex"
            );
        }
        None
    };

    let completions_dir = resolve_completions_dir(&config, None);
    let spec_store = Arc::new(SpecStore::with_completions_dir(
        config.spec.clone(),
        completions_dir,
    ));

    let interaction_logger = InteractionLogger::new(
        config.interaction_log_path(),
        config.logging.max_log_size_mb,
    );

    let nl_cache = crate::nl_cache::NlCache::new();

    let shutdown = CancellationToken::new();

    let state = Arc::new(
        RuntimeState::new(
            spec_store,
            interaction_logger,
            config.clone(),
            llm_client,
            nl_cache,
        )
        .with_shutdown_token(shutdown.clone()),
    );

    let result = run_server(listener, state, shutdown).await;

    tracing::info!("Shutting down");
    let _ = std::fs::remove_file(config.socket_path());
    let _ = std::fs::remove_file(config.pid_path());

    result
}

pub(super) async fn add_command(
    command: String,
    output_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
    let config = Config::load();
    let completions_dir = resolve_completions_dir(&config, output_dir);

    let spec_store = SpecStore::with_completions_dir(config.spec.clone(), completions_dir);

    if !spec_store.can_discover_command(&command) {
        eprintln!("Cannot discover '{command}': blocked by safety blocklist or config");
        std::process::exit(1);
    }

    if spec_store.has_system_completion(&command) {
        eprintln!("'{command}' already has completions installed (found in zsh fpath)");
        std::process::exit(1);
    }

    let cwd = std::env::current_dir().ok();
    match spec_store.discover_command(&command, cwd.as_deref()).await {
        Some((spec, path)) => {
            let n_opts = spec.options.len();
            let n_subs = spec.subcommands.len();
            println!("Discovered {command}: {n_opts} options, {n_subs} subcommands",);
            println!("  Wrote {}", path.display());
        }
        None => {
            eprintln!("No spec discovered for '{command}' (--help produced no parseable output)");
            std::process::exit(1);
        }
    }

    Ok(())
}

pub(super) async fn scan_project(
    output_dir: Option<PathBuf>,
    force: bool,
    no_gap_check: bool,
) -> anyhow::Result<()> {
    let config = Config::load();
    let output = resolve_completions_dir(&config, output_dir);

    let gap_only = !no_gap_check && !force;
    let existing = if gap_only {
        crate::zsh_completion::scan_available_commands()
    } else {
        std::collections::HashSet::new()
    };

    if force && output.exists() {
        for entry in std::fs::read_dir(&output)?.flatten() {
            let _ = std::fs::remove_file(entry.path());
        }
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let spec_store = SpecStore::new(config.spec.clone());
    let project_specs: Vec<_> = spec_store.lookup_all_project_specs(&cwd).await;

    let mut report =
        crate::compsys_export::generate_all(&project_specs, &existing, &output, gap_only)?;

    if !force {
        let generated_set: std::collections::HashSet<String> =
            report.generated.iter().cloned().collect();
        report.removed = crate::compsys_export::remove_stale_project_auto(&output, &generated_set)?;
    }

    println!(
        "Generated {} completions in {}",
        report.generated.len(),
        output.display()
    );
    if !report.skipped_existing.is_empty() {
        println!(
            "Skipped {} commands with existing compsys functions",
            report.skipped_existing.len()
        );
    }
    for name in &report.generated {
        println!("  _{name}");
    }
    if !report.removed.is_empty() {
        println!("Removed {} stale project completions", report.removed.len());
        for name in &report.removed {
            println!("  _{name}");
        }
    }

    Ok(())
}

pub(super) async fn run_generator_query(
    command: String,
    cwd: Option<PathBuf>,
    strip_prefix: Option<String>,
    split_on: Option<String>,
) -> anyhow::Result<()> {
    let config = Config::load();
    let cwd = resolve_cwd(cwd);
    let socket_path = config.socket_path();

    let mut request = serde_json::json!({
        "type": "run_generator",
        "command": command,
        "cwd": cwd.to_string_lossy(),
    });
    if let Some(ref value) = split_on {
        request["split_on"] = serde_json::Value::String(value.clone());
    }
    if let Some(ref value) = strip_prefix {
        request["strip_prefix"] = serde_json::Value::String(value.clone());
    }

    let line =
        super::rpc::request_tsv_json(&socket_path, &request, std::time::Duration::from_secs(5))
            .await?;
    print_complete_result_values(&line);
    Ok(())
}

/// Parse a `complete_result` TSV line and print each non-empty value.
fn print_complete_result_values(line: &str) {
    for (value, _) in parse_complete_result_line(line) {
        if !value.is_empty() {
            println!("{value}");
        }
    }
}

pub(super) async fn run_complete_query(
    command: String,
    context: Vec<String>,
    cwd: Option<PathBuf>,
) -> anyhow::Result<()> {
    let config = Config::load();
    let cwd = resolve_cwd(cwd);
    let socket_path = config.socket_path();
    let request = serde_json::json!({
        "type": "complete",
        "command": command,
        "context": context,
        "cwd": cwd.to_string_lossy(),
    });

    let line =
        super::rpc::request_tsv_json(&socket_path, &request, std::time::Duration::from_secs(5))
            .await?;
    print_complete_result_values_with_descriptions(&line);
    Ok(())
}

fn print_complete_result_values_with_descriptions(line: &str) {
    for (value, desc) in parse_complete_result_line(line) {
        if desc.is_empty() {
            println!("{value}");
        } else {
            println!("{value}\t{desc}");
        }
    }
}

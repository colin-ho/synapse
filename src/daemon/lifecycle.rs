use std::path::PathBuf;
use std::sync::Arc;

use tokio::net::UnixListener;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

use crate::config::Config;
use crate::logging::InteractionLogger;
use crate::session::SessionManager;
use crate::spec_store::SpecStore;

use super::server::run_server;
use super::state::RuntimeState;

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
            // Clean up files
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

    // Set up tracing
    let level = match verbose {
        0 => config.general.log_level.as_str(),
        1 => "info",
        2 => "debug",
        _ => "trace",
    };

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));

    if let Some(log_path) = log_file {
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
        // Simple daemonization: fork and exit parent
        // For now, just run in foreground — proper daemonize can be added later
        tracing::info!("Starting daemon in foreground mode");
    }

    // Check for existing daemon
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
        // Stale PID file
        let _ = std::fs::remove_file(&pid_path);
    }

    let socket_path = config.socket_path();

    // Remove stale socket
    if socket_path.exists() {
        std::fs::remove_file(&socket_path)?;
    }

    // Ensure parent directory exists
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Write PID file
    std::fs::write(&pid_path, std::process::id().to_string())?;

    // Bind socket
    let listener = UnixListener::bind(&socket_path)?;
    tracing::info!("Listening on {}", socket_path.display());

    // Init LLM client (if configured)
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

    // Init discovery LLM client (if configured separately from main LLM)
    let discovery_llm_client = if let Some(ref discovery_config) = config.llm.discovery {
        let resolved = discovery_config.resolve(&config.llm);
        if let Some(mut client) =
            crate::llm::LlmClient::from_config(&resolved, config.security.scrub_paths)
        {
            tracing::info!(
                "Discovery LLM enabled (provider: {}, model: {})",
                resolved.provider,
                resolved.model
            );
            client.auto_detect_model().await;
            client.probe_health().await;
            Some(Arc::new(client))
        } else {
            tracing::warn!(
                "Discovery LLM config present but client creation failed, using main LLM"
            );
            llm_client.clone()
        }
    } else {
        llm_client.clone()
    };

    // Init spec system
    let completions_dir = config
        .completions
        .output_dir
        .as_ref()
        .map(|s| {
            PathBuf::from(s.replace('~', &dirs::home_dir().unwrap_or_default().to_string_lossy()))
        })
        .unwrap_or_else(crate::compsys_export::completions_dir);
    let spec_store = Arc::new(
        SpecStore::with_completions_dir(config.spec.clone(), discovery_llm_client, completions_dir)
            .with_auto_regenerate(config.completions.auto_regenerate),
    );

    let session_manager = SessionManager::new();
    let interaction_logger = InteractionLogger::new(
        config.interaction_log_path(),
        config.logging.max_log_size_mb,
    );

    let nl_cache = crate::nl_cache::NlCache::new();

    let shutdown = CancellationToken::new();

    let state = Arc::new(
        RuntimeState::new(
            spec_store,
            session_manager,
            interaction_logger,
            config.clone(),
            llm_client,
            nl_cache,
        )
        .with_shutdown_token(shutdown.clone()),
    );

    // Main loop
    let result = run_server(listener, state, shutdown).await;

    // Cleanup
    tracing::info!("Shutting down");
    let _ = std::fs::remove_file(config.socket_path());
    let _ = std::fs::remove_file(config.pid_path());

    result
}

pub(super) async fn generate_completions(
    output_dir: Option<PathBuf>,
    force: bool,
    no_gap_check: bool,
) -> anyhow::Result<()> {
    let config = Config::load();
    let output = output_dir.unwrap_or_else(|| {
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
    });

    let gap_only = !no_gap_check && !force;
    let existing = if gap_only {
        crate::zsh_completion::scan_available_commands()
    } else {
        std::collections::HashSet::new()
    };

    // If force, remove existing generated files first
    if force && output.exists() {
        for entry in std::fs::read_dir(&output)?.flatten() {
            let _ = std::fs::remove_file(entry.path());
        }
    }

    // Collect project specs for the current working directory
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let spec_store = SpecStore::new(config.spec.clone(), None);
    let project_specs: Vec<_> = spec_store.lookup_all_project_specs(&cwd).await;

    let report = crate::compsys_export::generate_all(&project_specs, &existing, &output, gap_only)?;

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

    Ok(())
}

pub(super) async fn run_complete_query(
    command: String,
    context: Vec<String>,
    cwd: Option<PathBuf>,
) -> anyhow::Result<()> {
    let config = Config::load();
    let cwd = cwd.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")));

    // Try to connect to running daemon first
    let socket_path = config.socket_path();
    if socket_path.exists() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        if let Ok(mut stream) = tokio::net::UnixStream::connect(&socket_path).await {
            let request = serde_json::json!({
                "type": "complete",
                "command": command,
                "context": context,
                "cwd": cwd.to_string_lossy(),
            });
            let mut request_line = serde_json::to_string(&request)?;
            request_line.push('\n');
            stream.write_all(request_line.as_bytes()).await?;

            let (reader, _) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let mut line = String::new();
            let timeout = std::time::Duration::from_secs(5);
            match tokio::time::timeout(timeout, reader.read_line(&mut line)).await {
                Ok(Ok(n)) if n > 0 => {
                    let line = line.trim();
                    // Parse TSV: complete_result\tN\tval1\tdesc1\tval2\tdesc2...
                    let fields: Vec<&str> = line.split('\t').collect();
                    if fields.first() == Some(&"complete_result") {
                        let count: usize = fields.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                        for i in 0..count {
                            let val = fields.get(2 + i * 2).unwrap_or(&"");
                            let desc = fields.get(3 + i * 2).unwrap_or(&"");
                            if desc.is_empty() {
                                println!("{val}");
                            } else {
                                println!("{val}\t{desc}");
                            }
                        }
                    }
                    return Ok(());
                }
                _ => {
                    // Daemon didn't respond, fall through to offline mode
                }
            }
        }
    }

    // Offline fallback: use spec store directly
    let spec_store = SpecStore::new(config.spec.clone(), None);
    if let Some(spec) = spec_store.lookup(&command, &cwd).await {
        // Walk context to find the right level
        let mut current_subs = &spec.subcommands;
        let mut current_args = &spec.args;

        for ctx_part in &context {
            if ctx_part == "target" || ctx_part == "subcommand" {
                for sub in current_subs {
                    if let Some(ref desc) = sub.description {
                        println!("{}\t{desc}", sub.name);
                    } else {
                        println!("{}", sub.name);
                    }
                }
                return Ok(());
            }
            if let Some(sub) = current_subs
                .iter()
                .find(|s| s.name == *ctx_part || s.aliases.iter().any(|a| a == ctx_part))
            {
                current_subs = &sub.subcommands;
                current_args = &sub.args;
            }
        }

        // Return subcommands if at current level
        if !current_subs.is_empty() {
            for sub in current_subs {
                if let Some(ref desc) = sub.description {
                    println!("{}\t{desc}", sub.name);
                } else {
                    println!("{}", sub.name);
                }
            }
        }

        // Return static suggestions from args
        for arg in current_args {
            for s in &arg.suggestions {
                println!("{s}");
            }
        }
    }

    Ok(())
}

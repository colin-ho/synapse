use std::path::PathBuf;
use std::sync::Arc;

use tokio::net::UnixListener;
use tracing_subscriber::EnvFilter;

use crate::config::Config;
use crate::logging::InteractionLogger;
use crate::providers::environment::EnvironmentProvider;
use crate::providers::filesystem::FilesystemProvider;
use crate::providers::history::HistoryProvider;
use crate::providers::spec::SpecProvider;
use crate::providers::Provider;
use crate::ranking::Ranker;
use crate::session::SessionManager;
use crate::spec_store::SpecStore;
use crate::workflow::WorkflowPredictor;

use super::server::run_server;
use super::state::RuntimeState;

pub(super) fn stop_daemon(socket_path: Option<PathBuf>) -> anyhow::Result<()> {
    let config = Config::load().with_socket_override(socket_path);
    let pid_path = config.pid_path();

    if !pid_path.exists() {
        eprintln!("Daemon is not running (no PID file)");
        return Ok(());
    }

    let pid_str = std::fs::read_to_string(&pid_path)?;
    let pid: i32 = pid_str.trim().parse()?;

    match nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid),
        nix::sys::signal::Signal::SIGTERM,
    ) {
        Ok(()) => {
            eprintln!("Sent SIGTERM to daemon (PID {pid})");
            // Clean up files
            let _ = std::fs::remove_file(&pid_path);
            let _ = std::fs::remove_file(config.socket_path());
        }
        Err(nix::errno::Errno::ESRCH) => {
            eprintln!("Daemon not running (stale PID file), cleaning up");
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
        eprintln!("Daemon is not running (no PID file)");
        return Ok(());
    }

    let pid_str = std::fs::read_to_string(&pid_path)?;
    let pid: i32 = pid_str.trim().parse()?;

    match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) {
        Ok(()) => {
            eprintln!("Daemon is running (PID {pid})");
            eprintln!("Socket: {}", socket_path.display());
        }
        Err(_) => {
            eprintln!("Daemon is not running (stale PID file for PID {pid})");
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

    // Init components
    let history_provider = HistoryProvider::new(config.history.clone());
    history_provider.load_history().await;

    // Init LLM client (if configured)
    let llm_client =
        crate::llm::LlmClient::from_config(&config.llm, config.security.scrub_paths).map(Arc::new);
    if llm_client.is_some() {
        tracing::info!(
            "LLM spec discovery enabled (provider: {})",
            config.llm.provider
        );
    } else if config.llm.enabled {
        tracing::warn!("LLM enabled in config but API key env var not set, falling back to regex");
    }

    // Init spec system
    let spec_store = Arc::new(SpecStore::new(config.spec.clone(), llm_client));
    let spec_provider = SpecProvider::new();

    // Init filesystem and environment providers
    let filesystem_provider = FilesystemProvider::new();
    let environment_provider = EnvironmentProvider::new();
    environment_provider.scan_path().await;

    let providers = vec![
        Provider::History(Arc::new(history_provider)),
        Provider::Spec(Arc::new(spec_provider)),
        Provider::Filesystem(Arc::new(filesystem_provider)),
        Provider::Environment(Arc::new(environment_provider)),
    ];

    let ranker = Ranker::new(config.weights.clone());
    let workflow_predictor = WorkflowPredictor::new();

    let session_manager = SessionManager::new();
    let interaction_logger = InteractionLogger::new(
        config.interaction_log_path(),
        config.logging.max_log_size_mb,
    );

    let state = Arc::new(RuntimeState::new(
        providers,
        spec_store,
        ranker,
        workflow_predictor,
        session_manager,
        interaction_logger,
        config.clone(),
    ));

    // Main loop
    let result = run_server(listener, state).await;

    // Cleanup
    tracing::info!("Shutting down");
    let _ = std::fs::remove_file(config.socket_path());
    let _ = std::fs::remove_file(config.pid_path());

    result
}

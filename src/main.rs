mod cache;
mod config;
mod logging;
mod protocol;
mod providers;
mod ranking;
mod security;
mod session;

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tracing_subscriber::EnvFilter;

use crate::config::Config;
use crate::logging::InteractionLogger;
use crate::protocol::{Request, Response, SuggestionResponse};
use crate::providers::ai::AiProvider;
use crate::providers::context::ContextProvider;
use crate::providers::history::HistoryProvider;
use crate::providers::SuggestionProvider;
use crate::ranking::Ranker;
use crate::security::Scrubber;
use crate::session::SessionManager;

#[derive(Parser)]
#[command(name = "synapse", about = "Intelligent Zsh command suggestions")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Print the path to the Zsh plugin for sourcing
    #[arg(long)]
    shell_init: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage the synapse daemon
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
}

#[derive(Subcommand)]
enum DaemonAction {
    /// Start the daemon
    Start {
        /// Increase log verbosity (-v info, -vv debug, -vvv trace)
        #[arg(short, long, action = clap::ArgAction::Count)]
        verbose: u8,

        /// Log to file instead of stderr
        #[arg(long)]
        log_file: Option<PathBuf>,

        /// Run in foreground (don't daemonize)
        #[arg(long)]
        foreground: bool,
    },
    /// Stop the daemon
    Stop,
    /// Show daemon status
    Status,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    if cli.shell_init {
        print_shell_init();
        return Ok(());
    }

    match cli.command {
        Some(Commands::Daemon { action }) => match action {
            DaemonAction::Start {
                verbose,
                log_file,
                foreground,
            } => {
                start_daemon(verbose, log_file, foreground).await?;
            }
            DaemonAction::Stop => {
                stop_daemon()?;
            }
            DaemonAction::Status => {
                show_status()?;
            }
        },
        None => {
            eprintln!("Usage: synapse daemon start|stop|status");
            eprintln!("       synapse --shell-init");
            std::process::exit(1);
        }
    }

    Ok(())
}

fn print_shell_init() {
    // Find the plugin directory relative to the binary
    let exe = std::env::current_exe().unwrap_or_default();
    let plugin_dir = exe
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join("plugin"))
        .unwrap_or_else(|| PathBuf::from("plugin"));

    let plugin_path = plugin_dir.join("synapse.zsh");
    if plugin_path.exists() {
        println!("{}", plugin_path.display());
    } else {
        // Fallback: check alongside the binary
        let alt = exe.parent().unwrap_or(std::path::Path::new(".")).join("plugin").join("synapse.zsh");
        if alt.exists() {
            println!("{}", alt.display());
        } else {
            eprintln!("Warning: plugin file not found. Expected at {}", plugin_path.display());
            println!("{}", plugin_path.display());
        }
    }
}

fn stop_daemon() -> anyhow::Result<()> {
    let config = Config::load();
    let pid_path = config.pid_path();

    if !pid_path.exists() {
        eprintln!("Daemon is not running (no PID file)");
        return Ok(());
    }

    let pid_str = std::fs::read_to_string(&pid_path)?;
    let pid: i32 = pid_str.trim().parse()?;

    match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), nix::sys::signal::Signal::SIGTERM)
    {
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

fn show_status() -> anyhow::Result<()> {
    let config = Config::load();
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

async fn start_daemon(
    verbose: u8,
    log_file: Option<PathBuf>,
    foreground: bool,
) -> anyhow::Result<()> {
    let config = Config::load();

    // Set up tracing
    let level = match verbose {
        0 => config.general.log_level.as_str(),
        1 => "info",
        2 => "debug",
        _ => "trace",
    };

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(level));

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
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .init();
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
    let history_provider = Arc::new(HistoryProvider::new(config.history.clone()));
    history_provider.load_history().await;

    let context_provider = Arc::new(ContextProvider::new(config.context.clone()));

    // Set up scrubber for external AI providers
    let scrubber = if config.ai.provider != "ollama" {
        Some(Arc::new(Scrubber::new(config.security.clone())))
    } else {
        None
    };
    let ai_provider = Arc::new(AiProvider::new(config.ai.clone(), scrubber));

    let ranker = Arc::new(Ranker::new(config.weights.clone()));

    let session_manager = SessionManager::new();
    let interaction_logger = Arc::new(InteractionLogger::new(
        config.interaction_log_path(),
        config.logging.max_log_size_mb,
    ));

    let config = Arc::new(config);

    // Main loop
    let result = run_server(
        listener,
        history_provider,
        context_provider,
        ai_provider,
        ranker,
        session_manager,
        interaction_logger,
        config.clone(),
    )
    .await;

    // Cleanup
    tracing::info!("Shutting down");
    let _ = std::fs::remove_file(&config.socket_path());
    let _ = std::fs::remove_file(&config.pid_path());

    result
}

async fn run_server(
    listener: UnixListener,
    history_provider: Arc<HistoryProvider>,
    context_provider: Arc<ContextProvider>,
    ai_provider: Arc<AiProvider>,
    ranker: Arc<Ranker>,
    session_manager: SessionManager,
    interaction_logger: Arc<InteractionLogger>,
    config: Arc<Config>,
) -> anyhow::Result<()> {
    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, _addr)) => {
                        let hp = history_provider.clone();
                        let cp = context_provider.clone();
                        let ap = ai_provider.clone();
                        let rk = ranker.clone();
                        let sm = session_manager.clone();
                        let il = interaction_logger.clone();
                        let cfg = config.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(stream, hp, cp, ap, rk, sm, il, cfg).await {
                                tracing::debug!("Connection error: {e}");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::error!("Accept error: {e}");
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Received Ctrl+C, shutting down");
                break;
            }
        }
    }

    Ok(())
}

async fn handle_connection(
    stream: tokio::net::UnixStream,
    history_provider: Arc<HistoryProvider>,
    context_provider: Arc<ContextProvider>,
    ai_provider: Arc<AiProvider>,
    ranker: Arc<Ranker>,
    session_manager: SessionManager,
    interaction_logger: Arc<InteractionLogger>,
    config: Arc<Config>,
) -> anyhow::Result<()> {
    let (reader, writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let writer = Arc::new(tokio::sync::Mutex::new(writer));
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break; // Connection closed
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        tracing::trace!("Received: {trimmed}");

        let response = match serde_json::from_str::<Request>(trimmed) {
            Ok(request) => handle_request(
                request,
                &history_provider,
                &context_provider,
                &ai_provider,
                &ranker,
                &session_manager,
                &interaction_logger,
                &config,
                writer.clone(),
            )
            .await,
            Err(e) => {
                tracing::warn!("Parse error: {e}");
                Response::Error {
                    message: format!("Invalid request: {e}"),
                }
            }
        };

        let mut response_json = serde_json::to_string(&response)?;
        response_json.push('\n');
        let mut w = writer.lock().await;
        w.write_all(response_json.as_bytes()).await?;
        w.flush().await?;
    }

    Ok(())
}

async fn handle_request(
    request: Request,
    history_provider: &Arc<HistoryProvider>,
    context_provider: &Arc<ContextProvider>,
    ai_provider: &Arc<AiProvider>,
    ranker: &Arc<Ranker>,
    session_manager: &SessionManager,
    interaction_logger: &Arc<InteractionLogger>,
    config: &Arc<Config>,
    writer: Arc<tokio::sync::Mutex<tokio::net::unix::OwnedWriteHalf>>,
) -> Response {
    match request {
        Request::Suggest(req) => {
            tracing::debug!(
                session = %req.session_id,
                buffer = %req.buffer,
                "Suggest request"
            );

            session_manager.update_from_request(&req).await;

            // Phase 1: Immediate — query history + context concurrently
            let (history_result, context_result) = tokio::join!(
                history_provider.suggest(&req),
                context_provider.suggest(&req),
            );

            let mut suggestions = Vec::new();
            if let Some(s) = history_result {
                suggestions.push(s);
            }
            if let Some(s) = context_result {
                suggestions.push(s);
            }

            // Rank immediate results
            let ranked = ranker.rank(suggestions, &req.recent_commands);

            let current_score = ranked.as_ref().map(|r| r.score).unwrap_or(0.0);

            let response = match ranked {
                Some(r) => {
                    let mut text = r.text;
                    if text.len() > config.general.max_suggestion_length {
                        text.truncate(config.general.max_suggestion_length);
                    }

                    let resp = SuggestionResponse {
                        text,
                        source: r.source,
                        confidence: r.score.min(1.0),
                    };

                    session_manager
                        .record_suggestion(&req.session_id, resp.clone())
                        .await;

                    Response::Suggestion(resp)
                }
                None => Response::Suggestion(SuggestionResponse {
                    text: String::new(),
                    source: crate::protocol::SuggestionSource::History,
                    confidence: 0.0,
                }),
            };

            // Phase 2: Deferred — spawn AI provider with debounce
            if config.ai.enabled {
                let ap = ai_provider.clone();
                let sm = session_manager.clone();
                let cfg = config.clone();
                let rk = ranker.clone();
                let buffer_snapshot = req.buffer.clone();
                let session_id = req.session_id.clone();

                tokio::spawn(async move {
                    // Debounce: wait before calling AI
                    tokio::time::sleep(std::time::Duration::from_millis(cfg.general.debounce_ms)).await;

                    // Check if buffer has changed since we started
                    if let Some(current_buffer) = sm.get_last_buffer(&session_id).await {
                        if current_buffer != buffer_snapshot {
                            tracing::debug!("Buffer changed, skipping AI suggestion");
                            return;
                        }
                    }

                    // Call AI provider
                    if let Some(ai_suggestion) = ap.suggest(&req).await {
                        let ai_ranked = rk.rank(vec![ai_suggestion], &req.recent_commands);
                        if let Some(ai_r) = ai_ranked {
                            // Only push update if AI score beats current best
                            if ai_r.score > current_score {
                                let mut text = ai_r.text;
                                if text.len() > cfg.general.max_suggestion_length {
                                    text.truncate(cfg.general.max_suggestion_length);
                                }

                                let update = Response::Update(SuggestionResponse {
                                    text,
                                    source: ai_r.source,
                                    confidence: ai_r.score.min(1.0),
                                });

                                if let Ok(mut json) = serde_json::to_string(&update) {
                                    json.push('\n');
                                    let mut w = writer.lock().await;
                                    let _ = w.write_all(json.as_bytes()).await;
                                    let _ = w.flush().await;
                                }
                            }
                        }
                    }
                });
            }

            response
        }

        Request::Interaction(report) => {
            tracing::debug!(
                session = %report.session_id,
                action = ?report.action,
                "Interaction report"
            );

            interaction_logger.log_interaction(
                &report.session_id,
                report.action,
                &report.buffer_at_action,
                &report.suggestion,
                report.source,
                0.0,
                "",
            );

            Response::Ack
        }

        Request::Ping => {
            tracing::trace!("Ping");
            Response::Pong
        }

        Request::Shutdown => {
            tracing::info!("Shutdown requested");
            // Trigger graceful shutdown
            tokio::spawn(async {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                std::process::exit(0);
            });
            Response::Ack
        }

        Request::ReloadConfig => {
            tracing::info!("Config reload requested");
            // TODO: actually reload config
            Response::Ack
        }

        Request::ClearCache => {
            tracing::info!("Cache clear requested");
            // TODO: clear caches
            Response::Ack
        }
    }
}


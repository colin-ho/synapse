mod cache;
mod completion_context;
mod config;
mod logging;
mod project;
mod protocol;
mod providers;
mod ranking;
mod security;
mod session;
mod spec;
mod spec_autogen;
mod spec_store;
mod workflow;

use std::io::IsTerminal;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tracing_subscriber::EnvFilter;

use crate::config::Config;
use crate::logging::InteractionLogger;
use crate::protocol::{
    Request, Response, SuggestionListResponse, SuggestionResponse, SuggestionSource,
};
use crate::providers::ai::AiProvider;
use crate::providers::context::ContextProvider;
use crate::providers::environment::EnvironmentProvider;
use crate::providers::filesystem::FilesystemProvider;
use crate::providers::history::HistoryProvider;
use crate::providers::spec::SpecProvider;
use crate::providers::{Provider, ProviderRequest, ProviderSuggestion, SuggestionProvider};
use crate::ranking::Ranker;
use crate::security::Scrubber;
use crate::session::SessionManager;
use crate::spec_store::SpecStore;
use crate::workflow::WorkflowPredictor;

#[derive(Parser)]
#[command(name = "synapse", about = "Intelligent Zsh command suggestions")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Stop the daemon
    Stop {
        /// Override the socket path
        #[arg(long)]
        socket_path: Option<PathBuf>,
    },
    /// Show daemon status
    Status {
        /// Override the socket path
        #[arg(long)]
        socket_path: Option<PathBuf>,
    },
    /// Start the daemon (used internally by the shell plugin)
    #[command(hide = true)]
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

        /// Override the socket path (overrides SYNAPSE_SOCKET env var and config)
        #[arg(long)]
        socket_path: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Stop { socket_path }) => {
            stop_daemon(socket_path)?;
        }
        Some(Commands::Status { socket_path }) => {
            show_status(socket_path)?;
        }
        Some(Commands::Start {
            verbose,
            log_file,
            foreground,
            socket_path,
        }) => {
            start_daemon(verbose, log_file, foreground, socket_path).await?;
        }
        None => {
            if std::io::stdout().is_terminal() {
                setup_shell_rc("~/.zshrc")?;
            } else {
                print_init_code();
            }
        }
    }

    Ok(())
}

/// Check if the current binary is running from a Cargo target directory (dev mode).
/// Returns (exe_path, workspace_root) if detected.
fn detect_dev_mode() -> Option<(PathBuf, PathBuf)> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let profile_dir = exe.parent()?;
    let target_dir = profile_dir.parent()?;

    let profile = profile_dir.file_name()?.to_str()?;
    if !matches!(profile, "debug" | "release") {
        return None;
    }
    if target_dir.file_name()?.to_str()? != "target" {
        return None;
    }

    let workspace_root = target_dir.parent()?;
    if workspace_root.join("Cargo.toml").exists() {
        Some((exe.to_path_buf(), workspace_root.to_path_buf()))
    } else {
        None
    }
}

/// Produce an 8-char hex hash of a path for unique socket names.
/// Uses FNV-1a for deterministic output across Rust versions.
fn workspace_hash(path: &std::path::Path) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in path.to_string_lossy().as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{:08x}", (hash & 0xFFFF_FFFF) as u32)
}

/// Find the plugin file. In dev mode, uses workspace root; otherwise searches relative to binary.
fn find_plugin_path(exe: &std::path::Path, workspace_root: Option<&std::path::Path>) -> PathBuf {
    // Dev mode: workspace_root/plugin/synapse.zsh
    if let Some(root) = workspace_root {
        let p = root.join("plugin").join("synapse.zsh");
        if p.exists() {
            return p;
        }
    }

    // Relative to binary: ../plugin/ (installed layout)
    if let Some(parent) = exe.parent() {
        if let Some(grandparent) = parent.parent() {
            let p = grandparent.join("plugin").join("synapse.zsh");
            if p.exists() {
                return p;
            }
        }
        let p = parent.join("plugin").join("synapse.zsh");
        if p.exists() {
            return p;
        }
    }

    // Fallback
    PathBuf::from("plugin/synapse.zsh")
}

/// Output shell initialization code to stdout.
fn print_init_code() {
    if let Some((exe, workspace_root)) = detect_dev_mode() {
        print_dev_init_code(&exe, &workspace_root);
    } else {
        let exe = std::env::current_exe().unwrap_or_default();
        let exe = exe.canonicalize().unwrap_or(exe);
        print_normal_init_code(&exe);
    }
}

/// Output dev-mode shell initialization code.
fn print_dev_init_code(exe: &std::path::Path, workspace_root: &std::path::Path) {
    let plugin_path = find_plugin_path(exe, Some(workspace_root));
    let hash = workspace_hash(workspace_root);
    let socket_path = format!("/tmp/synapse-dev-{hash}.sock");
    let pid_path = format!("/tmp/synapse-dev-{hash}.pid");
    let log_path = format!("/tmp/synapse-dev-{hash}.log");
    let profile = exe
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    // Status on stderr (not captured by eval's $())
    eprintln!("synapse dev ({profile})");
    eprintln!("  workspace: {}", workspace_root.display());
    eprintln!("  socket:    {socket_path}");
    eprintln!("  logs:      tail -f {log_path}");

    print!(
        r#"# synapse dev mode
export SYNAPSE_BIN="{exe}"
export SYNAPSE_SOCKET="{socket}"
# Stop existing dev daemon on this socket
if [[ -f "{pid}" ]] && kill -0 $(<"{pid}") 2>/dev/null; then
    kill $(<"{pid}") 2>/dev/null
    command sleep 0.1
fi
command rm -f "{socket}" "{pid}"
# Start daemon with dev logging
"{exe}" start --foreground --socket-path "{socket}" --log-file "{log}" -vv &>/dev/null &
disown
_synapse_i=0
while [[ ! -S "{socket}" ]] && (( _synapse_i < 50 )); do command sleep 0.1; (( _synapse_i++ )); done
unset _synapse_i
source "{plugin}"
if [[ -S "{socket}" ]]; then
    echo "synapse dev: ready" >&2
else
    echo "synapse dev: daemon failed to start. check: tail -f {log}" >&2
fi
_synapse_dev_cleanup() {{
    if [[ -n "$SYNAPSE_SOCKET" ]]; then
        local pid_file="${{SYNAPSE_SOCKET%.sock}}.pid"
        if [[ -f "$pid_file" ]]; then
            local pid=$(<"$pid_file")
            [[ -n "$pid" ]] && kill "$pid" 2>/dev/null
            rm -f "$pid_file"
        fi
        rm -f "$SYNAPSE_SOCKET"
    fi
    unset SYNAPSE_SOCKET SYNAPSE_BIN
}}
if [[ -z "$_SYNAPSE_DEV_TRAP_SET" ]]; then
    _SYNAPSE_DEV_TRAP_SET=1
    trap '_synapse_dev_cleanup' EXIT
fi
"#,
        exe = exe.display(),
        socket = socket_path,
        pid = pid_path,
        log = log_path,
        plugin = plugin_path.display(),
    );
}

/// Output normal-mode shell initialization code.
fn print_normal_init_code(exe: &std::path::Path) {
    let plugin_path = find_plugin_path(exe, None);

    print!(
        r#"export SYNAPSE_BIN="{exe}"
source "{plugin}"
"#,
        exe = exe.display(),
        plugin = plugin_path.display(),
    );
}

/// Idempotently append the init line to a shell RC file.
fn setup_shell_rc(rc_file: &str) -> anyhow::Result<()> {
    let path = rc_file.replace('~', &dirs::home_dir().unwrap_or_default().to_string_lossy());
    let path = PathBuf::from(path);

    let init_line = r#"eval "$(synapse)""#;

    if path.exists() {
        let contents = std::fs::read_to_string(&path)?;
        if contents.contains(init_line) {
            eprintln!("synapse already present in {}", path.display());
            return Ok(());
        }
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(file)?;
    writeln!(file, "# Synapse — intelligent command suggestions")?;
    writeln!(file, "{init_line}")?;

    eprintln!("Added synapse to {}", path.display());
    eprintln!("Restart your shell or run: {init_line}");

    Ok(())
}

fn stop_daemon(socket_path: Option<PathBuf>) -> anyhow::Result<()> {
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

fn show_status(socket_path: Option<PathBuf>) -> anyhow::Result<()> {
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

async fn start_daemon(
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

    let context_provider = ContextProvider::new(config.context.clone());

    // Set up scrubber for external AI providers
    let scrubber = if config.ai.provider != "ollama" {
        Some(Arc::new(Scrubber::new(config.security.clone())))
    } else {
        None
    };
    let ai_provider = AiProvider::new(config.ai.clone(), scrubber);

    // Init spec system
    let spec_store = Arc::new(SpecStore::new(config.spec.clone()));
    let spec_provider = SpecProvider::new(spec_store.clone());

    // Init filesystem and environment providers
    let filesystem_provider = FilesystemProvider::new();
    let environment_provider = EnvironmentProvider::new();
    environment_provider.scan_path().await;

    let providers = Arc::new(vec![
        Provider::History(Arc::new(history_provider)),
        Provider::Context(Arc::new(context_provider)),
        Provider::Spec(Arc::new(spec_provider)),
        Provider::Filesystem(Arc::new(filesystem_provider)),
        Provider::Environment(Arc::new(environment_provider)),
        Provider::Ai(Arc::new(ai_provider)),
    ]);

    let ranker = Arc::new(Ranker::new(config.weights.clone()));
    let workflow_predictor = Arc::new(WorkflowPredictor::new());

    let session_manager = SessionManager::new();
    let interaction_logger = Arc::new(InteractionLogger::new(
        config.interaction_log_path(),
        config.logging.max_log_size_mb,
    ));

    let config = Arc::new(config);

    // Main loop
    let result = run_server(
        listener,
        providers,
        spec_store,
        ranker,
        workflow_predictor,
        session_manager,
        interaction_logger,
        config.clone(),
    )
    .await;

    // Cleanup
    tracing::info!("Shutting down");
    let _ = std::fs::remove_file(config.socket_path());
    let _ = std::fs::remove_file(config.pid_path());

    result
}

#[allow(clippy::too_many_arguments)]
async fn run_server(
    listener: UnixListener,
    providers: Arc<Vec<Provider>>,
    spec_store: Arc<SpecStore>,
    ranker: Arc<Ranker>,
    workflow_predictor: Arc<WorkflowPredictor>,
    session_manager: SessionManager,
    interaction_logger: Arc<InteractionLogger>,
    config: Arc<Config>,
) -> anyhow::Result<()> {
    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, _addr)) => {
                        let p = providers.clone();
                        let ss = spec_store.clone();
                        let rk = ranker.clone();
                        let wp = workflow_predictor.clone();
                        let sm = session_manager.clone();
                        let il = interaction_logger.clone();
                        let cfg = config.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(stream, p, ss, rk, wp, sm, il, cfg).await {
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

#[allow(clippy::too_many_arguments)]
async fn handle_connection(
    stream: tokio::net::UnixStream,
    providers: Arc<Vec<Provider>>,
    spec_store: Arc<SpecStore>,
    ranker: Arc<Ranker>,
    workflow_predictor: Arc<WorkflowPredictor>,
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
            Ok(request) => {
                handle_request(
                    request,
                    &providers,
                    &spec_store,
                    &ranker,
                    &workflow_predictor,
                    &session_manager,
                    &interaction_logger,
                    &config,
                    writer.clone(),
                )
                .await
            }
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

#[allow(clippy::too_many_arguments)]
async fn handle_request(
    request: Request,
    providers: &Arc<Vec<Provider>>,
    spec_store: &Arc<SpecStore>,
    ranker: &Arc<Ranker>,
    workflow_predictor: &Arc<WorkflowPredictor>,
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

            let provider_request =
                ProviderRequest::from_suggest_request(&req, spec_store.as_ref()).await;

            // Phase 1: Immediate — query all providers concurrently
            let suggestions =
                collect_provider_suggestions(providers, &provider_request, 1, false, None).await;

            // Rank immediate results
            let ranked = ranker.rank(
                suggestions,
                &provider_request.recent_commands,
                Some(provider_request.completion()),
            );

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
                let ai_provider = providers
                    .iter()
                    .find(|provider| provider.source() == SuggestionSource::Ai)
                    .cloned();
                let sm = session_manager.clone();
                let cfg = config.clone();
                let rk = ranker.clone();
                let provider_request = provider_request.clone();
                let buffer_snapshot = provider_request.buffer.clone();
                let session_id = provider_request.session_id.clone();

                if let Some(ai_provider) = ai_provider {
                    tokio::spawn(async move {
                        // Debounce: wait before calling AI
                        tokio::time::sleep(std::time::Duration::from_millis(
                            cfg.general.debounce_ms,
                        ))
                        .await;

                        // Check if buffer has changed since we started
                        if let Some(current_buffer) = sm.get_last_buffer(&session_id).await {
                            if current_buffer != buffer_snapshot {
                                tracing::debug!("Buffer changed, skipping AI suggestion");
                                return;
                            }
                        }

                        // Call AI provider
                        let ai_suggestions = ai_provider.suggest(&provider_request, 1).await;
                        let ai_ranked =
                            rk.rank(ai_suggestions, &provider_request.recent_commands, None);
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
                    });
                }
            }

            response
        }

        Request::ListSuggestions(req) => {
            tracing::debug!(
                session = %req.session_id,
                buffer = %req.buffer,
                max_results = req.max_results,
                "ListSuggestions request"
            );

            let max = req.max_results.min(config.spec.max_list_results);
            let provider_request =
                ProviderRequest::from_list_request(&req, spec_store.as_ref()).await;
            let all_suggestions = collect_provider_suggestions(
                providers,
                &provider_request,
                max,
                config.ai.enabled,
                Some(200),
            )
            .await;

            let ranked = ranker.rank_multi(
                all_suggestions,
                &provider_request.recent_commands,
                max,
                Some(provider_request.completion()),
            );

            let items = ranked.iter().map(|r| r.to_suggestion_item()).collect();

            Response::SuggestionList(SuggestionListResponse { suggestions: items })
        }

        Request::Interaction(report) => {
            tracing::debug!(
                session = %report.session_id,
                action = ?report.action,
                "Interaction report"
            );

            // Record workflow transition on Accept
            if report.action == crate::protocol::InteractionAction::Accept {
                if let Some(prev) = session_manager.get_last_accepted(&report.session_id).await {
                    workflow_predictor.record(&prev, &report.suggestion).await;
                }
                session_manager
                    .record_accepted(&report.session_id, report.suggestion.clone())
                    .await;
            }

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

async fn collect_provider_suggestions(
    providers: &[Provider],
    request: &ProviderRequest,
    max: usize,
    include_ai: bool,
    ai_timeout_ms: Option<u64>,
) -> Vec<ProviderSuggestion> {
    let mut task_set = tokio::task::JoinSet::new();

    for provider in providers {
        let source = provider.source();
        if !include_ai && source == SuggestionSource::Ai {
            continue;
        }

        let provider = provider.clone();
        let request = request.clone();
        task_set.spawn(async move {
            if source == SuggestionSource::Ai {
                if let Some(timeout_ms) = ai_timeout_ms {
                    tokio::time::timeout(
                        std::time::Duration::from_millis(timeout_ms),
                        provider.suggest(&request, max),
                    )
                    .await
                    .unwrap_or_default()
                } else {
                    provider.suggest(&request, max).await
                }
            } else {
                provider.suggest(&request, max).await
            }
        });
    }

    let mut all_suggestions = Vec::new();
    while let Some(result) = task_set.join_next().await {
        match result {
            Ok(mut suggestions) => all_suggestions.append(&mut suggestions),
            Err(error) => tracing::debug!("Provider task failed: {error}"),
        }
    }

    all_suggestions
}

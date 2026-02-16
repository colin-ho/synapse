use std::io::IsTerminal;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

mod handlers;
mod lifecycle;
mod server;
mod shell;
mod state;

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

pub async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Stop { socket_path }) => {
            lifecycle::stop_daemon(socket_path)?;
        }
        Some(Commands::Status { socket_path }) => {
            lifecycle::show_status(socket_path)?;
        }
        Some(Commands::Start {
            verbose,
            log_file,
            foreground,
            socket_path,
        }) => {
            lifecycle::start_daemon(verbose, log_file, foreground, socket_path).await?;
        }
        None => {
            if std::io::stdout().is_terminal() {
                shell::setup_shell_rc("~/.zshrc")?;
            } else {
                shell::print_init_code();
            }
        }
    }

    Ok(())
}

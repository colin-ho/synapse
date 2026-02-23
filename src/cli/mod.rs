use std::io::IsTerminal;
use std::path::PathBuf;

use clap::{CommandFactory, Parser, Subcommand};

mod add;
mod run_generator;
mod scan;
pub mod shell;
mod translate;

#[derive(Parser)]
#[command(name = "synapse", about = "Intelligent Zsh command suggestions")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Add synapse to your ~/.zshrc
    Install,
    /// Scan project files in cwd and write completion files (Makefile, package.json, etc.)
    Scan {
        /// Output directory (default: ~/.synapse/completions/)
        #[arg(long)]
        output_dir: Option<PathBuf>,

        /// Regenerate even if files already exist
        #[arg(long)]
        force: bool,

        /// Generate for all commands, even those with existing compsys functions
        #[arg(long)]
        no_gap_check: bool,
    },
    /// Run a generator command with timeout, split, and prefix stripping
    RunGenerator {
        /// Generator shell command to run
        command: String,

        /// Working directory
        #[arg(long)]
        cwd: Option<PathBuf>,

        /// Strip this prefix from each output line
        #[arg(long)]
        strip_prefix: Option<String>,

        /// Split output on this delimiter (default: newline)
        #[arg(long)]
        split_on: Option<String>,
    },
    /// Add completions for a command by running its --help or completion generator
    Add {
        /// Command name to add
        command: String,

        /// Output directory (default: ~/.synapse/completions/)
        #[arg(long)]
        output_dir: Option<PathBuf>,
    },
    /// Translate natural language to a shell command
    Translate {
        /// The natural language query
        query: String,

        /// Working directory
        #[arg(long)]
        cwd: PathBuf,

        /// Recent commands for context
        #[arg(long)]
        recent_command: Vec<String>,

        /// Environment hints (KEY=VAL)
        #[arg(long)]
        env_hint: Vec<String>,
    },
}

pub async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Add {
            command,
            output_dir,
        }) => {
            add::add_command(command, output_dir).await?;
        }
        Some(Commands::Install) => {
            shell::setup_shell_rc("~/.zshrc")?;
        }
        Some(Commands::Scan {
            output_dir,
            force,
            no_gap_check,
        }) => {
            scan::scan_project(output_dir, force, no_gap_check).await?;
        }
        Some(Commands::RunGenerator {
            command,
            cwd,
            strip_prefix,
            split_on,
        }) => {
            run_generator::run_generator(command, cwd, strip_prefix, split_on).await?;
        }
        Some(Commands::Translate {
            query,
            cwd,
            recent_command,
            env_hint,
        }) => {
            translate::translate(query, cwd, recent_command, env_hint).await?;
        }
        None => {
            if std::io::stdout().is_terminal() {
                Cli::command().print_help()?;
                println!();
            } else {
                shell::print_init_code()?;
            }
        }
    }

    Ok(())
}

use std::path::PathBuf;

use crate::config::Config;
use crate::spec_store::SpecStore;

use super::scan::resolve_completions_dir;

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

    match spec_store.discover_command(&command).await {
        Some((spec, path)) => {
            let n_opts = spec.options.len();
            let n_subs = spec.subcommands.len();
            println!("Discovered {command}: {n_opts} options, {n_subs} subcommands");
            println!("  Wrote {}", path.display());
        }
        None => {
            eprintln!("No spec discovered for '{command}' (--help produced no parseable output)");
            std::process::exit(1);
        }
    }

    Ok(())
}

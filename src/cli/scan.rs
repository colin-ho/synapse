use std::path::PathBuf;

use crate::config::Config;
use crate::spec_store::SpecStore;

pub(crate) fn resolve_completions_dir(config: &Config, output_dir: Option<PathBuf>) -> PathBuf {
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

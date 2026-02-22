use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use crate::spec::CommandSpec;

use super::{export_command_spec, GenerationReport};

pub(super) fn write_completion_file(spec: &CommandSpec, dir: &Path) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let filename = format!("_{}", spec.name);
    let path = dir.join(filename);
    let content = export_command_spec(spec);
    std::fs::write(&path, content)?;
    Ok(path)
}

pub(super) fn generate_all(
    specs: &[CommandSpec],
    existing_commands: &HashSet<String>,
    output_dir: &Path,
    gap_only: bool,
) -> io::Result<GenerationReport> {
    std::fs::create_dir_all(output_dir)?;
    let mut report = GenerationReport::default();

    for spec in specs {
        if spec.name.is_empty() {
            continue;
        }

        let is_project_auto = spec.source == crate::spec::SpecSource::ProjectAuto;
        if gap_only && !is_project_auto && existing_commands.contains(&spec.name) {
            report.skipped_existing.push(spec.name.clone());
            continue;
        }

        write_completion_file(spec, output_dir)?;
        report.generated.push(spec.name.clone());
    }

    Ok(report)
}

pub(super) fn remove_stale_project_auto(
    output_dir: &Path,
    generated_names: &HashSet<String>,
) -> io::Result<Vec<String>> {
    let mut removed = Vec::new();
    let entries = match std::fs::read_dir(output_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(removed),
        Err(error) => return Err(error),
    };

    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let name_str = file_name.to_string_lossy();

        if !name_str.starts_with('_') {
            continue;
        }

        let cmd_name = &name_str[1..];
        if generated_names.contains(cmd_name) {
            continue;
        }

        let content = match std::fs::read_to_string(entry.path()) {
            Ok(content) => content,
            Err(_) => continue,
        };

        let is_project_auto = content
            .lines()
            .take(5)
            .any(|line| line == "# Source: project-auto");

        if is_project_auto {
            std::fs::remove_file(entry.path())?;
            removed.push(cmd_name.to_string());
        }
    }

    Ok(removed)
}

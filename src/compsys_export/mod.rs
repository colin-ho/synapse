//! Convert `CommandSpec` into zsh `_arguments`-style completion functions.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use crate::spec::CommandSpec;

mod export;
mod filesystem;
mod format;

pub fn completions_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".synapse")
        .join("completions")
}

pub fn write_completion_file(spec: &CommandSpec, dir: &Path) -> io::Result<PathBuf> {
    filesystem::write_completion_file(spec, dir)
}

#[derive(Debug, Default)]
pub struct GenerationReport {
    pub generated: Vec<String>,
    pub skipped_existing: Vec<String>,
    pub removed: Vec<String>,
}

pub fn generate_all(
    specs: &[CommandSpec],
    existing_commands: &HashSet<String>,
    output_dir: &Path,
    gap_only: bool,
) -> io::Result<GenerationReport> {
    filesystem::generate_all(specs, existing_commands, output_dir, gap_only)
}

pub fn remove_stale_project_auto(
    output_dir: &Path,
    generated_names: &HashSet<String>,
) -> io::Result<Vec<String>> {
    filesystem::remove_stale_project_auto(output_dir, generated_names)
}

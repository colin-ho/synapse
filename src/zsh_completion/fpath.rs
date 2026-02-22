use std::collections::HashSet;
use std::path::PathBuf;

/// Literal directories to search for zsh completion files.
const FPATH_DIRS: &[&str] = &[
    "/usr/local/share/zsh/site-functions",
    "/opt/homebrew/share/zsh/site-functions",
];

/// Parent directory containing versioned zsh function dirs.
const ZSH_SHARE_DIR: &str = "/usr/share/zsh";

pub(super) fn resolve_fpath_dirs() -> Vec<PathBuf> {
    if let Ok(fpath) = std::env::var("FPATH") {
        if !fpath.is_empty() {
            return fpath
                .split(':')
                .filter(|entry| !entry.is_empty())
                .map(PathBuf::from)
                .collect();
        }
    }

    fallback_fpath_dirs()
}

fn fallback_fpath_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = FPATH_DIRS.iter().map(PathBuf::from).collect();
    if let Ok(entries) = std::fs::read_dir(ZSH_SHARE_DIR) {
        for entry in entries.flatten() {
            dirs.push(entry.path().join("functions"));
        }
    }
    dirs
}

pub(super) fn scan_available_commands() -> HashSet<String> {
    let mut commands = HashSet::new();

    for dir in resolve_fpath_dirs() {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if let Some(cmd) = name.strip_prefix('_') {
                    if !cmd.is_empty() && !cmd.contains('.') {
                        commands.insert(cmd.to_string());
                    }
                }
            }
        }
    }

    commands
}

pub(super) fn find_completion_file(command: &str) -> Option<PathBuf> {
    let target = format!("_{command}");

    for dir in resolve_fpath_dirs() {
        let candidate = dir.join(&target);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    None
}

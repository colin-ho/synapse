use anyhow::Context as _;
use std::io::ErrorKind;
use std::io::Write as _;
use std::path::PathBuf;

/// The plugin source, embedded at compile time.
const EMBEDDED_PLUGIN: &str = include_str!("../../plugin/synapse.zsh");

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

/// Find the plugin file. In dev mode, uses workspace root; otherwise searches relative to binary.
/// If no on-disk plugin is found, extracts the embedded plugin to a data directory.
fn find_plugin_path(
    exe: &std::path::Path,
    workspace_root: Option<&std::path::Path>,
) -> anyhow::Result<PathBuf> {
    // Dev mode: workspace_root/plugin/synapse.zsh
    if let Some(root) = workspace_root {
        let p = root.join("plugin").join("synapse.zsh");
        if p.exists() {
            return Ok(p);
        }
    }

    // Relative to binary: ../plugin/ (installed layout)
    if let Some(parent) = exe.parent() {
        if let Some(grandparent) = parent.parent() {
            let p = grandparent.join("plugin").join("synapse.zsh");
            if p.exists() {
                return Ok(p);
            }
        }
        let p = parent.join("plugin").join("synapse.zsh");
        if p.exists() {
            return Ok(p);
        }
    }

    // Fallback: extract embedded plugin to ~/.synapse/plugin/synapse.zsh
    extract_embedded_plugin().context("failed to extract embedded shell plugin")
}

/// Extract the embedded plugin to a well-known data directory and return the path.
fn extract_embedded_plugin() -> anyhow::Result<PathBuf> {
    let data_dir = dirs::home_dir().context("failed to determine home directory")?;
    extract_embedded_plugin_at(&data_dir)
}

fn extract_embedded_plugin_at(data_dir: &std::path::Path) -> anyhow::Result<PathBuf> {
    let plugin_path = data_dir.join(".synapse").join("plugin").join("synapse.zsh");

    // Write if missing or content has changed (e.g. after upgrade)
    let needs_write = match std::fs::read_to_string(&plugin_path) {
        Ok(existing) => existing != EMBEDDED_PLUGIN,
        Err(err) if err.kind() == ErrorKind::NotFound => true,
        Err(err) => {
            return Err(err)
                .with_context(|| format!("failed to read plugin at {}", plugin_path.display()));
        }
    };

    if needs_write {
        if let Some(parent) = plugin_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        std::fs::write(&plugin_path, EMBEDDED_PLUGIN)
            .with_context(|| format!("failed to write plugin at {}", plugin_path.display()))?;
    }

    if !plugin_path.is_file() {
        anyhow::bail!(
            "plugin path is not a regular file: {}",
            plugin_path.display()
        );
    }

    Ok(plugin_path)
}

/// Output shell initialization code to stdout.
pub fn print_init_code() -> anyhow::Result<()> {
    if let Some((exe, workspace_root)) = detect_dev_mode() {
        print_dev_init_code(&exe, &workspace_root)?;
    } else {
        let exe = std::env::current_exe().unwrap_or_default();
        let exe = exe.canonicalize().unwrap_or(exe);
        print_normal_init_code(&exe)?;
    }
    Ok(())
}

/// Output dev-mode shell initialization code.
fn print_dev_init_code(
    exe: &std::path::Path,
    workspace_root: &std::path::Path,
) -> anyhow::Result<()> {
    let plugin_path = find_plugin_path(exe, Some(workspace_root))?;
    let profile = exe
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    // Status on stderr (not captured by eval's $())
    eprintln!("synapse dev ({profile})");
    eprintln!("  workspace: {}", workspace_root.display());

    print!(
        r#"# synapse dev mode
export SYNAPSE_BIN="{exe}"
fpath=("$HOME/.synapse/completions" $fpath)
source "{plugin}"
echo "synapse dev: ready" >&2
"#,
        exe = exe.display(),
        plugin = plugin_path.display(),
    );
    Ok(())
}

/// Output normal-mode shell initialization code.
fn print_normal_init_code(exe: &std::path::Path) -> anyhow::Result<()> {
    let plugin_path = find_plugin_path(exe, None)?;

    // Update notification (cache read only, no network)
    if let Some(version) = super::update::cached_update_available() {
        eprintln!("synapse: update available ({version}). Run: synapse update");
    }

    print!(
        r#"export SYNAPSE_BIN="{exe}"
fpath=("$HOME/.synapse/completions" $fpath)
source "{plugin}"
(command "$SYNAPSE_BIN" update --check &>/dev/null &)
"#,
        exe = exe.display(),
        plugin = plugin_path.display(),
    );
    Ok(())
}

/// Idempotently add the init line to a shell RC file.
/// If `compinit` is found in the file, the init line is inserted before it
/// (synapse must add to fpath before compinit scans). Otherwise, appends.
pub fn setup_shell_rc(rc_file: &str) -> anyhow::Result<()> {
    let path = rc_file.replace('~', &dirs::home_dir().unwrap_or_default().to_string_lossy());
    let path = PathBuf::from(path);

    let init_line = r#"eval "$(synapse)""#;
    let init_block = format!("# Synapse — intelligent command suggestions\n{init_line}\n\n");

    if path.exists() {
        let contents = std::fs::read_to_string(&path)?;
        if contents.contains(init_line) {
            println!("synapse already present in {}", path.display());
            return Ok(());
        }

        // Try to insert before compinit so synapse's fpath additions are visible
        if let Some(pos) = find_compinit_line_start(&contents) {
            let mut new_contents = String::with_capacity(contents.len() + init_block.len());
            new_contents.push_str(&contents[..pos]);
            new_contents.push_str(&init_block);
            new_contents.push_str(&contents[pos..]);
            std::fs::write(&path, new_contents)?;
            println!("Added synapse to {} (before compinit)", path.display());
            println!("Restart your shell or run: {init_line}");
            return Ok(());
        }
    }

    // No compinit found or file doesn't exist — append
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(file)?;
    writeln!(file, "# Synapse — intelligent command suggestions")?;
    writeln!(file, "{init_line}")?;

    println!("Added synapse to {}", path.display());
    println!("Restart your shell or run: {init_line}");

    Ok(())
}

/// Find the byte offset of the start of the first non-commented line
/// containing `compinit`. Returns `None` if no such line exists.
fn find_compinit_line_start(contents: &str) -> Option<usize> {
    let mut offset = 0;
    for line in contents.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('#') && trimmed.contains("compinit") {
            return Some(offset);
        }
        // +1 for the newline character (lines() strips it)
        offset += line.len() + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_embedded_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let result = extract_embedded_plugin_at(dir.path());
        assert!(result.is_ok());
        let path = result.unwrap();
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, EMBEDDED_PLUGIN);
    }

    #[test]
    fn test_setup_shell_rc_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let rc = dir.path().join(".zshrc");
        std::fs::write(&rc, "# existing content\n").unwrap();

        setup_shell_rc(rc.to_str().unwrap()).unwrap();
        let after_first = std::fs::read_to_string(&rc).unwrap();
        assert!(after_first.contains(r#"eval "$(synapse)""#));

        setup_shell_rc(rc.to_str().unwrap()).unwrap();
        let after_second = std::fs::read_to_string(&rc).unwrap();
        assert_eq!(after_first, after_second);
    }

    #[test]
    fn test_setup_inserts_before_compinit() {
        let dir = tempfile::tempdir().unwrap();
        let rc = dir.path().join(".zshrc");
        std::fs::write(&rc, "autoload -Uz compinit\ncompinit\n").unwrap();

        setup_shell_rc(rc.to_str().unwrap()).unwrap();
        let content = std::fs::read_to_string(&rc).unwrap();
        let synapse_pos = content.find("synapse").unwrap();
        let compinit_pos = content.find("compinit").unwrap();
        assert!(synapse_pos < compinit_pos);
    }
}

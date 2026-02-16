use std::io::Write as _;
use std::path::PathBuf;

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
pub(super) fn print_init_code() {
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
pub(super) fn setup_shell_rc(rc_file: &str) -> anyhow::Result<()> {
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

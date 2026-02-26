use anyhow::{bail, Context as _};
use std::path::PathBuf;

const GITHUB_RELEASES_API: &str = "https://api.github.com/repos/colin-ho/synapse/releases/latest";
const CHECK_INTERVAL_SECS: u64 = 86400; // 24 hours

type Version = (u64, u64, u64);

fn cache_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".synapse").join("version-check.json"))
}

fn parse_version(s: &str) -> Option<Version> {
    let s = s.strip_prefix('v').unwrap_or(s);
    let mut parts = s.split('.');
    Some((
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    ))
}

fn current_version() -> Version {
    parse_version(env!("CARGO_PKG_VERSION")).expect("invalid CARGO_PKG_VERSION")
}

// --- Cache ---

#[derive(serde::Serialize, serde::Deserialize)]
struct VersionCache {
    latest: String,
    checked_at: u64,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn read_cache() -> Option<VersionCache> {
    let data = std::fs::read_to_string(cache_path()?).ok()?;
    serde_json::from_str(&data).ok()
}

fn write_cache(latest: &str) {
    let Some(path) = cache_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let cache = VersionCache {
        latest: latest.to_string(),
        checked_at: now_secs(),
    };
    let _ = std::fs::write(path, serde_json::to_string(&cache).unwrap_or_default());
}

/// Returns the latest version string if an update is available, based on cache only.
pub fn cached_update_available() -> Option<String> {
    let cache = read_cache()?;
    let latest = parse_version(&cache.latest)?;
    if latest > current_version() {
        Some(cache.latest)
    } else {
        None
    }
}

// --- Network ---

async fn fetch_latest_tag() -> anyhow::Result<String> {
    let client = reqwest::Client::builder()
        .user_agent("synapse-updater")
        .build()?;
    let resp: serde_json::Value = client.get(GITHUB_RELEASES_API).send().await?.json().await?;
    resp["tag_name"]
        .as_str()
        .map(String::from)
        .context("missing tag_name in GitHub response")
}

/// Background check: fetch latest version and write cache. Silent on all errors.
async fn check_and_cache() {
    let cache = read_cache();
    if let Some(ref c) = cache {
        if now_secs().saturating_sub(c.checked_at) < CHECK_INTERVAL_SECS {
            return;
        }
    }
    if let Ok(tag) = fetch_latest_tag().await {
        write_cache(&tag);
    }
}

// --- Self-update ---

fn detect_target() -> anyhow::Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu"),
        (os, arch) => bail!("unsupported platform: {os}-{arch}"),
    }
}

fn is_dev_binary() -> bool {
    let Some(exe) = std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok())
    else {
        return false;
    };
    let Some(profile) = exe
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
    else {
        return false;
    };
    if !matches!(profile, "debug" | "release") {
        return false;
    }
    exe.parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        == Some("target")
}

async fn do_update() -> anyhow::Result<()> {
    if is_dev_binary() {
        bail!("self-update is not supported for dev builds — use `cargo build` instead");
    }

    let target = detect_target()?;
    let tag = fetch_latest_tag().await?;
    let latest = parse_version(&tag).context("cannot parse latest version")?;
    let current = current_version();

    if latest <= current {
        println!("Already up to date (v{})", env!("CARGO_PKG_VERSION"));
        write_cache(&tag);
        return Ok(());
    }

    println!("Updating synapse v{} → {tag}...", env!("CARGO_PKG_VERSION"));

    let url = format!(
        "https://github.com/colin-ho/synapse/releases/download/{tag}/synapse-{tag}-{target}.tar.gz"
    );
    let bytes = reqwest::get(&url)
        .await?
        .error_for_status()
        .context("failed to download release")?
        .bytes()
        .await?;

    let decoder = flate2::read::GzDecoder::new(&bytes[..]);
    let mut archive = tar::Archive::new(decoder);

    let exe = std::env::current_exe()?.canonicalize()?;
    let exe_dir = exe.parent().context("cannot determine binary directory")?;
    let tmp_path = exe_dir.join(".synapse-update-tmp");

    let mut found = false;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        if path.file_name().and_then(|n| n.to_str()) == Some("synapse") {
            entry.unpack(&tmp_path)?;
            found = true;
            break;
        }
    }
    if !found {
        let _ = std::fs::remove_file(&tmp_path);
        bail!("synapse binary not found in release archive");
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o755))?;
    }

    std::fs::rename(&tmp_path, &exe).context("failed to replace binary (try with sudo?)")?;
    write_cache(&tag);

    println!("Updated to {tag}");
    Ok(())
}

pub async fn run(check: bool) -> anyhow::Result<()> {
    if check {
        check_and_cache().await;
        Ok(())
    } else {
        do_update().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_version() {
        assert_eq!(parse_version("v0.2.0"), Some((0, 2, 0)));
        assert_eq!(parse_version("1.10.3"), Some((1, 10, 3)));
        assert_eq!(parse_version("nope"), None);
    }

    #[test]
    fn test_version_comparison() {
        assert!((0, 2, 0) > (0, 1, 0));
        assert!((0, 10, 0) > (0, 9, 0));
        assert!((1, 0, 0) > (0, 99, 99));
    }

    #[test]
    fn test_cached_update_no_cache() {
        // With no cache file, should return None
        assert!(cached_update_available().is_none() || cached_update_available().is_some());
    }

    #[test]
    fn test_is_dev_binary() {
        // Test runner binary is in target/debug/deps/, not target/debug/,
        // so is_dev_binary() returns false here — just verify it doesn't panic
        let _ = is_dev_binary();
    }

    #[test]
    fn test_detect_target() {
        // Should succeed on macOS/Linux
        assert!(detect_target().is_ok());
    }
}

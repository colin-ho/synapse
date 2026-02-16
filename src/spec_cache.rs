use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::spec::{CommandSpec, SpecSource};

/// A discovered spec with metadata for staleness detection.
#[derive(Debug, Serialize, Deserialize)]
pub struct DiscoveredSpec {
    /// ISO 8601 timestamp of when the spec was discovered
    #[serde(default)]
    pub discovered_at: Option<String>,
    /// Resolved path of the binary (e.g., "/usr/bin/rg")
    #[serde(default)]
    pub command_path: Option<String>,
    /// Output of `command --version` for staleness detection
    #[serde(default)]
    pub version_output: Option<String>,
    /// The actual spec
    #[serde(flatten)]
    pub spec: CommandSpec,
}

/// Returns the directory where discovered specs are stored.
pub fn specs_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("synapse")
        .join("specs")
}

/// Load all discovered specs from disk.
pub fn load_all_discovered() -> HashMap<String, CommandSpec> {
    let dir = specs_dir();
    let mut specs = HashMap::new();

    if !dir.is_dir() {
        return specs;
    }

    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("Failed to read discovered specs dir: {e}");
            return specs;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "toml") {
            match load_spec_file(&path) {
                Ok(discovered) => {
                    let mut spec = discovered.spec;
                    spec.source = SpecSource::Discovered;
                    specs.insert(spec.name.clone(), spec);
                }
                Err(e) => {
                    tracing::debug!("Failed to load discovered spec {}: {e}", path.display());
                }
            }
        }
    }

    specs
}

/// Load a single discovered spec from disk.
fn load_spec_file(path: &Path) -> anyhow::Result<DiscoveredSpec> {
    let content = std::fs::read_to_string(path)?;
    let spec: DiscoveredSpec = toml::from_str(&content)?;
    Ok(spec)
}

/// Save a discovered spec to disk.
pub fn save_discovered(spec: &DiscoveredSpec) -> std::io::Result<()> {
    let dir = specs_dir();
    std::fs::create_dir_all(&dir)?;

    let path = dir.join(format!("{}.toml", spec.spec.name));

    let toml_str = toml::to_string_pretty(spec)
        .map_err(|e| std::io::Error::other(format!("TOML serialize error: {e}")))?;

    std::fs::write(&path, toml_str)?;
    tracing::debug!(
        "Saved discovered spec for {} to {}",
        spec.spec.name,
        path.display()
    );
    Ok(())
}

/// Check if a discovered spec is stale and should be re-discovered.
pub fn is_stale(spec: &DiscoveredSpec, max_age_secs: u64) -> bool {
    let Some(ref discovered_at) = spec.discovered_at else {
        return true;
    };

    let Ok(timestamp) = chrono::DateTime::parse_from_rfc3339(discovered_at) else {
        return true;
    };

    let age = chrono::Utc::now().signed_duration_since(timestamp);
    age.num_seconds() as u64 > max_age_secs
}

/// Remove a discovered spec from disk.
#[allow(dead_code)]
pub fn remove_discovered(command: &str) -> std::io::Result<()> {
    let path = specs_dir().join(format!("{command}.toml"));
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_roundtrip() {
        let spec = DiscoveredSpec {
            discovered_at: Some("2026-02-15T10:30:00Z".to_string()),
            command_path: Some("/usr/bin/rg".to_string()),
            version_output: Some("ripgrep 14.1.0".to_string()),
            spec: CommandSpec {
                name: "rg".to_string(),
                description: Some("Fast grep".to_string()),
                ..Default::default()
            },
        };

        let toml_str = toml::to_string_pretty(&spec).unwrap();
        let roundtrip: DiscoveredSpec = toml::from_str(&toml_str).unwrap();

        assert_eq!(roundtrip.spec.name, "rg");
        assert_eq!(roundtrip.discovered_at, spec.discovered_at);
        assert_eq!(roundtrip.command_path, spec.command_path);
        assert_eq!(roundtrip.version_output, spec.version_output);
    }

    #[test]
    fn test_staleness_fresh() {
        let spec = DiscoveredSpec {
            discovered_at: Some(chrono::Utc::now().to_rfc3339()),
            command_path: None,
            version_output: None,
            spec: CommandSpec {
                name: "test".to_string(),
                ..Default::default()
            },
        };

        assert!(!is_stale(&spec, 604800)); // 7 days
    }

    #[test]
    fn test_staleness_old() {
        let old = chrono::Utc::now() - chrono::Duration::days(10);
        let spec = DiscoveredSpec {
            discovered_at: Some(old.to_rfc3339()),
            command_path: None,
            version_output: None,
            spec: CommandSpec {
                name: "test".to_string(),
                ..Default::default()
            },
        };

        assert!(is_stale(&spec, 604800)); // 7 days
    }

    #[test]
    fn test_staleness_missing_timestamp() {
        let spec = DiscoveredSpec {
            discovered_at: None,
            command_path: None,
            version_output: None,
            spec: CommandSpec {
                name: "test".to_string(),
                ..Default::default()
            },
        };

        assert!(is_stale(&spec, 604800));
    }

    #[test]
    fn test_save_and_load() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test.toml");

        let spec = DiscoveredSpec {
            discovered_at: Some("2026-02-15T10:30:00Z".to_string()),
            command_path: Some("/usr/bin/test".to_string()),
            version_output: None,
            spec: CommandSpec {
                name: "test".to_string(),
                description: Some("A test tool".to_string()),
                ..Default::default()
            },
        };

        let toml_str = toml::to_string_pretty(&spec).unwrap();
        std::fs::write(&path, &toml_str).unwrap();

        let loaded = load_spec_file(&path).unwrap();
        assert_eq!(loaded.spec.name, "test");
        assert_eq!(loaded.spec.description.as_deref(), Some("A test tool"));
    }
}

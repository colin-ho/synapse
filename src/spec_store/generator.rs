use std::path::Path;
use std::time::Duration;

use tokio::process::Command;

use crate::spec::GeneratorSpec;

use super::{GeneratorCacheEntry, SpecStore};

pub(super) async fn run_generator(
    store: &SpecStore,
    generator: &GeneratorSpec,
    cwd: &Path,
) -> Vec<String> {
    let cache_key = (generator.command.clone(), cwd.to_path_buf());

    if let Some(cached) = store.generator_cache.get(&cache_key).await {
        return cached.items;
    }

    let timeout = Duration::from_millis(
        generator
            .timeout_ms
            .min(crate::config::GENERATOR_TIMEOUT_MS),
    );

    let result = match tokio::time::timeout(timeout, async {
        Command::new("sh")
            .arg("-c")
            .arg(&generator.command)
            .current_dir(cwd)
            .output()
            .await
    })
    .await
    {
        Ok(Ok(output)) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let split_on = &generator.split_on;
            let items: Vec<String> = stdout
                .split(split_on.as_str())
                .filter_map(|line| {
                    let mut item = line.trim().to_string();
                    if item.is_empty() {
                        return None;
                    }
                    if let Some(prefix) = &generator.strip_prefix {
                        if let Some(stripped) = item.strip_prefix(prefix.as_str()) {
                            item = stripped.to_string();
                        }
                    }
                    if item.is_empty() {
                        None
                    } else {
                        Some(item)
                    }
                })
                .collect();
            items
        }
        Ok(Ok(output)) => {
            tracing::debug!(
                "Generator command failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            Vec::new()
        }
        Ok(Err(err)) => {
            tracing::debug!("Generator command error: {err}");
            Vec::new()
        }
        Err(_) => {
            tracing::debug!("Generator command timed out: {}", generator.command);
            Vec::new()
        }
    };

    let ttl = Duration::from_secs(generator.cache_ttl_secs);
    let entry = GeneratorCacheEntry {
        items: result.clone(),
        ttl,
    };
    store.generator_cache.insert(cache_key, entry).await;

    result
}

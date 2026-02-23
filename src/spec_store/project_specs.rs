use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use crate::spec::{CommandSpec, SpecSource};
use crate::spec_autogen;

use super::SpecStore;

impl SpecStore {
    /// Look up a spec by command name.
    /// Checks project specs first, then the in-memory discovered spec cache.
    pub async fn lookup(&self, command: &str, cwd: &Path) -> Option<CommandSpec> {
        let project_specs = self.get_project_specs(cwd).await;
        if let Some(spec) = project_specs.get(command) {
            return Some(spec.clone());
        }
        self.discovered_cache.get(command).await
    }

    /// Return all project specs for the given cwd as a Vec (for compsys export).
    pub async fn lookup_all_project_specs(&self, cwd: &Path) -> Vec<CommandSpec> {
        let project_specs = self.get_project_specs(cwd).await;
        project_specs.values().cloned().collect()
    }

    /// Get all available command names for a given cwd.
    pub async fn all_command_names(&self, cwd: &Path) -> Vec<String> {
        let mut seen: HashSet<String> = self
            .zsh_index
            .read()
            .map(|idx| idx.clone())
            .unwrap_or_default();

        let project_specs = self.get_project_specs(cwd).await;
        for key in project_specs.keys() {
            seen.insert(key.clone());
        }

        seen.into_iter().collect()
    }

    /// Get project-specific specs (user-defined + auto-generated), cached.
    pub async fn get_project_specs(&self, cwd: &Path) -> Arc<HashMap<String, CommandSpec>> {
        if !self.config.enabled {
            return Arc::new(HashMap::new());
        }

        let key = cwd.to_path_buf();
        if let Some(cached) = self.project_cache.get(&key).await {
            return cached;
        }

        let auto_generate = self.config.auto_generate;
        let cwd_owned = cwd.to_path_buf();

        let mut specs = tokio::task::spawn_blocking(move || {
            let mut specs = HashMap::new();

            if auto_generate {
                let auto_specs = spec_autogen::generate_specs(&cwd_owned);
                for mut spec in auto_specs {
                    if !specs.contains_key(&spec.name) {
                        spec.source = SpecSource::ProjectAuto;
                        specs.insert(spec.name.clone(), spec);
                    }
                }
            }

            specs
        })
        .await
        .unwrap_or_default();

        let project_root = crate::project::find_project_root(cwd, self.config.scan_depth);
        let scan_root = project_root.as_deref().unwrap_or(cwd);

        if self.config.discover_project_cli && self.config.trust_project_generators {
            let cli_specs = spec_autogen::discover_project_cli_specs(
                scan_root,
                crate::config::DISCOVER_TIMEOUT_MS,
            )
            .await;
            for mut spec in cli_specs {
                if !specs.contains_key(&spec.name) {
                    spec.source = SpecSource::ProjectAuto;
                    specs.insert(spec.name.clone(), spec);
                }
            }
        }

        let specs = Arc::new(specs);
        self.project_cache.insert(key, specs.clone()).await;
        specs
    }
}

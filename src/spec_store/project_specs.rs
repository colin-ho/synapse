use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use crate::spec::{CommandSpec, SpecSource};
use crate::spec_autogen;

use super::SpecStore;

impl SpecStore {
    /// Look up a spec by command name from project specs.
    pub async fn lookup(&self, command: &str, cwd: &Path) -> Option<CommandSpec> {
        let project_specs = self.get_project_specs(cwd).await;
        project_specs.get(command).cloned()
    }

    /// Return all project specs for the given cwd as a Vec (for compsys export).
    pub async fn lookup_all_project_specs(&self, cwd: &Path) -> Vec<CommandSpec> {
        let project_specs = self.get_project_specs(cwd).await;
        project_specs.values().cloned().collect()
    }

    /// Get all available command names for a given cwd.
    pub async fn all_command_names(&self, cwd: &Path) -> Vec<String> {
        let mut seen: HashSet<String> = self.zsh_index.clone();

        let project_specs = self.get_project_specs(cwd).await;
        for key in project_specs.keys() {
            seen.insert(key.clone());
        }

        seen.into_iter().collect()
    }

    /// Get project-specific auto-generated specs, computed once per process.
    pub async fn get_project_specs(&self, cwd: &Path) -> Arc<HashMap<String, CommandSpec>> {
        self.project_specs
            .get_or_init(|| async {
                if !self.config.enabled || !self.config.auto_generate {
                    return Arc::new(HashMap::new());
                }

                let cwd_owned = cwd.to_path_buf();
                let specs = tokio::task::spawn_blocking(move || {
                    let mut specs = HashMap::new();
                    for mut spec in spec_autogen::generate_specs(&cwd_owned) {
                        spec.source = SpecSource::ProjectAuto;
                        specs.insert(spec.name.clone(), spec);
                    }
                    specs
                })
                .await
                .unwrap_or_default();

                Arc::new(specs)
            })
            .await
            .clone()
    }
}

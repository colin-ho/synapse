//! Parse zsh completion files (`_arguments` specs) into `CommandSpec`.

use std::collections::HashSet;

use crate::spec::CommandSpec;

mod fpath;
mod generator;
mod parser;

pub fn scan_available_commands() -> HashSet<String> {
    fpath::scan_available_commands()
}

pub async fn try_completion_generator(
    command: &str,
    timeout: std::time::Duration,
) -> Option<CommandSpec> {
    generator::try_completion_generator(command, timeout).await
}

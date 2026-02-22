//! Parse zsh completion files (`_arguments` specs) into `CommandSpec`.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::spec::CommandSpec;

mod fpath;
mod generator;
mod parser;

pub fn scan_available_commands() -> HashSet<String> {
    fpath::scan_available_commands()
}

pub fn find_completion_file(command: &str) -> Option<PathBuf> {
    fpath::find_completion_file(command)
}

pub fn find_and_parse(command: &str) -> Option<CommandSpec> {
    let path = find_completion_file(command)?;
    let content = std::fs::read_to_string(&path).ok()?;
    let spec = parse_zsh_completion(command, &content);
    if spec.options.is_empty() && spec.subcommands.is_empty() {
        return None;
    }
    Some(spec)
}

pub fn parse_zsh_completion(command: &str, content: &str) -> CommandSpec {
    parser::parse_zsh_completion(command, content)
}

pub async fn try_completion_generator(
    command: &str,
    timeout: std::time::Duration,
) -> Option<CommandSpec> {
    generator::try_completion_generator(command, timeout).await
}

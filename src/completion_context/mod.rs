mod builder;
mod tokenizer;

pub use tokenizer::{split_at_last_operator, tokenize, tokenize_with_operators, Token};

/// Where we are in the command being typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Position {
    /// Completing the command name itself (`gi▏` → `git`)
    CommandName,
    /// Completing a subcommand (`git ch▏` → `checkout`)
    Subcommand,
    /// Completing an option flag (`git commit --am▏` → `--amend`)
    OptionFlag,
    /// Completing the value for a specific option (`git checkout -b ▏`)
    OptionValue { option: String },
    /// Completing a positional argument (`cd ▏`)
    Argument { index: usize },
    /// Completing a command after a pipe (`cat foo | ▏`)
    PipeTarget,
    /// Completing a file path after a redirect (`echo hello > ▏`)
    Redirect,
    /// Cannot determine position
    Unknown,
}

/// What kind of value is expected at the current position.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ExpectedType {
    Any,
    FilePath,
    Directory,
    Executable,
    Generator(String),
    OneOf(Vec<String>),
    Hostname,
    EnvVar,
    Command,
}

/// Parsed understanding of the current command buffer.
#[derive(Debug, Clone)]
pub struct CompletionContext {
    /// The raw buffer string
    pub buffer: String,
    /// Tokenized buffer (words only, no operators)
    pub tokens: Vec<String>,
    /// Whether the buffer ends with a space
    #[allow(dead_code)]
    pub trailing_space: bool,
    /// The partial word being typed (last token if no trailing space, empty if trailing space)
    pub partial: String,
    /// Everything before the partial — prepend to completions
    pub prefix: String,
    /// The command name (first token of the last segment)
    pub command: Option<String>,
    /// Where we are in the command
    pub position: Position,
    /// What type of value is expected
    pub expected_type: ExpectedType,
    /// The subcommand path walked (e.g. ["checkout"] for `git checkout`)
    pub subcommand_path: Vec<String>,
    /// Options already present in the buffer
    #[allow(dead_code)]
    pub present_options: Vec<String>,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{CompletionContext, ExpectedType, Position};
    use crate::config::SpecConfig;
    use crate::spec_store::SpecStore;

    fn make_store() -> Arc<SpecStore> {
        Arc::new(SpecStore::new(SpecConfig::default(), None))
    }

    #[tokio::test]
    async fn test_empty_buffer() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompletionContext::build("", dir.path(), &store).await;
        assert_eq!(ctx.position, Position::CommandName);
        assert!(ctx.tokens.is_empty());
    }

    #[tokio::test]
    async fn test_command_name_partial() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompletionContext::build("gi", dir.path(), &store).await;
        assert_eq!(ctx.position, Position::CommandName);
        assert_eq!(ctx.partial, "gi");
        assert_eq!(ctx.prefix, "");
        assert_eq!(ctx.command, Some("gi".into()));
    }

    #[tokio::test]
    async fn test_git_subcommand() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompletionContext::build("git ch", dir.path(), &store).await;
        assert_eq!(ctx.position, Position::Subcommand);
        assert_eq!(ctx.partial, "ch");
        assert_eq!(ctx.prefix, "git ");
        assert_eq!(ctx.command, Some("git".into()));
    }

    #[tokio::test]
    async fn test_git_subcommand_trailing_space() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompletionContext::build("git ", dir.path(), &store).await;
        assert_eq!(ctx.position, Position::Subcommand);
        assert_eq!(ctx.partial, "");
        assert_eq!(ctx.prefix, "git ");
    }

    #[tokio::test]
    async fn test_git_checkout_argument() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompletionContext::build("git checkout ", dir.path(), &store).await;
        assert_eq!(ctx.position, Position::Argument { index: 0 });
        assert_eq!(ctx.subcommand_path, vec!["checkout"]);
        assert_eq!(ctx.command, Some("git".into()));
        // git checkout has a generator for branches
        assert!(matches!(ctx.expected_type, ExpectedType::Generator(_)));
    }

    #[tokio::test]
    async fn test_git_commit_option() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompletionContext::build("git commit --am", dir.path(), &store).await;
        assert_eq!(ctx.position, Position::OptionFlag);
        assert_eq!(ctx.partial, "--am");
        assert_eq!(ctx.subcommand_path, vec!["commit"]);
    }

    #[tokio::test]
    async fn test_option_value_while_typing() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompletionContext::build("git checkout -b fe", dir.path(), &store).await;
        assert_eq!(
            ctx.position,
            Position::OptionValue {
                option: "-b".into()
            }
        );
        assert_eq!(ctx.partial, "fe");
        assert_eq!(ctx.prefix, "git checkout -b ");
    }

    #[tokio::test]
    async fn test_cd_directory() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompletionContext::build("cd ", dir.path(), &store).await;
        assert_eq!(ctx.position, Position::Argument { index: 0 });
        assert_eq!(ctx.expected_type, ExpectedType::Directory);
        assert_eq!(ctx.command, Some("cd".into()));
    }

    #[tokio::test]
    async fn test_cat_filepath() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompletionContext::build("cat ", dir.path(), &store).await;
        assert_eq!(ctx.position, Position::Argument { index: 0 });
        assert_eq!(ctx.expected_type, ExpectedType::FilePath);
    }

    #[tokio::test]
    async fn test_ssh_argument() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompletionContext::build("ssh ", dir.path(), &store).await;
        assert_eq!(ctx.position, Position::Argument { index: 0 });
        // ssh destination arg has a generator for ~/.ssh/config hosts
        assert!(matches!(ctx.expected_type, ExpectedType::Generator(_)));
    }

    #[tokio::test]
    async fn test_unknown_command() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompletionContext::build("unknown_cmd ", dir.path(), &store).await;
        assert_eq!(ctx.position, Position::Unknown);
        assert_eq!(ctx.expected_type, ExpectedType::Any);
    }

    #[tokio::test]
    async fn test_sudo_recursive() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompletionContext::build("sudo git ch", dir.path(), &store).await;
        // Should resolve to git subcommand completion
        assert_eq!(ctx.position, Position::Subcommand);
        assert_eq!(ctx.partial, "ch");
        assert_eq!(ctx.command, Some("git".into()));
        // Prefix should include "sudo "
        assert!(ctx.prefix.starts_with("sudo "));
    }

    #[tokio::test]
    async fn test_sudo_command_name() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompletionContext::build("sudo gi", dir.path(), &store).await;
        assert_eq!(ctx.position, Position::CommandName);
        assert_eq!(ctx.partial, "gi");
    }

    #[tokio::test]
    async fn test_present_options_tracked() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompletionContext::build("git commit --verbose --am", dir.path(), &store).await;
        assert_eq!(ctx.position, Position::OptionFlag);
        assert!(ctx.present_options.contains(&"--verbose".to_string()));
    }

    #[tokio::test]
    async fn test_cargo_subcommand() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompletionContext::build("cargo b", dir.path(), &store).await;
        assert_eq!(ctx.position, Position::Subcommand);
        assert_eq!(ctx.partial, "b");
        assert_eq!(ctx.command, Some("cargo".into()));
    }

    #[tokio::test]
    async fn test_export_envvar() {
        let store = make_store();
        let dir = tempfile::tempdir().unwrap();
        let ctx = CompletionContext::build("export ", dir.path(), &store).await;
        assert_eq!(ctx.position, Position::Argument { index: 0 });
        assert_eq!(ctx.expected_type, ExpectedType::EnvVar);
    }
}

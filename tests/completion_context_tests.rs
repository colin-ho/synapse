use std::sync::Arc;

use synapse::completion_context::{CompletionContext, ExpectedType, Position};
use synapse::config::SpecConfig;
use synapse::spec_store::SpecStore;

fn make_store() -> Arc<SpecStore> {
    Arc::new(SpecStore::new(SpecConfig::default()))
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
    // ssh has a builtin spec now, so expected type comes from the spec (Any)
    assert_eq!(ctx.expected_type, ExpectedType::Any);
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

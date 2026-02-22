mod client;
mod prompt;
mod response;
mod scrub;

pub use client::{LlmClient, LlmError};
pub use prompt::{build_nl_prompt, NlTranslationContext, NlTranslationItem, NlTranslationResult};
pub use response::{detect_destructive_command, extract_commands};
pub use scrub::scrub_env_values;

pub(crate) use scrub::scrub_home_paths;

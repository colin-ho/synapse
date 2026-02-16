# LLM-Powered Workflow Prediction

## Overview

Enhance the existing workflow prediction system with LLM intelligence to predict multi-step command sequences and generate contextually appropriate arguments. The bigram predictor (`src/workflow.rs`) already collects transition data but its `predict()` method is dead code. This design activates it, integrates it into the suggestion pipeline, and layers LLM predictions on top for novel sequences and argument generation.

## Problem

The current system treats each suggestion independently. It has no concept of "the user just ran `git add .` and is now likely to run `git commit`." The infrastructure exists:

- `WorkflowPredictor` records command bigrams in `~/.local/share/synapse/workflows.json`
- `predict()` returns top predictions with probabilities
- `InteractionLogger` records accept/dismiss/ignore events with timestamps
- `recent_commands` is sent in every request
- `last_exit_code` is sent but marked `#[allow(dead_code)]`

None of this data feeds into the suggestion pipeline. The ranking system's recency bonus only checks prefix overlap with recent commands — it doesn't predict what comes next.

## Design

### Phase 1: Activate the Bigram Predictor (No LLM)

Wire the existing `WorkflowPredictor::predict()` into the suggestion pipeline:

**New Provider: WorkflowProvider**

```rust
pub struct WorkflowProvider {
    predictor: Arc<WorkflowPredictor>,
}

impl SuggestionProvider for WorkflowProvider {
    async fn suggest(&self, request: &ProviderRequest, max: NonZeroUsize) -> Vec<ProviderSuggestion> {
        // Only activate when buffer is empty or very short (< 5 chars)
        if request.buffer.len() > 4 {
            return Vec::new();
        }

        let previous = request.recent_commands.first()?;
        let predictions = self.predictor.predict(previous, max.get()).await;

        predictions.into_iter()
            .filter(|(cmd, prob)| *prob > 0.1 && cmd.starts_with(&request.buffer))
            .map(|(cmd, prob)| ProviderSuggestion {
                text: cmd,
                source: SuggestionSource::History,  // or new Workflow source
                score: prob,
                description: Some("predicted next command".into()),
                kind: SuggestionKind::Command,
            })
            .collect()
    }
}
```

**Integration point:** Add to the provider list in `RuntimeState` construction. The workflow provider only activates at `Position::CommandName` when the buffer is empty or very short, so it adds negligible overhead.

**Ranking:** Add a `Workflow` source weight to the position weight tables. At `CommandName` position with an empty buffer, workflow predictions should dominate:

| Position | Workflow weight |
|---|---|
| CommandName (empty buffer) | 0.50 |
| CommandName (partial) | 0.15 |
| All other positions | 0.0 |

### Phase 2: LLM-Powered Workflow Prediction

Layer LLM intelligence on top of the bigram predictor for two cases:

1. **Novel sequences** — when the bigram predictor has no data (new project, uncommon workflow)
2. **Argument generation** — the bigram predictor knows the next *command* but not its *arguments*

#### LLM Workflow Prompt

When the buffer is empty and the bigram predictor has no strong prediction (probability < 0.3), query the LLM:

```
You are a terminal workflow predictor. Given the user's recent commands and context, predict the single most likely next command they will type.

Working directory: {cwd}
Project type: {detected_project_type}  (e.g., "Rust/Cargo", "Node.js/npm", "Python/poetry")
Recent commands (most recent first):
1. {recent_commands[0]}
2. {recent_commands[1]}
3. {recent_commands[2]}
Last exit code: {last_exit_code}

Respond with ONLY the complete command (no explanation).
```

#### Exit Code Awareness

The `last_exit_code` field is already sent in every request but unused. This design activates it:

- `exit_code == 0` → workflow continues normally (predict next step)
- `exit_code != 0` → the previous command failed. Predict a recovery action:
  - After `cargo build` failure → suggest `cargo build` again (retry) or infer the failing file
  - After `git push` rejection → suggest `git pull --rebase`
  - After `npm test` failure → suggest re-running or editing the test file

The LLM naturally handles exit-code-aware prediction because the prompt includes the code. The bigram predictor can be extended with exit-code-tagged transitions: `(prev_command, exit_code) → next_command`.

#### Argument Enrichment

The biggest value of LLM workflow prediction is generating contextually appropriate arguments. Examples:

| After | Bigram predicts | LLM predicts |
|---|---|---|
| `git add .` | `git commit` | `git commit -m "<message from staged diff>"` |
| `mkdir new-project` | `cd` | `cd new-project` |
| `git checkout -b feat/auth` | `git push` | `git push -u origin feat/auth` |
| `docker build -t myapp .` | `docker run` | `docker run -p 8080:8080 myapp` |

For commit messages specifically, the LLM can inspect `git diff --staged` output (truncated to ~2000 tokens) to generate a contextual message.

#### Async Delivery

LLM workflow predictions run as Phase 2 (async), same as the existing update mechanism:

1. Phase 1: Return the bigram prediction immediately (or empty if none)
2. Phase 2: Fire an LLM request in the background
3. If the LLM returns a better prediction before the user types more, push an `Update` response

This ensures the fast path remains <20ms. The LLM prediction typically arrives in 200-500ms, before the user starts typing.

### Data Collection Improvements

Extend `WorkflowPredictor` to capture richer signals:

```rust
struct WorkflowData {
    // Existing: command → command bigrams
    bigrams: HashMap<String, Vec<(String, u32)>>,

    // New: exit-code-aware transitions
    // (prev_command, exit_code_bucket) → next_command
    transitions: HashMap<(String, ExitBucket), Vec<(String, u32)>>,

    // New: project-type-scoped bigrams
    // (project_type, prev_command) → next_command
    project_bigrams: HashMap<(String, String), Vec<(String, u32)>>,
}

enum ExitBucket {
    Success,  // 0
    Failure,  // 1-125
    Signal,   // 126+
}
```

Project type detection reuses the existing `spec_autogen` scanning — if `Cargo.toml` exists, it's a Rust project; if `package.json`, it's Node; etc.

### Config

```toml
[llm]
# ... (shared with spec discovery)
workflow_prediction = true         # enable LLM workflow predictions
workflow_max_diff_tokens = 2000    # max tokens of git diff for commit message generation

[workflow]
enabled = true                     # enable workflow prediction (Phase 1: bigram only)
min_probability = 0.15             # minimum bigram probability to suggest
```

### Protocol Changes

No protocol changes required. Workflow predictions are delivered as regular `Suggestion` or `Update` responses from the existing source types. The description field ("predicted next command") distinguishes them in the dropdown.

### Security

- Recent commands are already sent in every request — no new data exposure
- `git diff --staged` output is only sent to the LLM when generating commit messages
- The `security.command_blocklist` is applied to recent commands before including them in LLM prompts
- Commit message generation respects `security.scrub_paths` and `security.scrub_env_keys`

### Cost Analysis

Workflow predictions only fire when the buffer is empty and the bigram predictor has no confident match. In practice, this is ~5-15 times per working session. Using Claude Haiku:

| Scenario | Calls/day | Input tokens | Output tokens | Daily cost |
|---|---|---|---|---|
| Light usage (20 empty-buffer events) | ~5 LLM calls | ~1500 | ~100 | ~$0.001 |
| Heavy usage (100 empty-buffer events) | ~25 LLM calls | ~7500 | ~500 | ~$0.005 |
| Commit message (includes diff) | ~3 calls | ~6000 | ~150 | ~$0.003 |

### Implementation Order

1. **Wire `predict()` into the pipeline** — add WorkflowProvider, position weights, activation logic
2. **Activate `last_exit_code`** — remove `#[allow(dead_code)]`, add exit-code-aware transitions
3. **Add project-type-scoped bigrams** — detect project type, scope transitions
4. **Add LLM fallback** — async workflow prediction for novel sequences
5. **Add commit message generation** — `git diff --staged` → LLM → suggested commit message
6. **Add argument enrichment** — LLM fills in arguments for bigram-predicted commands

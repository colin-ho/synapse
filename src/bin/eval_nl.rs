use std::collections::HashMap;
use std::io::Write;
use std::time::{Duration, Instant};

use clap::Parser;
use serde::{Deserialize, Serialize};

use synapse::config::LlmConfig;
use synapse::llm::{LlmClient, NlTranslationContext};

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "eval-nl", about = "NL translation eval harness for Synapse")]
struct Cli {
    /// Path to eval cases TOML file
    #[arg(long, default_value = "evals/nl_cases.toml")]
    cases: String,

    /// Comma-separated list of model identifiers to test
    #[arg(long)]
    models: String,

    /// LLM API base URL
    #[arg(long, default_value = "http://127.0.0.1:1234")]
    base_url: String,

    /// Temperature for generation
    #[arg(long, default_value_t = 0.3)]
    temperature: f32,

    /// Request timeout in milliseconds
    #[arg(long, default_value_t = 15000)]
    timeout_ms: u64,

    /// Only run cases matching these tags (comma-separated)
    #[arg(long)]
    tags: Option<String>,

    /// Number of times to repeat each case
    #[arg(long, default_value_t = 1)]
    repeat: usize,

    /// Print each result as it completes
    #[arg(long)]
    verbose: bool,

    /// Max suggestions to request from the LLM per case
    #[arg(long, default_value_t = 3)]
    max_suggestions: usize,

    /// Force JSON output to stdout instead of auto-generating a file
    #[arg(long)]
    stdout: bool,

    /// Append /no_think to queries (disables reasoning for Qwen3 models)
    #[arg(long)]
    no_think: bool,
}

// ---------------------------------------------------------------------------
// Data types — TOML input
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CasesFile {
    cases: Vec<EvalCase>,
}

#[derive(Deserialize, Clone)]
struct EvalCase {
    id: String,
    query: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default = "default_cwd")]
    cwd: String,
    #[serde(default = "default_os")]
    os: String,
    #[serde(default)]
    project_type: Option<String>,
    #[serde(default)]
    available_tools: Vec<String>,
    #[serde(default)]
    recent_commands: Vec<String>,
    #[serde(default)]
    git_branch: Option<String>,
    #[serde(default)]
    project_commands: HashMap<String, Vec<String>>,
    #[serde(default)]
    cwd_entries: Vec<String>,
    #[serde(default)]
    relevant_specs: HashMap<String, Vec<String>>,
    expected: Vec<ExpectedCommand>,
}

fn default_cwd() -> String {
    "/home/user".into()
}
fn default_os() -> String {
    "macOS 14.5".into()
}

#[derive(Deserialize, Clone)]
struct ExpectedCommand {
    command: String,
    #[serde(default = "default_match_mode")]
    match_mode: MatchMode,
}

fn default_match_mode() -> MatchMode {
    MatchMode::Exact
}

#[derive(Deserialize, Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum MatchMode {
    Exact,
    Contains,
    StartsWith,
    Regex,
}

// ---------------------------------------------------------------------------
// Data types — results
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct RunResult {
    case_id: String,
    model: String,
    repeat_index: usize,
    latency_ms: u64,
    generated_commands: Vec<String>,
    passed: bool,
    matched_command: Option<String>,
    matched_mode: Option<String>,
    error: Option<String>,
}

#[derive(Serialize)]
struct GridCell {
    model: String,
    total: usize,
    passed: usize,
    pass_rate: f64,
    avg_latency_ms: f64,
    p95_latency_ms: f64,
}

#[derive(Serialize)]
struct EvalOutput {
    timestamp: String,
    models: Vec<String>,
    total_runs: usize,
    results: Vec<RunResult>,
    grid: Vec<GridCell>,
}

// ---------------------------------------------------------------------------
// Scoring
// ---------------------------------------------------------------------------

fn command_matches(generated: &str, expected: &ExpectedCommand) -> bool {
    let gen = generated.trim();
    let exp = expected.command.trim();
    match expected.match_mode {
        MatchMode::Exact => gen == exp,
        MatchMode::Contains => gen.contains(exp) || exp.contains(gen),
        MatchMode::StartsWith => gen.starts_with(exp) || exp.starts_with(gen),
        MatchMode::Regex => regex::Regex::new(exp)
            .map(|re| re.is_match(gen))
            .unwrap_or(false),
    }
}

fn score_case(
    generated: &[String],
    expected: &[ExpectedCommand],
) -> (bool, Option<String>, Option<MatchMode>) {
    for gen in generated {
        for exp in expected {
            if command_matches(gen, exp) {
                return (true, Some(exp.command.clone()), Some(exp.match_mode));
            }
        }
    }
    (false, None, None)
}

// ---------------------------------------------------------------------------
// Context building
// ---------------------------------------------------------------------------

fn build_context(case: &EvalCase, query_suffix: &str) -> NlTranslationContext {
    let query = if query_suffix.is_empty() {
        case.query.clone()
    } else {
        format!("{} {}", case.query, query_suffix)
    };
    NlTranslationContext {
        query,
        cwd: case.cwd.clone(),
        os: case.os.clone(),
        project_type: case.project_type.clone(),
        available_tools: case.available_tools.clone(),
        recent_commands: case.recent_commands.clone(),
        git_branch: case.git_branch.clone(),
        project_commands: case.project_commands.clone(),
        cwd_entries: case.cwd_entries.clone(),
        relevant_specs: case.relevant_specs.clone(),
    }
}

// ---------------------------------------------------------------------------
// Model validation
// ---------------------------------------------------------------------------

async fn validate_models(base_url: &str, requested: &[String]) -> Vec<String> {
    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Warning: could not reach {url}: {e}");
            eprintln!("Proceeding with requested models (API may still work).");
            return requested.to_vec();
        }
    };

    #[derive(Deserialize)]
    struct ModelsResponse {
        data: Vec<ModelEntry>,
    }
    #[derive(Deserialize)]
    struct ModelEntry {
        id: String,
    }

    let models_resp: ModelsResponse = match resp.json().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Warning: could not parse /v1/models response: {e}");
            return requested.to_vec();
        }
    };

    let available: Vec<String> = models_resp.data.into_iter().map(|m| m.id).collect();
    let mut validated = Vec::new();

    for req in requested {
        if available.iter().any(|a| a == req) {
            validated.push(req.clone());
        } else {
            eprintln!(
                "Warning: model '{}' not found. Available: {}",
                req,
                available.join(", ")
            );
        }
    }

    if validated.is_empty() && !available.is_empty() {
        eprintln!(
            "No requested models available. Using first available: {}",
            available[0]
        );
        validated.push(available[0].clone());
    }

    validated
}

// ---------------------------------------------------------------------------
// LLM client construction
// ---------------------------------------------------------------------------

fn make_client(model: &str, base_url: &str, timeout_ms: u64) -> LlmClient {
    let config = LlmConfig {
        enabled: true,
        provider: "openai".into(),
        api_key_env: "LMSTUDIO_API_KEY".into(),
        base_url: Some(base_url.to_string()),
        model: model.to_string(),
        timeout_ms,
        max_calls_per_discovery: 0,
        natural_language: true,
        nl_max_suggestions: 5,
        temperature: 0.3,
        temperature_multi: 0.7,
        discovery: None,
    };
    LlmClient::from_config(&config, false).expect("failed to create LLM client")
}

// ---------------------------------------------------------------------------
// Auto filename
// ---------------------------------------------------------------------------

fn sanitize_slug(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .chars()
        .take(50)
        .collect()
}

fn auto_output_path(models: &[String]) -> String {
    let model_slug = models
        .iter()
        .map(|m| sanitize_slug(m))
        .collect::<Vec<_>>()
        .join("+");
    let now = chrono::Local::now().format("%Y-%m-%d_%H%M%S");
    format!("evals/results/{model_slug}_{now}.json")
}

// ---------------------------------------------------------------------------
// Output formatting
// ---------------------------------------------------------------------------

fn p95(latencies: &[u64]) -> u64 {
    if latencies.is_empty() {
        return 0;
    }
    let mut sorted = latencies.to_vec();
    sorted.sort();
    let idx = ((sorted.len() as f64) * 0.95).ceil() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() > max {
        format!("{}...", &s[..max - 3])
    } else {
        s.to_string()
    }
}

fn print_summary(
    grid: &[GridCell],
    models: &[String],
    results: &[RunResult],
    cases: &[EvalCase],
    total_runs: usize,
    num_repeats: usize,
) {
    let stderr = std::io::stderr();
    let mut w = stderr.lock();

    let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    writeln!(w, "\n{}", "=".repeat(70)).ok();
    writeln!(w, "  NL Eval Results \u{2014} {}", timestamp).ok();
    writeln!(
        w,
        "  {} cases \u{00d7} {} repeats \u{00d7} {} models = {} runs",
        cases.len(),
        num_repeats,
        models.len(),
        total_runs,
    )
    .ok();
    writeln!(w, "{}", "=".repeat(70)).ok();

    // Summary grid
    writeln!(w, "\nSUMMARY").ok();
    writeln!(w, "{}", "\u{2500}".repeat(70)).ok();
    writeln!(
        w,
        "  {:30} {:>6} {:>10} {:>10}",
        "Model", "Pass%", "Avg(s)", "P95(s)"
    )
    .ok();
    writeln!(
        w,
        "  {:30} {:>6} {:>10} {:>10}",
        "\u{2500}".repeat(30),
        "\u{2500}".repeat(6),
        "\u{2500}".repeat(10),
        "\u{2500}".repeat(10)
    )
    .ok();

    let mut sorted_grid: Vec<&GridCell> = grid.iter().collect();
    sorted_grid.sort_by(|a, b| {
        b.pass_rate
            .partial_cmp(&a.pass_rate)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    for cell in &sorted_grid {
        writeln!(
            w,
            "  {:30} {:5.1}% {:>9.1}s {:>9.1}s",
            truncate_str(&cell.model, 30),
            cell.pass_rate * 100.0,
            cell.avg_latency_ms / 1000.0,
            cell.p95_latency_ms / 1000.0,
        )
        .ok();
    }

    // Tag breakdown
    let mut all_tags: Vec<String> = cases.iter().flat_map(|c| c.tags.clone()).collect();
    all_tags.sort();
    all_tags.dedup();

    if !all_tags.is_empty() {
        writeln!(w, "\nBREAKDOWN BY TAG").ok();
        writeln!(w, "{}", "\u{2500}".repeat(70)).ok();

        let top_configs: Vec<&GridCell> = sorted_grid.iter().take(5).copied().collect();

        write!(w, "  {:12}", "Tag").ok();
        for cell in &top_configs {
            write!(w, "\u{2502} {:14}", truncate_str(&cell.model, 14)).ok();
        }
        writeln!(w).ok();

        write!(w, "  {:12}", "\u{2500}".repeat(12)).ok();
        for _ in &top_configs {
            write!(w, "\u{253c}{}", "\u{2500}".repeat(15)).ok();
        }
        writeln!(w).ok();

        for tag in &all_tags {
            write!(w, "  {:12}", tag).ok();
            for cell in &top_configs {
                let tag_results: Vec<&RunResult> = results
                    .iter()
                    .filter(|r| {
                        r.model == cell.model
                            && cases
                                .iter()
                                .any(|c| c.id == r.case_id && c.tags.contains(tag))
                    })
                    .collect();
                let total = tag_results.len();
                let passed = tag_results.iter().filter(|r| r.passed).count();
                if total > 0 {
                    write!(
                        w,
                        "\u{2502} {:2}/{:2} {:3.0}%   ",
                        passed,
                        total,
                        (passed as f64 / total as f64) * 100.0,
                    )
                    .ok();
                } else {
                    write!(w, "\u{2502} {:14}", "N/A").ok();
                }
            }
            writeln!(w).ok();
        }
    }

    // Detailed results
    writeln!(w, "\nDETAILED RESULTS").ok();
    writeln!(w, "{}", "\u{2500}".repeat(70)).ok();

    for case in cases {
        let case_results: Vec<&RunResult> =
            results.iter().filter(|r| r.case_id == case.id).collect();
        let any_pass = case_results.iter().any(|r| r.passed);
        let label = if any_pass { "PASS" } else { "FAIL" };
        writeln!(w, "[{}] {} \u{2014} {:?}", label, case.id, case.query).ok();

        for r in &case_results {
            let model_label = truncate_str(&r.model, 20);
            let status = if r.passed { "\u{2713}" } else { "\u{2717}" };
            if let Some(ref err) = r.error {
                writeln!(
                    w,
                    "  {}: {} {:.1}s ERROR: {}",
                    model_label,
                    status,
                    r.latency_ms as f64 / 1000.0,
                    err,
                )
                .ok();
            } else if r.passed {
                let mode_str = r.matched_mode.as_deref().unwrap_or("?");
                let cmd = r
                    .generated_commands
                    .first()
                    .map(|s| s.as_str())
                    .unwrap_or("");
                writeln!(
                    w,
                    "  {}: {} {:.1}s  {:?} ({} match)",
                    model_label,
                    status,
                    r.latency_ms as f64 / 1000.0,
                    cmd,
                    mode_str,
                )
                .ok();
            } else {
                let cmd = r
                    .generated_commands
                    .first()
                    .map(|s| s.as_str())
                    .unwrap_or("(empty)");
                writeln!(
                    w,
                    "  {}: {} {:.1}s  {:?}",
                    model_label,
                    status,
                    r.latency_ms as f64 / 1000.0,
                    cmd,
                )
                .ok();
                if let Some(first_exp) = case.expected.first() {
                    writeln!(w, "  {:>30} expected: {}", "", first_exp.command).ok();
                }
            }
        }
        writeln!(w).ok();
    }

    // Errors summary
    let errors: Vec<&RunResult> = results.iter().filter(|r| r.error.is_some()).collect();
    if !errors.is_empty() {
        writeln!(w, "ERRORS").ok();
        writeln!(w, "{}", "\u{2500}".repeat(70)).ok();
        for r in &errors {
            writeln!(
                w,
                "  {}: {} ({}, repeat {})",
                truncate_str(&r.model, 20),
                r.error.as_deref().unwrap_or("?"),
                r.case_id,
                r.repeat_index,
            )
            .ok();
        }
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Load cases
    let cases_text = std::fs::read_to_string(&cli.cases)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", cli.cases));
    let cases_file: CasesFile = toml::from_str(&cases_text)
        .unwrap_or_else(|e| panic!("Failed to parse {}: {e}", cli.cases));
    let mut cases = cases_file.cases;

    // Filter by tags
    if let Some(ref tag_filter) = cli.tags {
        let tags: Vec<&str> = tag_filter.split(',').map(str::trim).collect();
        cases.retain(|c| c.tags.iter().any(|t| tags.contains(&t.as_str())));
    }

    if cases.is_empty() {
        eprintln!("No eval cases to run.");
        std::process::exit(1);
    }

    // Parse and validate models
    let requested_models: Vec<String> = cli
        .models
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();
    let models = validate_models(&cli.base_url, &requested_models).await;

    if models.is_empty() {
        eprintln!("No models available. Exiting.");
        std::process::exit(1);
    }

    // Compute output path
    let output_path = if cli.stdout {
        None
    } else {
        let path = auto_output_path(&models);
        std::fs::create_dir_all("evals/results").ok();
        Some(path)
    };

    let total_runs = cases.len() * cli.repeat * models.len();
    eprintln!(
        "Running {} cases x {} repeats x {} models = {} total runs",
        cases.len(),
        cli.repeat,
        models.len(),
        total_runs,
    );
    if let Some(ref path) = output_path {
        eprintln!("Results will be written to {path}");
    }

    let mut results: Vec<RunResult> = Vec::with_capacity(total_runs);
    let mut run_count = 0usize;

    for model in &models {
        let client = make_client(model, &cli.base_url, cli.timeout_ms);

        for case in &cases {
            for repeat_idx in 0..cli.repeat {
                run_count += 1;
                let query_suffix = if cli.no_think { "/no_think" } else { "" };
                let ctx = build_context(case, query_suffix);
                let start = Instant::now();

                let result = client
                    .translate_command(&ctx, cli.max_suggestions, cli.temperature)
                    .await;

                let latency_ms = start.elapsed().as_millis() as u64;

                let run_result = match result {
                    Ok(translation) => {
                        let generated: Vec<String> = translation
                            .items
                            .iter()
                            .map(|i| i.command.clone())
                            .collect();
                        let (passed, matched_cmd, matched_mode) =
                            score_case(&generated, &case.expected);

                        RunResult {
                            case_id: case.id.clone(),
                            model: model.clone(),
                            repeat_index: repeat_idx,
                            latency_ms,
                            generated_commands: generated,
                            passed,
                            matched_command: matched_cmd,
                            matched_mode: matched_mode.map(|m| format!("{:?}", m).to_lowercase()),
                            error: None,
                        }
                    }
                    Err(e) => RunResult {
                        case_id: case.id.clone(),
                        model: model.clone(),
                        repeat_index: repeat_idx,
                        latency_ms,
                        generated_commands: vec![],
                        passed: false,
                        matched_command: None,
                        matched_mode: None,
                        error: Some(format!("{e}")),
                    },
                };

                if cli.verbose {
                    let status = if run_result.passed { "PASS" } else { "FAIL" };
                    let cmd = run_result
                        .generated_commands
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "(empty)".into());
                    eprintln!(
                        "[{}/{}] [{}] {}: {:.1}s {:?}",
                        run_count,
                        total_runs,
                        status,
                        case.id,
                        latency_ms as f64 / 1000.0,
                        cmd,
                    );
                }

                results.push(run_result);
            }
        }
    }

    // Compute grid
    let mut grid: Vec<GridCell> = Vec::new();
    for model in &models {
        let cell_results: Vec<&RunResult> = results.iter().filter(|r| r.model == *model).collect();

        let total = cell_results.len();
        let passed = cell_results.iter().filter(|r| r.passed).count();
        let latencies: Vec<u64> = cell_results.iter().map(|r| r.latency_ms).collect();
        let avg_latency = if latencies.is_empty() {
            0.0
        } else {
            latencies.iter().sum::<u64>() as f64 / latencies.len() as f64
        };

        grid.push(GridCell {
            model: model.clone(),
            total,
            passed,
            pass_rate: if total > 0 {
                passed as f64 / total as f64
            } else {
                0.0
            },
            avg_latency_ms: avg_latency,
            p95_latency_ms: p95(&latencies) as f64,
        });
    }

    // Print human-readable summary to stderr
    print_summary(&grid, &models, &results, &cases, total_runs, cli.repeat);

    // JSON output
    let output = EvalOutput {
        timestamp: chrono::Local::now().to_rfc3339(),
        models: models.clone(),
        total_runs,
        results,
        grid,
    };

    let json = serde_json::to_string_pretty(&output).expect("failed to serialize results");

    if let Some(ref path) = output_path {
        std::fs::write(path, &json).unwrap_or_else(|e| panic!("Failed to write {path}: {e}"));
        eprintln!("\nJSON results written to {path}");
    } else {
        println!("{json}");
    }
}

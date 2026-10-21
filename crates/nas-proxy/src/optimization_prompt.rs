//! The optimization prompt: an English text per installed model for an AI
//! chat, with what that chat needs to suggest faster settings. Only this
//! module knows its wording and what it leaves out: never a path, the MAC,
//! a key, the Git remote or a live reading.

use std::{fmt::Write, sync::Arc};

use strata_admin_ui::api::{CodeBenchRun, CodeBenchState};
use strata_pc_protocol::{
    SamplingField, SamplingSource, SamplingSupport, StrataBenchmark, StrataInfo, StrataModel,
};

use crate::{clock, state::AppState};

const STRATA_REPO: &str = "https://github.com/Niko1221/Strata";
const NOT_RUN: &str = "not run yet";

/// The prompt for installed `model`, from what the PC service reports now
/// or reported last; never wakes the PC. `Err` says why there is none.
pub async fn prompt(state: &Arc<AppState>, model: &str) -> Result<String, String> {
    let page = crate::strata::page(state).await;
    let Some(info) = page.info else {
        return Err(page
            .problem
            .unwrap_or_else(|| "The PC service has not reported Strata yet.".into()));
    };
    let read_at = if page.stale {
        state.strata.last_at()
    } else {
        None
    };
    build(&info, model, &state.code_bench.results(), read_at)
}

/// The prompt from `info`; `read_at` is when it was read, if not just now.
pub fn build(
    info: &StrataInfo,
    model: &str,
    code_bench: &[CodeBenchRun],
    read_at: Option<u64>,
) -> Result<String, String> {
    let m = info
        .models
        .iter()
        .find(|m| m.id == model)
        .ok_or_else(|| format!("{model} is not installed."))?;
    let mut out = String::new();
    let o = &mut out;
    let _ = writeln!(
        o,
        "I run a local LLM with Strata ({STRATA_REPO}), an inference engine of its own, \
         not llama.cpp or llama-server. Help me make the model below as fast as possible \
         on my PC without making its answers worse."
    );
    let _ = writeln!(o);
    let _ = writeln!(
        o,
        "Suggest changes to the model's engine options (`args`, `user_args`) and to its \
         `sampling` block that give the most tokens per second on this hardware. Give one \
         reason for each change. Suggest only options this Strata version has, and say when \
         a change can alter the answers. End your answer with a ready-to-paste `user_args` \
         list and `sampling` block."
    );
    if let Some(at) = read_at {
        let _ = writeln!(o);
        let _ = writeln!(
            o,
            "This data was read on {}; the PC has been asleep since.",
            clock::full_text(at)
        );
    }

    let _ = writeln!(o, "\n## PC\n");
    let _ = writeln!(o, "- RAM: {:.1} GB", info.ram_gb);
    match info.vram_gb {
        Some(vram) => {
            let _ = writeln!(o, "- VRAM: {vram:.1} GB (the graphics card with the most)");
        }
        None => {
            let _ = writeln!(o, "- VRAM: not reported");
        }
    }

    let _ = writeln!(o, "\n## Strata\n");
    let version = info.version.as_deref().unwrap_or("unknown");
    let _ = writeln!(o, "- Version: commit {version} of {STRATA_REPO}");

    let _ = writeln!(o, "\n## Model {}\n", m.id);
    model_lines(o, info, m);

    let _ = writeln!(o, "\n## Benchmarks\n");
    let _ = writeln!(o, "- Speed benchmark: {}", speed_text(info, &m.id));
    let _ = writeln!(
        o,
        "- Code benchmark: {}",
        code_bench_text(code_bench, &m.id)
    );
    Ok(out)
}

fn model_lines(o: &mut String, info: &StrataInfo, m: &StrataModel) {
    if let Some(family) = &m.family {
        match info.families.iter().find(|f| &f.id == family) {
            Some(f) => {
                let _ = writeln!(o, "- Family: {} ({family})", f.title);
            }
            None => {
                let _ = writeln!(o, "- Family: {family}");
            }
        }
    }
    if let Some(size) = &m.size {
        let _ = writeln!(o, "- Quantization: {size}");
    }
    let _ = writeln!(o, "- Model files: {:.1} GB", m.disk_bytes as f64 / 1e9);
    let _ = writeln!(o, "- Active: {}", if m.active { "yes" } else { "no" });
    if let Some(context) = m.context {
        let _ = writeln!(o, "- Context: {context} tokens (`--max-context`)");
    }
    if let Some(on) = m.speed_projection {
        let _ = writeln!(
            o,
            "- Experimental speed projection (a control vector): {}",
            if on { "on" } else { "off" }
        );
    }
    if let Some(recovery) = &m.loop_recovery {
        let _ = writeln!(o, "- Loop recovery: {recovery}");
    }
    if let Some(sampling) = &m.sampling {
        if sampling.support == SamplingSupport::None {
            let _ = writeln!(
                o,
                "- Sampling: this Strata version ignores sampling defaults and decodes greedily"
            );
        } else {
            let _ = writeln!(
                o,
                "- Sampling with thinking: {}",
                fields_text(&sampling.thinking)
            );
            let _ = writeln!(
                o,
                "- Sampling without thinking: {}",
                fields_text(&sampling.instruct)
            );
        }
    }
}

/// `temperature 0.6 (config), top_k 20 (model card)`; empty is greedy.
fn fields_text(fields: &[SamplingField]) -> String {
    if fields.is_empty() {
        return "greedy".into();
    }
    fields
        .iter()
        .map(|f| {
            let source = match f.source {
                SamplingSource::ModelCard => "model card",
                SamplingSource::Config => "config",
                SamplingSource::ChatSettings => "chat settings",
            };
            format!("{} {} ({source})", f.name, f.value)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The model's last speed benchmark, from each model's runs or, from older
/// services, the single last one.
fn speed_text(info: &StrataInfo, model: &str) -> String {
    let Some(b) = info
        .benchmarks
        .iter()
        .chain(info.benchmark.iter())
        .find(|b| b.model == model)
    else {
        return NOT_RUN.into();
    };
    bench_text(b)
}

fn bench_text(b: &StrataBenchmark) -> String {
    let when = clock::full_text(b.at);
    if b.result.is_empty() {
        let why = b.error.as_deref().unwrap_or("no reason given");
        return format!("the last run on {when} failed: {why}");
    }
    let mut text = match b.decode_tps {
        Some(tps) => format!("{tps:.1} tokens/s answering"),
        None => b.result.clone(),
    };
    if let Some(tps) = b.prompt_tps {
        let _ = write!(text, ", {tps:.0} tokens/s reading a 4K prompt");
    }
    if let Some(context) = b.context {
        let _ = write!(text, ", at {context} tokens context");
    }
    if let Some(on) = b.speed_projection {
        let _ = write!(text, ", speed projection {}", if on { "on" } else { "off" });
    }
    if let Some(version) = &b.version {
        let _ = write!(text, ", Strata {version}");
    }
    let _ = write!(text, ", run on {when}");
    text
}

/// The model's last finished code benchmark run: hidden tests passed.
fn code_bench_text(runs: &[CodeBenchRun], model: &str) -> String {
    let Some(run) = runs
        .iter()
        .find(|r| r.model == model && r.state == CodeBenchState::Done)
    else {
        return NOT_RUN.into();
    };
    let passed: u32 = run.tasks.iter().map(|t| t.passed).sum();
    let total: u32 = run.tasks.iter().map(|t| t.total).sum();
    let when = clock::full_text(run.ended.unwrap_or(run.started));
    format!(
        "{passed} of {total} hidden tests passed over {} programming tasks with opencode, run on {when}",
        run.tasks.len()
    )
}

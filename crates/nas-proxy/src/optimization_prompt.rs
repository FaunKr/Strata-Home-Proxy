//! The optimization prompt: an English text per installed model for an AI
//! chat, with what that chat needs to suggest faster settings. Only this
//! module knows its wording and what it leaves out: never a path, the MAC,
//! a key, the Git remote or a live reading.

use std::{fmt::Write, sync::Arc};

use strata_admin_ui::api::{CodeBenchRun, CodeBenchState};
use strata_pc_protocol::{
    MachineFacts, SamplingField, SamplingSource, SamplingSupport, StrataBenchmark, StrataInfo,
    StrataModel,
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

/// The prompt for installed model `id` from `info`; `read_at` is when it was read, if not just now.
pub fn build(
    info: &StrataInfo,
    id: &str,
    code_bench: &[CodeBenchRun],
    read_at: Option<u64>,
) -> Result<String, String> {
    let model = info
        .models
        .iter()
        .find(|m| m.id == id)
        .ok_or_else(|| format!("{id} is not installed."))?;
    let mut text = String::new();
    let out = &mut text;
    let _ = writeln!(
        out,
        "I run a local LLM with Strata ({STRATA_REPO}), an inference engine of its own, \
         not llama.cpp or llama-server. Help me make the model below as fast as possible \
         on my PC without making its answers worse."
    );
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Suggest changes to the model's engine options (`args`, `user_args`) and to its \
         `sampling` block that give the most tokens per second on this hardware. Give one \
         reason for each change. Suggest only options this Strata version has, and say when \
         a change can alter the answers. End your answer with a ready-to-paste `user_args` \
         list and `sampling` block."
    );
    if let Some(at) = read_at {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "This data was read on {}; the PC service has not answered since.",
            clock::full_text(at)
        );
    }

    let _ = writeln!(out, "\n## PC\n");
    match &info.machine {
        Some(machine) => machine_lines(out, info, machine),
        None => {
            let _ = writeln!(out, "- RAM: {:.1} GB", info.ram_gb);
            vram_line(out, info);
        }
    }

    let _ = writeln!(out, "\n## Strata\n");
    let version = info.version.as_deref().unwrap_or("unknown");
    let _ = writeln!(out, "- Version: {version}");

    let _ = writeln!(out, "\n## Model {}\n", model.id);
    model_lines(out, info, model);
    if let Some(config) = &model.config {
        let _ = writeln!(
            out,
            "\n### Run config\n\nThe model's `strata-<id>.json`, which Strata starts it with; \
             the key is left out and paths are cut to file names.\n\n```json\n{}\n```",
            config.trim_end()
        );
    }
    if let Some(chat) = &model.chat_settings {
        let _ = writeln!(
            out,
            "\n### Chat settings\n\nWhat Strata's web app keeps for this model; its temperature, \
             top_p and top_k count over the run config's sampling.\n\n```json\n{}\n```",
            chat.trim_end()
        );
    }

    let _ = writeln!(out, "\n## Benchmarks\n");
    let _ = writeln!(out, "- Speed benchmark: {}", speed_text(info, &model.id));
    let _ = writeln!(
        out,
        "- Code benchmark: {}",
        code_bench_text(code_bench, &model.id)
    );
    Ok(text)
}

fn vram_line(out: &mut String, info: &StrataInfo) {
    match info.vram_gb {
        Some(vram) => {
            let _ = writeln!(
                out,
                "- VRAM: {vram:.1} GB (the graphics card with the most)"
            );
        }
        None => {
            let _ = writeln!(out, "- VRAM: not reported");
        }
    }
}

/// What the PC is; a fact the PC service could not read is left out.
fn machine_lines(out: &mut String, info: &StrataInfo, machine: &MachineFacts) {
    let windows = [
        machine.windows.edition.clone(),
        machine.windows.version.clone(),
        machine.windows.build.as_ref().map(|b| format!("build {b}")),
    ];
    let windows = joined(&windows, " ");
    if !windows.is_empty() {
        let _ = writeln!(out, "- OS: {windows}");
    }

    let cpu = &machine.cpu;
    let mut cpu_parts = vec![
        cpu.cores.map(|c| format!("{c} cores")),
        cpu.threads.map(|t| format!("{t} threads")),
        cpu.base_mhz.map(|m| format!("{m} MHz base clock")),
    ];
    if let Some(on) = cpu.avx2 {
        cpu_parts.push(Some(format!("AVX2 {}", yes_no(on))));
    }
    if let Some(on) = cpu.avx512 {
        cpu_parts.push(Some(format!("AVX-512 {}", yes_no(on))));
    }
    let details = joined(&cpu_parts, ", ");
    match (&cpu.name, details.is_empty()) {
        (Some(name), true) => {
            let _ = writeln!(out, "- CPU: {name}");
        }
        (Some(name), false) => {
            let _ = writeln!(out, "- CPU: {name} ({details})");
        }
        (None, false) => {
            let _ = writeln!(out, "- CPU: {details}");
        }
        (None, true) => {}
    }

    // The sticks' sum when each size is known, else what Windows sees.
    let total = machine
        .ram_sticks
        .iter()
        .map(|s| s.size_bytes)
        .sum::<Option<u64>>()
        .filter(|total| *total > 0)
        .map(|b| format!("{:.0} GB", gb(b)))
        .unwrap_or_else(|| format!("{:.1} GB", info.ram_gb));
    let mut ram = format!("- RAM: {total}");
    if !machine.ram_sticks.is_empty() {
        let filled = machine.ram_sticks.len();
        let _ = match machine.ram_slots {
            Some(slots) => write!(ram, ", {filled} of {slots} slots filled"),
            None => write!(ram, ", {filled} sticks"),
        };
    }
    let _ = writeln!(out, "{ram}");
    for stick in &machine.ram_sticks {
        let parts = [
            stick.size_bytes.map(|b| format!("{:.0} GB", gb(b))),
            stick.kind.clone(),
            stick.rated_mts.map(|m| format!("rated {m} MT/s")),
            stick.configured_mts.map(|m| format!("running at {m} MT/s")),
        ];
        let stick = joined(&parts, ", ");
        if !stick.is_empty() {
            let _ = writeln!(out, "  - Stick: {stick}");
        }
    }

    if machine.gpus.is_empty() {
        vram_line(out, info);
    }
    for gpu in &machine.gpus {
        let pcie = match (gpu.pcie_max_gen, gpu.pcie_max_lanes) {
            (Some(g), Some(l)) => {
                let mut text = format!("PCIe gen {g} x{l}");
                if let (Some(now_g), Some(now_l)) = (gpu.pcie_gen, gpu.pcie_lanes) {
                    let _ = write!(
                        text,
                        ", gen {now_g} x{now_l} when read; an idle card lowers its link"
                    );
                }
                Some(text)
            }
            _ => None,
        };
        let parts = [
            gpu.vram_bytes.map(|b| format!("{:.1} GB VRAM", gb(b))),
            gpu.driver_version.as_ref().map(|d| format!("driver {d}")),
            pcie,
        ];
        let details = joined(&parts, ", ");
        let name = gpu.name.as_deref().unwrap_or("unknown");
        if details.is_empty() {
            let _ = writeln!(out, "- Graphics card: {name}");
        } else {
            let _ = writeln!(out, "- Graphics card: {name} ({details})");
        }
    }

    if let Some(drive) = &machine.model_drive {
        let kind = match drive.as_str() {
            "nvme" => "NVMe SSD",
            "ssd" => "SSD",
            "hdd" => "hard disk",
            other => other,
        };
        let _ = writeln!(out, "- Drive with the models: {kind}");
    }
}

/// Bytes in GB as Windows counts them (2^30), so a 32 GB stick reads 32.
fn gb(bytes: u64) -> f64 {
    bytes as f64 / (1u64 << 30) as f64
}

fn yes_no(on: bool) -> &'static str {
    if on { "yes" } else { "no" }
}

fn joined(parts: &[Option<String>], sep: &str) -> String {
    parts
        .iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>()
        .join(sep)
}

fn model_lines(out: &mut String, info: &StrataInfo, model: &StrataModel) {
    if let Some(family) = &model.family {
        match info.families.iter().find(|f| &f.id == family) {
            Some(f) => {
                let _ = writeln!(out, "- Family: {} ({family})", f.title);
            }
            None => {
                let _ = writeln!(out, "- Family: {family}");
            }
        }
    }
    if let Some(size) = &model.size {
        let _ = writeln!(out, "- Quantization: {size}");
    }
    let _ = writeln!(
        out,
        "- Model files: {:.1} GB",
        model.disk_bytes as f64 / 1e9
    );
    let _ = writeln!(out, "- Active: {}", if model.active { "yes" } else { "no" });
    if let Some(context) = model.context {
        let _ = writeln!(out, "- Context: {context} tokens (`--max-context`)");
    }
    if let Some(on) = model.speed_projection {
        let _ = writeln!(
            out,
            "- Experimental speed projection (a control vector): {}",
            if on { "on" } else { "off" }
        );
    }
    if let Some(recovery) = &model.loop_recovery {
        let _ = writeln!(out, "- Loop recovery: {recovery}");
    }
    if let Some(sampling) = &model.sampling {
        if sampling.support == SamplingSupport::None {
            let _ = writeln!(
                out,
                "- Sampling: this Strata version ignores sampling defaults and decodes greedily"
            );
        } else {
            let _ = writeln!(
                out,
                "- Sampling with thinking: {}",
                fields_text(&sampling.thinking)
            );
            let _ = writeln!(
                out,
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

/// The model's last code benchmark run: hidden tests passed, or why it
/// gave none.
fn code_bench_text(runs: &[CodeBenchRun], model: &str) -> String {
    let Some(run) = runs.iter().find(|r| r.model == model) else {
        return NOT_RUN.into();
    };
    let when = clock::full_text(run.ended.unwrap_or(run.started));
    if run.state != CodeBenchState::Done {
        let why = run.error.as_deref().unwrap_or("no reason given");
        return format!("the last run on {when} failed: {why}");
    }
    let passed: u32 = run.tasks.iter().map(|t| t.passed).sum();
    let total: u32 = run.tasks.iter().map(|t| t.total).sum();
    let mut text = format!(
        "{passed} of {total} hidden tests passed over {} programming tasks with opencode, run on {when}",
        run.tasks.len()
    );
    if let Some(why) = &run.failed_later {
        let _ = write!(text, "; a later run failed: {why}");
    }
    text
}

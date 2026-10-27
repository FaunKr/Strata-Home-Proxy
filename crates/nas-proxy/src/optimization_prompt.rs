//! The optimization prompt: an English text per installed model for an AI
//! chat, with what that chat needs to suggest faster settings. Only this
//! module knows its wording and what it leaves out: never a path, the MAC,
//! a key, the Git remote or a live reading.

use std::{fmt::Write, sync::Arc};

use strata_admin_ui::api::{CodeBenchRun, CodeBenchState, RunSettings, SpeedRun};
use strata_pc_protocol::{
    DocumentedVariable, LOAD_LINES, MachineFacts, NeverRule, PresetState, SamplingField,
    SamplingSource, SamplingSupport, SpeedSwitch, StrataInfo, StrataModel, StrataOptions,
    StrataPreset, StrataTune, TuneMode, TuneUse, TunerOption, TunerTry,
};

use crate::{clock, state::AppState};

const STRATA_REPO: &str = "https://github.com/Niko1221/Strata";
const NOT_RUN: &str = "not run yet";
/// How many of a model's runs of each benchmark the prompt lists.
const RUNS_LISTED: usize = 5;

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
    let speed = state.strata.benchmarks.runs_of(model);
    let code = state.code_bench.runs_of(model);
    build(&info, model, &speed, &code, read_at)
}

/// The prompt for installed model `id` from `info` and the model's speed
/// and code benchmark runs, newest first; `read_at` is when `info` was
/// read, if not just now.
pub fn build(
    info: &StrataInfo,
    id: &str,
    speed: &[SpeedRun],
    code: &[CodeBenchRun],
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
        "Suggest changes to the model's engine options (`args`, `user_args`), its engine \
         environment (`env`) and its `sampling` block that give the most tokens per second on \
         this hardware. Give one reason for each change. Suggest only options this Strata \
         version has and only environment variables from the list under *Engine environment \
         variables*, and say when a change can alter the answers. End your answer as *Your \
         answer* at the end says."
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
    if let Some(options) = &info.options {
        options_lines(out, options);
    }
    env_variables(out, info.options.as_ref(), model);

    let _ = writeln!(out, "\n## Model {}\n", model.id);
    model_lines(out, info, model);
    if let Some(config) = &model.config {
        let _ = writeln!(
            out,
            "\n### Run config\n\nThe model's `strata-<id>.json`, which Strata starts it with; \
             secrets are left out and paths are cut to file names.\n"
        );
        fenced(out, "json", config);
    }
    if let Some(env) = &model.env {
        let _ = writeln!(
            out,
            "\n### Engine environment\n\nThe run config's `env`, which Strata puts into the \
             engine's environment at every start, as my web UI shows it in *Engine environment*:\n"
        );
        if env.is_empty() {
            let _ = writeln!(out, "None set.");
        } else {
            let lines: Vec<String> = env
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect();
            fenced(out, "text", &lines.join("\n"));
        }
    }
    if let Some(chat) = &model.chat_settings {
        let ignored = model
            .sampling
            .as_ref()
            .is_some_and(|s| s.support == SamplingSupport::None);
        let effect = if ignored {
            "this Strata version ignores their sampling and decodes greedily"
        } else {
            "their temperature, top_p and top_k count over the run config's sampling"
        };
        let _ = writeln!(
            out,
            "\n### Chat settings\n\nWhat Strata's web app keeps for this model; {effect}.\n"
        );
        fenced(out, "json", chat);
    }
    let _ = writeln!(out, "\n### Load lines\n");
    match &model.load_lines {
        Some(load) => {
            let when = clock::full_text(load.at);
            let end = if load.answered {
                "until it answered"
            } else {
                "until it ended without answering, so this load failed"
            };
            if load.lines.is_empty() {
                let _ = writeln!(out, "The last load, on {when}, printed no lines ({end}).");
            } else {
                let _ = writeln!(
                    out,
                    "Strata's output while it last loaded this model, on {when}, {end} \
                     (at most the last {LOAD_LINES} lines):\n"
                );
                fenced(out, "text", &load.lines.join("\n"));
            }
        }
        None => {
            let _ = writeln!(out, "No load seen yet.");
        }
    }

    let _ = writeln!(out, "\n### Strata's tuning\n");
    let _ = writeln!(out, "- Preset: {}", preset_text(model));
    let _ = writeln!(
        out,
        "- Last tune: {}",
        model
            .last_tune
            .as_ref()
            .map_or_else(|| NOT_RUN.into(), tune_text)
    );

    let _ = writeln!(
        out,
        "\n## Benchmarks\n\nThe model's last {RUNS_LISTED} runs of each, newest first, each with the \
         settings it ran with, so you can see which settings gave which result."
    );
    let _ = writeln!(out, "\n### Speed benchmark\n");
    runs_lines(out, speed.iter().map(speed_line));
    let _ = writeln!(out, "\n### Code benchmark\n");
    runs_lines(out, code.iter().map(code_line));
    answer_lines(out, model);
    Ok(text)
}

/// How the answer has to end: one block per field of the web UI, each
/// pasted as it is; no options for a Strata that keeps none by hand.
fn answer_lines(out: &mut String, model: &StrataModel) {
    let options = if model.user_args.is_some() {
        "a `text` block with the whole new `user_args`, one option with its value per line \
         (`--name value`), for the *Engine options* field in my web UI."
    } else {
        "an empty `text` block: my Strata version keeps no engine options set by hand."
    };
    let _ = writeln!(
        out,
        "\n## Your answer\n\nEnd your answer with exactly three code blocks, in this order, \
         each ready to paste as it is and holding nothing else:\n\n\
         1. Engine options: {options}\n\
         2. Engine environment: a `text` block with the whole new `env`, one `NAME=value` per \
         line, for the *Engine environment* field.\n\
         3. Sampling: a `json` block with the whole new `sampling` object of the run config.\n\n\
         Leave a block empty when nothing in its field should change."
    );
}

/// The model's preset and Strata's word on it: an active one wins over the
/// run config, so the chat has to know its settings.
fn preset_text(model: &StrataModel) -> String {
    let Some(presets) = &model.presets else {
        return "unknown; the PC service has not reported Strata's presets".into();
    };
    const OTHER: &str = "the last tune was for other settings, such as another context";
    let Some(preset) = &presets.latest else {
        return if presets.other_context {
            format!("none for this context; {OTHER}")
        } else {
            "none".into()
        };
    };
    let what = preset_what(preset);
    let because = if preset.reasons.is_empty() {
        String::new()
    } else {
        format!(", because {}", one_line(&preset.reasons.join(", ")))
    };
    let mut text = match (preset.active, preset.state) {
        (false, _) => format!("{what}, saved but not applied: the run config runs as it is"),
        (true, Some(PresetState::Hard)) => {
            format!("{what}, active but not applied{because}: the run config runs as it is")
        }
        (true, state) => {
            let mut text = format!(
                "{what}, active: Strata puts it over the run config when the model starts, \
                 so its settings win over `args` and `user_args`"
            );
            if state == Some(PresetState::Soft) {
                let _ = write!(text, "; Strata offers a re-check{because}");
            }
            text
        }
    };
    if presets.other_context {
        let _ = write!(text, "; {OTHER}");
    }
    text
}

/// Tuned settings as code: one line each, with paths cut to file names as
/// in the run config.
fn settings_code(settings: &[String]) -> String {
    let each: Vec<String> = settings
        .iter()
        .map(|setting| {
            let words: Vec<String> = setting
                .split_whitespace()
                .map(|word| match word.split_once('=') {
                    Some((name, value)) if is_path(value) => format!("{name}={}", file_name(value)),
                    _ if is_path(word) => file_name(word).to_string(),
                    _ => word.to_string(),
                })
                .collect();
            format!("`{}`", words.join(" ").replace('`', "'"))
        })
        .collect();
    each.join(", ")
}

/// A Windows or Unix path, not a value such as `1/2`.
fn is_path(text: &str) -> bool {
    text.contains('\\') || text.starts_with('/') || text.contains(":/")
}

fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

/// `"Name" (--kv-grow; 31.2 → 35.1 tokens/s answering, +12%)`.
fn preset_what(preset: &StrataPreset) -> String {
    let mut parts = Vec::new();
    if !preset.settings.is_empty() {
        parts.push(settings_code(&preset.settings));
    }
    let speed = match (preset.base_tps, preset.decode_tps) {
        (Some(base), Some(tuned)) => Some(format!("{base:.1} → {tuned:.1} tokens/s answering")),
        (None, Some(tuned)) => Some(format!("{tuned:.1} tokens/s answering")),
        _ => None,
    };
    let gain = preset.gain_pct.map(|g| format!("{g:+.0}%"));
    let measured: Vec<String> = speed.into_iter().chain(gain).collect();
    if !measured.is_empty() {
        parts.push(format!("measured {}", measured.join(", ")));
    }
    let name = one_line(&preset.name);
    if parts.is_empty() {
        format!("\"{name}\"")
    } else {
        format!("\"{name}\" ({})", parts.join("; "))
    }
}

/// The model's last tune: when, how and what it found.
fn tune_text(tune: &StrataTune) -> String {
    let mode = match tune.mode {
        TuneMode::Quick => "quick",
        TuneMode::Thorough => "thorough",
    };
    let usage = match tune.usage {
        TuneUse::Coding => "coding",
        TuneUse::Chat => "chat",
    };
    let found = match (tune.code.as_str(), tune.base_tps, tune.tuned_tps) {
        ("faster", Some(base), Some(tuned)) => {
            let mut text = format!("faster, {base:.1} → {tuned:.1} tokens/s");
            if !tune.changed.is_empty() {
                let _ = write!(text, " with {}", settings_code(&tune.changed));
            }
            if let Some(name) = &tune.preset_name {
                let _ = write!(text, ", saved as preset \"{}\"", one_line(name));
            }
            text
        }
        ("already_fastest", ..) => "the model's own settings were already the fastest".into(),
        (code, ..) => match &tune.message {
            Some(message) => format!("{code}: {}", one_line(message)),
            None => code.to_string(),
        },
    };
    format!(
        "on {} ({mode}, for {usage}): {found}",
        clock::full_text(tune.at)
    )
}

/// What this Strata version offers to tune: the tuner's table, what it
/// never picks and the engine's own help.
fn options_lines(out: &mut String, options: &StrataOptions) {
    if !options.tuner.is_empty() {
        let _ = writeln!(
            out,
            "\n### Options the tuner knows\n\nStrata's own tuner (`strata tune`) searches \
             these engine options. Answers: `exact` gives the same answers as without the \
             option, `placement` the same model with other numbers on the GPU, `quality` can \
             change the answers, `nonrepeatable` differs from run to run.\n"
        );
        let _ = writeln!(out, "| Option | Values | Answers |");
        let _ = writeln!(out, "|---|---|---|");
        for option in &options.tuner {
            let _ = writeln!(
                out,
                "| {} | {} | {} |",
                cell(&format!("`{}`", option.flag)),
                cell(&option_values(option)),
                cell(&option.answer)
            );
        }
    }
    if !options.tries.is_empty() {
        let _ = writeln!(out, "\n### The tuner's own tries\n");
        for one in &options.tries {
            let _ = writeln!(out, "- {}", one_line(&try_text(one)));
        }
    }
    if options.never.iter().any(|rule| !rule.env) {
        let _ = writeln!(
            out,
            "\n### Settings the tuner never picks\n\nEach with Strata's reason:\n"
        );
        // Variables are listed with the others they may be set to.
        for rule in options.never.iter().filter(|rule| !rule.env) {
            let _ = writeln!(
                out,
                "- {}",
                one_line(&format!("{}: {}", never_text(rule), rule.reason))
            );
        }
    }
    if let Some(help) = &options.engine_help {
        let _ = writeln!(
            out,
            "\n### Engine help\n\nWhat the engine prints for `--help`:\n"
        );
        fenced(out, "text", help);
    }
}

/// The engine environment variables an answer may suggest for `model`:
/// those of the tuner's tries and of Strata's speed switches, those the
/// tuner never sets, with Strata's words, and those only Strata's
/// documentation names.
fn env_variables(out: &mut String, options: Option<&StrataOptions>, model: &StrataModel) {
    let _ = writeln!(out, "\n### Engine environment variables\n");
    let tries: Vec<&TunerTry> = options
        .map(|o| o.tries.iter().filter(|t| !t.env.is_empty()).collect())
        .unwrap_or_default();
    let never: Vec<&NeverRule> = options
        .map(|o| o.never.iter().filter(|rule| rule.env).collect())
        .unwrap_or_default();
    let switches: Vec<&SpeedSwitch> = options
        .map(|o| {
            o.switches
                .iter()
                .filter(|s| !s.env.is_empty())
                .filter(|s| s.models.iter().any(|m| m.eq_ignore_ascii_case(&model.id)))
                .collect()
        })
        .unwrap_or_default();
    let args = model
        .started_args
        .as_ref()
        .or(model.user_args.as_ref())
        .map(Vec::as_slice)
        .unwrap_or_default();
    let documented: Vec<&DocumentedVariable> = options
        .map(|o| o.documented.iter().filter(|d| applies(d, args)).collect())
        .unwrap_or_default();
    if tries.is_empty() && never.is_empty() && switches.is_empty() && documented.is_empty() {
        let _ = writeln!(
            out,
            "The PC service did not report the variables Strata's tuner knows, so suggest none."
        );
        return;
    }
    if switches.is_empty() && documented.is_empty() {
        let _ = writeln!(
            out,
            "The variables Strata's tuner knows; suggest no others. Answers as in *Options the tuner knows*.\n"
        );
    } else {
        let _ = writeln!(
            out,
            "Suggest only variables listed in this section. The tuner's are answered as in \
             *Options the tuner knows*.\n"
        );
    }
    for one in tries {
        let set: Vec<String> = one
            .env
            .iter()
            .map(|(name, value)| match value {
                Some(value) => format!("`{name}={value}`"),
                None => format!("`{name}` removed"),
            })
            .collect();
        let text = format!("{}: {}", set.join(", "), try_text(one));
        let _ = writeln!(out, "- {}", one_line(&text));
    }
    for rule in never {
        let _ = writeln!(
            out,
            "- {}",
            one_line(&format!("never {}: {}", never_text(rule), rule.reason))
        );
    }
    for switch in switches {
        let set: Vec<String> = switch
            .env
            .iter()
            .map(|(name, value)| format!("`{name}={value}`"))
            .collect();
        let default = if switch.default_on { "on" } else { "off" };
        let text = format!(
            "{} ({default} by default): {}. {}",
            set.join(", "),
            switch.label,
            switch.effect
        );
        let _ = writeln!(out, "- {}", one_line(&text));
    }
    if documented.is_empty() {
        return;
    }
    let _ = writeln!(
        out,
        "\n#### From Strata's documentation, not tried by its tuner\n"
    );
    for one in documented {
        let value = one.value.as_deref().unwrap_or("<value>");
        let answers = if one.changes_answers {
            "Changes answers at rounding level."
        } else {
            "Does not change answers."
        };
        let text = format!("`{}={value}`: {} {answers}", one.name, one.text);
        let _ = writeln!(out, "- {}", one_line(&text));
    }
}

/// Whether a documented variable counts for a model started with `args`.
fn applies(variable: &DocumentedVariable, args: &[String]) -> bool {
    let has = |flag: &String| {
        args.iter().any(|a| {
            a == flag
                || a.strip_prefix(flag.as_str())
                    .is_some_and(|v| v.starts_with('='))
        })
    };
    (variable.needs_args.is_empty() || variable.needs_args.iter().any(has))
        && !variable.not_with_args.iter().any(has)
}

fn option_values(option: &TunerOption) -> String {
    if !option.takes_value {
        return "on or off".into();
    }
    if !option.choices.is_empty() {
        return option.choices.join(", ");
    }
    match (&option.min, &option.max) {
        (Some(min), Some(max)) => format!("{min} to {max}"),
        (Some(min), None) => format!("from {min}"),
        (None, Some(max)) => format!("up to {max}"),
        (None, None) => "any".into(),
    }
}

/// `Label (`--spec 3`), for flash-next: placement`.
fn try_text(one: &TunerTry) -> String {
    let mut text = one.label.clone();
    if !one.args.is_empty() {
        let _ = write!(text, " (`{}`)", one.args.join(" "));
    }
    if !one.families.is_empty() {
        let _ = write!(text, ", for {}", one.families.join(", "));
    }
    let _ = write!(text, ": {}", one.answer);
    text
}

/// Text from the PC on one line, so it cannot start a heading or list.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Text from the PC in a table cell, so it cannot end the cell.
fn cell(text: &str) -> String {
    one_line(text).replace('|', "\\|")
}

/// The setting and when the rule holds, e.g. `` `--spec` at_most 2, for flash-next ``.
fn never_text(rule: &NeverRule) -> String {
    let mut text = format!("`{}`", rule.setting);
    if !rule.values.is_empty() {
        let _ = write!(text, " = {}", rule.values.join(" or "));
    }
    if let Some(test) = &rule.test {
        let _ = write!(text, " {test}");
    }
    if let Some(limit) = &rule.limit {
        let _ = write!(text, " {limit}");
    }
    if !rule.when.is_empty() {
        let _ = write!(text, ", on PCs with {}", rule.when.join(", "));
    }
    if !rule.families.is_empty() {
        let _ = write!(text, ", for {}", rule.families.join(", "));
    }
    text
}

/// `text` in a fence longer than any run of backticks in it, so text from
/// the PC cannot end the block early.
fn fenced(out: &mut String, lang: &str, text: &str) {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    let _ = writeln!(out, "{fence}{lang}\n{}\n{fence}", text.trim_end());
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
            format!("{} {} ({source})", plain(&f.name), f.value)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Up to `RUNS_LISTED` run lines, or that there is none.
fn runs_lines(out: &mut String, lines: impl Iterator<Item = String>) {
    let mut any = false;
    for line in lines.take(RUNS_LISTED) {
        let _ = writeln!(out, "- {line}");
        any = true;
    }
    if !any {
        let _ = writeln!(out, "- {NOT_RUN}");
    }
}

/// A speed benchmark run: when, what it measured or why it failed, and
/// its settings.
fn speed_line(run: &SpeedRun) -> String {
    let record = &run.record;
    let result = if run.failed {
        failed_text(record.error.as_deref())
    } else {
        let mut text = match record.decode_tps {
            Some(tps) => format!("{tps:.1} tokens/s answering"),
            None => plain(&record.result),
        };
        if let Some(tps) = record.prompt_tps {
            let _ = write!(text, ", {tps:.0} tokens/s reading a 4K prompt");
        }
        text
    };
    let settings = match &run.settings {
        Some(settings) => settings_text(settings),
        // What the measurement itself says about how it ran.
        None => {
            let mut text = "Settings not recorded".to_string();
            if !run.failed {
                if let Some(context) = record.context {
                    let _ = write!(text, "; {context} tokens context");
                }
                if let Some(on) = record.speed_projection {
                    let _ = write!(text, "; speed projection {}", on_off(on));
                }
                if let Some(version) = &record.version {
                    let _ = write!(text, "; Strata {}", plain(version));
                }
            }
            text
        }
    };
    format!("{}: {result}. {settings}", clock::full_text(run.at))
}

/// A code benchmark run: when, its score with the earlier tests it broke
/// or why it gave none, and its settings.
fn code_line(run: &CodeBenchRun) -> String {
    let result = if run.state == CodeBenchState::Done {
        let score = run.score();
        let total: u32 = run.tasks.iter().map(|t| t.total).sum();
        let broken = match run.tasks.iter().map(|t| t.broken.len()).sum::<usize>() {
            0 => String::new(),
            1 => ", 1 earlier test broken".into(),
            n => format!(", {n} earlier tests broken"),
        };
        format!(
            "{score} of {total} hidden tests passed over {} programming tasks with opencode{broken}",
            run.tasks.len()
        )
    } else {
        failed_text(run.error.as_deref())
    };
    let settings = run
        .settings
        .as_ref()
        .map_or_else(|| "Settings not recorded".into(), settings_text);
    let when = clock::full_text(crate::code_bench::ended_or_started(run));
    format!("{when}: {result}. {settings}")
}

/// `failed: why`, with the error as plain text.
fn failed_text(error: Option<&str>) -> String {
    format!("failed: {}", plain(error.unwrap_or("no reason given")))
}

/// Text from the PC on one line, with paths cut to file names and no
/// backticks, so it can sit in a list line without opening code.
fn plain(text: &str) -> String {
    text.split_whitespace()
        .map(|word| if is_path(word) { file_name(word) } else { word })
        .collect::<Vec<_>>()
        .join(" ")
        .replace('`', "'")
}

/// `Settings: 65536 tokens context; speed projection off; …`.
fn settings_text(s: &RunSettings) -> String {
    let mut parts = Vec::new();
    if let Some(context) = s.context {
        parts.push(format!("{context} tokens context"));
    }
    if let Some(on) = s.speed_projection {
        parts.push(format!("speed projection {}", on_off(on)));
    }
    if let Some(version) = &s.strata_version {
        parts.push(format!("Strata {}", plain(version)));
    }
    parts.push(match &s.preset {
        Some(p) if p.settings.is_empty() => format!("preset \"{}\"", plain(&p.name)),
        Some(p) => format!(
            "preset \"{}\" ({})",
            plain(&p.name),
            settings_code(&p.settings)
        ),
        None => "no preset".into(),
    });
    if let Some(env) = &s.env {
        parts.push(if env.is_empty() {
            "no engine environment".into()
        } else {
            let set: Vec<String> = env
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect();
            format!("engine environment {}", settings_code(&set))
        });
    }
    if let Some(options) = &s.engine_options {
        parts.push(if options.is_empty() {
            "no engine options".into()
        } else {
            format!("engine options {}", settings_code(options))
        });
    }
    if let Some(sampling) = &s.sampling {
        parts.push(if sampling.support == SamplingSupport::None {
            "greedy decoding (this Strata ignores sampling)".into()
        } else {
            format!(
                "sampling with thinking {}, without thinking {}",
                fields_text(&sampling.thinking),
                fields_text(&sampling.instruct)
            )
        });
    }
    format!("Settings: {}", parts.join("; "))
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

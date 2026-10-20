//! The sampling a model gets when a request leaves it out, as Strata merges
//! it in `serve/server.py` (`Service._run`, Strata commit e88ee39d): the
//! model card's set for the thinking state, then the config's `sampling`
//! block, then the Chat settings of Strata's web app. A request's own
//! fields win over all of them.

use serde_json::{Map, Value};
pub use strata_pc_protocol::{SamplingField, SamplingSource, SamplingSupport, StrataSampling};

/// Names in `serve/server.py` that tell the versions apart.
const MODEL_CARD_NAME: &str = "MODEL_CARD_SAMPLING";
const CONFIG_BLOCK_NAME: &str = "sampling_defaults_from_config";

/// Which of the defaults the Strata in `strata_dir` applies, read from its
/// `serve/server.py` by name rather than by commit: the model card's sets
/// (Strata a8091f4d, 2026-10-15, on), only the config's `sampling` block
/// (9e6e5b7a, 2026-09-26, on), or neither. `None` while `serve/server.py`
/// cannot be read, so a passing error changes nothing.
pub fn support_of(strata_dir: &std::path::Path) -> Option<SamplingSupport> {
    let server = std::fs::read_to_string(strata_dir.join("serve").join("server.py"))
        .inspect_err(|e| tracing::debug!("serve/server.py not readable: {e}"))
        .ok()?;
    Some(if server.contains(MODEL_CARD_NAME) {
        SamplingSupport::ModelCard
    } else if server.contains(CONFIG_BLOCK_NAME) {
        SamplingSupport::Config
    } else {
        SamplingSupport::None
    })
}

type Set = &'static [(&'static str, f64)];

/// `MODEL_CARD_SAMPLING`: each family's sets, by the config's `arch`.
/// min_p 0, repetition_penalty 1 and presence_penalty 0 are Strata's "off"
/// and not listed.
const MODEL_CARD: &[(&str, Set, Set)] = &[
    // Qwen3.8-Flash-Next and its fine-tunes: arch, thinking, instruct.
    (
        "qwen4exp",
        &[("temperature", 1.0), ("top_p", 0.95), ("top_k", 20.0)],
        &[
            ("temperature", 0.7),
            ("top_p", 0.8),
            ("top_k", 20.0),
            ("presence_penalty", 1.5),
        ],
    ),
    // Qwen3.6-35B-A3B.
    (
        "qwen35moe",
        &[
            ("temperature", 1.0),
            ("top_p", 0.95),
            ("top_k", 20.0),
            ("presence_penalty", 1.5),
        ],
        &[
            ("temperature", 0.7),
            ("top_p", 0.8),
            ("top_k", 20.0),
            ("presence_penalty", 1.5),
        ],
    ),
];

/// Strata's family when a config names no `arch`.
const DEFAULT_ARCH: &str = "qwen4exp";

/// The sampling fields shown, in this order, and the config block's other
/// spellings of them.
const FIELDS: &[&str] = &[
    "temperature",
    "top_p",
    "top_k",
    "min_p",
    "presence_penalty",
    "repetition_penalty",
    "frequency_penalty",
];
const ALIASES: &[(&str, &str)] = &[("repeat_penalty", "repetition_penalty")];

/// The keys of the Chat settings that are sampling (`clean_shared_defaults`).
const CHAT_FIELDS: &[&str] = &["temperature", "top_p", "top_k"];

fn arch(cfg: &Map<String, Value>) -> &str {
    cfg.get("arch")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_ARCH)
}

fn model_card(arch: &str, thinking: bool) -> Set {
    MODEL_CARD
        .iter()
        .find(|(a, ..)| *a == arch)
        .map_or(&[], |(_, t, i)| if thinking { t } else { i })
}

/// The `sampling` block the service keeps in a config: the thinking set
/// where Strata reads only the block, so a plain request is not greedy;
/// none where Strata has the model card (the block would hide its instruct
/// set) or ignores the block.
pub fn managed_block(support: SamplingSupport, cfg: &Map<String, Value>) -> Option<Value> {
    (support == SamplingSupport::Config).then(|| {
        model_card(arch(cfg), true)
            .iter()
            .map(|&(name, value)| (name.to_string(), number(name, value)))
            .collect::<Map<_, _>>()
            .into()
    })
}

/// `top_k` is a whole number in Strata's checks.
fn number(name: &str, value: f64) -> Value {
    if name == "top_k" {
        Value::from(value as u64)
    } else {
        Value::from(value)
    }
}

/// The sampling Strata applies for a model with config `cfg` and the Chat
/// settings `chat` (`strata-<id>.shared-settings.json`), which the service
/// only reads.
pub fn effective(
    support: SamplingSupport,
    cfg: &Map<String, Value>,
    chat: Option<&Map<String, Value>>,
) -> StrataSampling {
    let fields = |thinking: bool| {
        let mut out: Vec<SamplingField> = Vec::new();
        let mut put = |name: &str, value: f64, source: SamplingSource| match out
            .iter_mut()
            .find(|f| f.name == name)
        {
            Some(f) => {
                f.value = value;
                f.source = source;
            }
            None => out.push(SamplingField {
                name: name.into(),
                value,
                source,
            }),
        };
        if support == SamplingSupport::ModelCard {
            for &(name, value) in model_card(arch(cfg), thinking) {
                put(name, value, SamplingSource::ModelCard);
            }
        }
        if support != SamplingSupport::None
            && let Some(block) = cfg.get("sampling").and_then(Value::as_object)
        {
            for (key, value) in block {
                let name = ALIASES
                    .iter()
                    .find(|(alias, _)| alias == key)
                    .map_or(key.as_str(), |(_, name)| name);
                if let (true, Some(value)) = (FIELDS.contains(&name), value.as_f64()) {
                    put(name, value, SamplingSource::Config);
                }
            }
        }
        if support != SamplingSupport::None {
            for (key, value) in chat.into_iter().flatten() {
                if let (true, Some(value)) = (CHAT_FIELDS.contains(&key.as_str()), value.as_f64()) {
                    put(key, value, SamplingSource::ChatSettings);
                }
            }
        }
        out.sort_by_key(|f| FIELDS.iter().position(|&n| n == f.name));
        out
    };
    StrataSampling {
        support,
        thinking: fields(true),
        instruct: fields(false),
    }
}

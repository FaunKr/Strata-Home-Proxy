//! The sampling a model gets when a request leaves it out, as Strata merges
//! it in `serve/server.py` (`Service._run`, Strata commit e88ee39d): the
//! model card's set for the thinking state, then the config's `sampling`
//! block, then the Chat settings of Strata's web app. A request's own
//! fields win over all of them.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Which of the defaults the installed Strata applies, read from its
/// `serve/server.py` by name rather than by commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Support {
    /// The model card's sets (Strata a8091f4d, 2026-10-15, on).
    ModelCard,
    /// Only the config's `sampling` block (9e6e5b7a, 2026-09-26, on).
    Config,
    /// Neither: a request without sampling is greedy.
    None,
}

/// Names in `serve/server.py` that tell the versions apart.
const MODEL_CARD_NAME: &str = "MODEL_CARD_SAMPLING";
const CONFIG_BLOCK_NAME: &str = "sampling_defaults_from_config";

impl Support {
    /// What the Strata in `strata_dir` applies; one that cannot be read
    /// applies none.
    pub fn of(strata_dir: &std::path::Path) -> Self {
        let server =
            std::fs::read_to_string(strata_dir.join("serve").join("server.py")).unwrap_or_default();
        if server.contains(MODEL_CARD_NAME) {
            Self::ModelCard
        } else if server.contains(CONFIG_BLOCK_NAME) {
            Self::Config
        } else {
            Self::None
        }
    }
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
pub fn managed_block(support: Support, cfg: &Map<String, Value>) -> Option<Value> {
    (support == Support::Config).then(|| {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    ModelCard,
    Config,
    ChatSettings,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Field {
    pub name: String,
    pub value: f64,
    pub source: Source,
}

/// What a request without sampling fields gets.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelSampling {
    pub support: Support,
    /// With thinking; empty is greedy.
    pub thinking: Vec<Field>,
    /// Without thinking.
    pub instruct: Vec<Field>,
}

/// The sampling Strata applies for a model with config `cfg` and the Chat
/// settings `chat` (`strata-<id>.shared-settings.json`), which the service
/// only reads.
pub fn effective(
    support: Support,
    cfg: &Map<String, Value>,
    chat: Option<&Map<String, Value>>,
) -> ModelSampling {
    let fields = |thinking: bool| {
        let mut out: Vec<Field> = Vec::new();
        let mut put = |name: &str, value: f64, source: Source| match out
            .iter_mut()
            .find(|f| f.name == name)
        {
            Some(f) => {
                f.value = value;
                f.source = source;
            }
            None => out.push(Field {
                name: name.into(),
                value,
                source,
            }),
        };
        if support == Support::ModelCard {
            for &(name, value) in model_card(arch(cfg), thinking) {
                put(name, value, Source::ModelCard);
            }
        }
        if support != Support::None
            && let Some(block) = cfg.get("sampling").and_then(Value::as_object)
        {
            for (key, value) in block {
                let name = ALIASES
                    .iter()
                    .find(|(alias, _)| alias == key)
                    .map_or(key.as_str(), |(_, name)| name);
                if let (true, Some(value)) = (FIELDS.contains(&name), value.as_f64()) {
                    put(name, value, Source::Config);
                }
            }
        }
        if support != Support::None {
            for (key, value) in chat.into_iter().flatten() {
                if let (true, Some(value)) = (CHAT_FIELDS.contains(&key.as_str()), value.as_f64()) {
                    put(key, value, Source::ChatSettings);
                }
            }
        }
        out.sort_by_key(|f| FIELDS.iter().position(|&n| n == f.name));
        out
    };
    ModelSampling {
        support,
        thinking: fields(true),
        instruct: fields(false),
    }
}

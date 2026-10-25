//! The integration tests as one binary: one link instead of one per file.
//! Every file here needs its `mod` line (scripts/check.sh checks it).

mod common;

mod admin;
mod api_keys;
mod code_bench;
mod data_files;
mod events;
mod forced;
mod hardware;
mod history;
mod login;
mod models_list;
mod optimization_prompt;
mod overview;
mod pc_service_changelog;
mod pc_service_update;
mod pc_setup;
mod power;
mod power_samples;
mod presets;
mod proxy;
mod public_name;
mod relay;
mod reported_mac;
mod request_log;
mod settings;
mod settings_page;
mod speed_history;
mod strata_key;
mod strata_output;
mod strata_page;
mod strata_update;
mod tune;
mod ui_language;

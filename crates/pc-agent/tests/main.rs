//! The integration tests as one binary: one link instead of one per file.
//! Every file here needs its `mod` line (scripts/check.sh checks it).

mod common;

mod agent;
mod command;
mod hardware;
mod load_lines;
mod managed_values;
mod model_configs;
mod strata;
mod strata_models;
mod strata_output;
mod strata_process;
mod strata_switch;
mod strata_terminal;
mod tune;
mod update;

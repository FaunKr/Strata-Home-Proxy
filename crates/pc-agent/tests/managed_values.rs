//! Start script, programs and key come from `strata-managed.json` or else the
//! TOML. Code outside `managed.rs` reads them through `Managed::start`,
//! `processes` and `api_key`, which know both; setting a managed value stays
//! allowed.

use std::path::Path;

const FIELDS: &[&str] = &[
    ".strata_start",
    ".strata_args",
    ".strata_processes",
    ".strata_api_key",
];

/// Whether the field at the end of `before` is only set here.
fn sets(after: &str, next_line: &str) -> bool {
    let after = after.trim_start();
    (after.starts_with('=') && !after.starts_with("=="))
        || (after.is_empty() && next_line.trim_start().starts_with(".get_or_insert_with"))
}

#[test]
fn managed_values_are_read_through_their_accessors() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    for entry in std::fs::read_dir(&src).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if !name.ends_with(".rs") || name == "managed.rs" || name == "config.rs" {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        for (n, line) in lines.iter().enumerate() {
            let next = lines.get(n + 1).copied().unwrap_or_default();
            for field in FIELDS {
                let mut rest = *line;
                while let Some(at) = rest.find(field) {
                    let after = &rest[at + field.len()..];
                    let whole = !after.starts_with(|c: char| c.is_alphanumeric() || c == '_');
                    if whole && !sets(after, next) {
                        found.push(format!("{name}:{}: {}", n + 1, line.trim()));
                    }
                    rest = after;
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "read managed values through Managed::start, processes and api_key:\n{}",
        found.join("\n")
    );
}

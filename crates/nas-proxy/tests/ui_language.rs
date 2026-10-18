//! The web UI is English. Its texts come from the admin-ui crate and from the
//! proxy's `admin.rs`, which answers the UI's server functions. The PC
//! service's messages and the proxy's logs and API errors stay German, so
//! those files are not checked.

use std::path::{Path, PathBuf};

/// Umlauts, ß and short words that only German uses.
const GERMAN: &[&str] = &["ä", "ö", "ü", "Ä", "Ö", "Ü", "ß"];
const GERMAN_WORDS: &[&str] = &[
    "nicht",
    "und",
    "oder",
    "der",
    "das",
    "ist",
    "wird",
    "bitte",
    "kein",
    "keine",
    "Abgelehnt",
    "Einstellung",
    "Fehler",
    "Passwort",
];

fn ui_sources() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files: Vec<PathBuf> = std::fs::read_dir(root.join("../admin-ui/src"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "rs"))
        .collect();
    files.push(root.join("src/admin.rs"));
    files
}

/// The string literals on one line; good enough for the UI's texts, which
/// sit on one line each.
fn literals(line: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find('"') {
        let after = &rest[start + 1..];
        let mut end = None;
        let mut escaped = false;
        for (i, c) in after.char_indices() {
            match c {
                '\\' if !escaped => escaped = true,
                '"' if !escaped => {
                    end = Some(i);
                    break;
                }
                _ => escaped = false,
            }
        }
        let Some(end) = end else { break };
        found.push(&after[..end]);
        rest = &after[end + 1..];
    }
    found
}

fn is_german(text: &str) -> bool {
    GERMAN.iter().any(|c| text.contains(c))
        || text
            .split(|c: char| !c.is_alphanumeric())
            .any(|word| GERMAN_WORDS.contains(&word))
}

#[test]
fn the_web_ui_has_no_german_texts() {
    let mut found = Vec::new();
    for path in ui_sources() {
        let text = std::fs::read_to_string(&path).unwrap();
        for (n, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for literal in literals(line) {
                if is_german(literal) {
                    found.push(format!("{}:{}: {literal}", path.display(), n + 1));
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "Texte der Weboberfläche auf Englisch:\n{}",
        found.join("\n")
    );
}

#[test]
fn german_texts_are_recognised() {
    assert!(is_german("Abgelehnt: busy"));
    assert!(is_german("PC schläft"));
    assert!(is_german("unbekannte Einstellung WAKE_TIMEOUT_SECS"));
    assert!(!is_german("Refused: busy, next try in 3 min"));
    assert!(!is_german("The process may die while updating"));
    assert_eq!(literals(r#"x("a \"b\"", "c")"#), vec![r#"a \"b\""#, "c"]);
}

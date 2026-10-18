//! Files in `DATA_DIR` can hold secrets (password hash, session key, tokens),
//! so the proxy writes them only through `files::write_private`.

use std::path::Path;

const UNGUARDED: &[&str] = &["fs::write(", "File::create("];

#[test]
fn data_files_are_written_through_write_private() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = Vec::new();
    for entry in std::fs::read_dir(&src).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "rs") || path.ends_with("files.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for (n, line) in text.lines().enumerate() {
            if UNGUARDED.iter().any(|call| line.contains(call)) {
                found.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
            }
        }
    }
    assert!(
        found.is_empty(),
        "Dateien in DATA_DIR mit files::write_private schreiben:\n{}",
        found.join("\n")
    );
}

#[cfg(unix)]
#[test]
fn write_private_leaves_only_the_owner_able_to_read() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("strata-files-{}", std::process::id()));
    // A leftover from a crash, readable by everyone.
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("geheim.tmp"), "alt").unwrap();
    std::fs::set_permissions(
        dir.join("geheim.tmp"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();

    strata_nas_proxy::files::write_private(&dir, "geheim", b"neu").unwrap();

    let meta = std::fs::metadata(dir.join("geheim")).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    assert_eq!(std::fs::read(dir.join("geheim")).unwrap(), b"neu");
    assert!(!dir.join("geheim.tmp").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

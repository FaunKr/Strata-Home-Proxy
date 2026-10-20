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
        "Write files in DATA_DIR with files::write_private:\n{}",
        found.join("\n")
    );
}

#[cfg(unix)]
#[test]
fn write_private_leaves_only_the_owner_able_to_read() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("strata-files-{}", std::process::id()));
    // A leftover from a crash, readable by everyone, is never reused.
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("secret.tmp"), "old").unwrap();
    std::fs::set_permissions(
        dir.join("secret.tmp"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();

    strata_nas_proxy::files::write_private(&dir, "secret", b"new").unwrap();

    let meta = std::fs::metadata(dir.join("secret")).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    assert_eq!(std::fs::read(dir.join("secret")).unwrap(), b"new");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn simultaneous_writes_of_one_file_never_mix() {
    let dir = std::env::temp_dir().join(format!("strata-files-race-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let contents: Vec<Vec<u8>> = (0..8u8).map(|i| vec![b'a' + i; 64 * 1024]).collect();
    std::thread::scope(|s| {
        for bytes in &contents {
            let dir = &dir;
            s.spawn(move || {
                for _ in 0..50 {
                    strata_nas_proxy::files::write_private(dir, "secret", bytes).unwrap();
                }
            });
        }
    });
    let written = std::fs::read(dir.join("secret")).unwrap();
    assert!(contents.contains(&written), "contents mixed");
    std::fs::remove_dir_all(&dir).unwrap();
}

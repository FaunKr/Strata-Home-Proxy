//! Files in `DATA_DIR`.

use std::{
    io::Write,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

/// Numbers the temporary files, so simultaneous writes never share one.
static NEXT_TMP: AtomicU64 = AtomicU64::new(0);

/// Writes via a temporary file of its own, so a crash never leaves half a file
/// behind and two writes of the same file never mix. Only the owner may read
/// it, since some files hold secrets.
pub fn write_private(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let n = NEXT_TMP.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!("{name}.{}-{n}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    // A new file every time: a leftover from a crash is never reused.
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Private from the first byte.
        options.mode(0o600);
    }
    let written = options
        .open(&tmp)
        .and_then(|mut file| file.write_all(bytes))
        .and_then(|()| std::fs::rename(&tmp, dir.join(name)));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

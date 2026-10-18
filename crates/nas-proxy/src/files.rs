//! Files in `DATA_DIR`.

use std::{io::Write, path::Path};

/// Writes via a temporary file, so a crash never leaves half a file behind.
/// Only the owner may read it, since some files hold secrets.
pub fn write_private(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("{name}.tmp"));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        // Private from the first byte; set_permissions covers a leftover .tmp.
        options.mode(0o600);
        let mut file = options.open(&tmp)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(bytes)?;
    }
    #[cfg(not(unix))]
    options.open(&tmp)?.write_all(bytes)?;
    std::fs::rename(&tmp, dir.join(name))
}

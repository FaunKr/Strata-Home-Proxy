//! Files in `DATA_DIR`.

use std::path::Path;

/// Writes via a temporary file, so a crash never leaves half a file behind.
/// Only the owner may read it, since some files hold secrets.
pub fn write_private(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("{name}.tmp"));
    std::fs::write(&tmp, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, dir.join(name))
}

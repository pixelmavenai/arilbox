//! Atomic file writes for daemon-owned files (LXC config, metadata).

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

/// Writes `data` to `path` atomically: `<path>.tmp` in the same directory, fsync, rename,
/// fsync the directory. The file gets `mode`.
pub fn write_atomic(path: &Path, data: &[u8], mode: u32) -> std::io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("path has no parent directory"))?;
    let mut tmp_name = path.file_name().unwrap_or_default().to_owned();
    tmp_name.push(".tmp");
    let tmp = dir.join(tmp_name);

    // A stale temp file from a crash is ours; replace it.
    let _ = std::fs::remove_file(&tmp);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&tmp)?;
    file.write_all(data)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)?;
    std::fs::File::open(dir)?.sync_all()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn writes_and_replaces() {
        let dir = std::env::temp_dir().join(format!("arilbox-test-atomic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config");
        write_atomic(&path, b"one", 0o640).unwrap();
        write_atomic(&path, b"two", 0o640).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert!(!dir.join("config.tmp").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

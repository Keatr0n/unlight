use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process,
    sync::atomic::{AtomicU64, Ordering},
};

const DEFAULT_MODE: u32 = 0o644;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Durability {
    /// The file survives a power cut once this returns.
    Synced,
    /// The file may be lost in a crash, which is fine for anything that can be regenerated.
    Cached,
}

fn temp_path(target: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let number = NEXT.fetch_add(1, Ordering::Relaxed);
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    target.with_file_name(format!(".{name}.{}-{number}.tmp", process::id()))
}

/// Replaces `path` in one step, so readers see the old file or the new one and never a
/// partial write. A symlink stays a symlink, and the file keeps its permissions.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8], durability: Durability) -> io::Result<()> {
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let mode = fs::metadata(&target).map_or(DEFAULT_MODE, |existing| existing.permissions().mode());
    let temp = temp_path(&target);
    let result = write_then_rename(&temp, &target, bytes, mode, durability);
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn write_then_rename(
    temp: &Path,
    target: &Path,
    bytes: &[u8],
    mode: u32,
    durability: Durability,
) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).mode(mode).open(temp)?;
    file.write_all(bytes)?;
    if durability == Durability::Synced {
        file.sync_all()?;
    }
    fs::rename(temp, target)?;
    if durability == Durability::Synced
        && let Some(directory) = target.parent()
    {
        File::open(directory)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn entries(directory: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn replaces_a_file_and_leaves_no_temp_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.xmp");
        write_atomic(&path, b"one", Durability::Synced).unwrap();
        write_atomic(&path, b"two", Durability::Cached).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert_eq!(entries(dir.path()), ["a.xmp"]);
    }

    #[test]
    fn keeps_existing_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.xmp");
        fs::write(&path, b"one").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        write_atomic(&path, b"two", Durability::Synced).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn writes_through_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.xmp");
        let link = dir.path().join("link.xmp");
        fs::write(&real, b"one").unwrap();
        symlink(&real, &link).unwrap();
        write_atomic(&link, b"two", Durability::Synced).unwrap();
        assert_eq!(fs::read(&real).unwrap(), b"two");
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
    }

    #[test]
    fn a_failed_write_keeps_the_original() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.xmp");
        fs::write(&path, b"one").unwrap();
        assert!(
            write_atomic(&dir.path().join("no-such-dir/b.xmp"), b"two", Durability::Synced)
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), b"one");
    }
}

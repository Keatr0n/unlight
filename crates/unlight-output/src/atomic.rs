//! Files appear under their final name complete or not at all: bytes go to a hidden file in
//! the destination directory, which is renamed once they are safely on disk.

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, BufWriter, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

const WRITE_BUFFER_BYTES: usize = 1 << 20;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollisionPolicy {
    /// Leave the existing file alone and report [`Error::Skipped`].
    Skip,
    Overwrite,
    /// Add `-1`, `-2` and so on until the name is free.
    #[default]
    Unique,
}

fn io_error(path: &Path) -> impl FnOnce(io::Error) -> Error {
    let path = path.to_owned();
    move |source| Error::Io { path, source }
}

/// A partly written file, deleted unless it gets published.
struct TempFile {
    path: PathBuf,
}

impl TempFile {
    fn create(directory: &Path) -> Result<(Self, File)> {
        loop {
            let serial = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = directory.join(format!(".unlight-{}-{serial}.tmp", process::id()));
            match File::create_new(&path) {
                Ok(file) => return Ok((Self { path }, file)),
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                Err(error) => return Err(io_error(&path)(error)),
            }
        }
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Where a file should end up.
pub(crate) struct Destination<'a> {
    pub directory: &'a Path,
    pub stem: &'a str,
    pub extension: &'a str,
    pub collision: CollisionPolicy,
}

impl Destination<'_> {
    fn path(&self, suffix: Option<u32>) -> PathBuf {
        let name = match suffix {
            Some(number) => format!("{}-{number}.{}", self.stem, self.extension),
            None => format!("{}.{}", self.stem, self.extension),
        };
        self.directory.join(name)
    }

    /// Fails with [`Error::Skipped`] when the work would be thrown away.
    pub fn check_skip(&self) -> Result<()> {
        let path = self.path(None);
        if self.collision == CollisionPolicy::Skip && path.exists() {
            return Err(Error::Skipped { path });
        }
        Ok(())
    }

    /// Takes the name atomically. A failure with `AlreadyExists` means someone else has it.
    fn claim(path: &Path) -> io::Result<()> {
        File::create_new(path).map(drop)
    }

    fn move_into_place(temp: &TempFile, path: &Path) -> Result<()> {
        fs::rename(&temp.path, path).map_err(io_error(path))
    }

    fn claim_and_move(temp: &TempFile, path: &Path) -> Result<()> {
        Self::move_into_place(temp, path).inspect_err(|_| {
            let _ = fs::remove_file(path);
        })
    }

    fn publish(&self, temp: &TempFile) -> Result<PathBuf> {
        match self.collision {
            CollisionPolicy::Overwrite => {
                let path = self.path(None);
                Self::move_into_place(temp, &path)?;
                Ok(path)
            }
            CollisionPolicy::Skip => {
                let path = self.path(None);
                match Self::claim(&path) {
                    Ok(()) => Self::claim_and_move(temp, &path).map(|()| path),
                    Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                        Err(Error::Skipped { path })
                    }
                    Err(error) => Err(io_error(&path)(error)),
                }
            }
            CollisionPolicy::Unique => self.publish_unique(temp),
        }
    }

    fn publish_unique(&self, temp: &TempFile) -> Result<PathBuf> {
        let candidates = std::iter::once(None).chain((1..=u32::MAX).map(Some));
        for suffix in candidates {
            let path = self.path(suffix);
            match Self::claim(&path) {
                Ok(()) => return Self::claim_and_move(temp, &path).map(|()| path),
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                Err(error) => return Err(io_error(&path)(error)),
            }
        }
        Err(io_error(self.directory)(ErrorKind::AlreadyExists.into()))
    }

    /// Runs `write` against a temporary file, then gives it its final name.
    pub fn write(&self, write: impl FnOnce(&mut dyn Write) -> Result<()>) -> Result<PathBuf> {
        let (temp, file) = TempFile::create(self.directory)?;
        let mut buffered = BufWriter::with_capacity(WRITE_BUFFER_BYTES, file);
        write(&mut buffered)?;
        buffered.flush().map_err(io_error(&temp.path))?;
        let file = buffered
            .into_inner()
            .map_err(|error| io_error(&temp.path)(error.into_error()))?;
        file.sync_all().map_err(io_error(&temp.path))?;

        let path = self.publish(&temp)?;
        File::open(self.directory)
            .and_then(|directory| directory.sync_all())
            .map_err(io_error(self.directory))?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn destination(directory: &Path, collision: CollisionPolicy) -> Destination<'_> {
        Destination {
            directory,
            stem: "photo",
            extension: "jpg",
            collision,
        }
    }

    fn write_text(destination: &Destination<'_>, text: &str) -> Result<PathBuf> {
        destination.write(|out| {
            out.write_all(text.as_bytes())
                .map_err(io_error(Path::new("test")))
        })
    }

    fn names(directory: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn writes_the_file_and_leaves_nothing_else() {
        let directory = tempdir().unwrap();
        let path = write_text(
            &destination(directory.path(), CollisionPolicy::Unique),
            "data",
        )
        .unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "data");
        assert_eq!(names(directory.path()), ["photo.jpg"]);
    }

    #[test]
    fn overwrite_replaces_the_file() {
        let directory = tempdir().unwrap();
        let target = destination(directory.path(), CollisionPolicy::Overwrite);
        write_text(&target, "old").unwrap();
        let path = write_text(&target, "new").unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), "new");
        assert_eq!(names(directory.path()), ["photo.jpg"]);
    }

    #[test]
    fn skip_keeps_the_original_and_says_so() {
        let directory = tempdir().unwrap();
        let target = destination(directory.path(), CollisionPolicy::Skip);
        let first = write_text(&target, "old").unwrap();
        let error = write_text(&target, "new").unwrap_err();
        assert!(
            matches!(&error, Error::Skipped { path } if *path == first),
            "{error}"
        );
        assert_eq!(fs::read_to_string(first).unwrap(), "old");
        assert_eq!(names(directory.path()), ["photo.jpg"]);
    }

    #[test]
    fn unique_counts_up() {
        let directory = tempdir().unwrap();
        let target = destination(directory.path(), CollisionPolicy::Unique);
        for text in ["a", "b", "c"] {
            write_text(&target, text).unwrap();
        }
        assert_eq!(
            names(directory.path()),
            ["photo-1.jpg", "photo-2.jpg", "photo.jpg"]
        );
        assert_eq!(
            fs::read_to_string(directory.path().join("photo-2.jpg")).unwrap(),
            "c"
        );
    }

    #[test]
    fn concurrent_unique_writers_never_share_a_name() {
        let directory = tempdir().unwrap();
        let paths: Vec<PathBuf> = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..16)
                .map(|i| {
                    let directory = directory.path();
                    scope.spawn(move || {
                        write_text(
                            &destination(directory, CollisionPolicy::Unique),
                            &i.to_string(),
                        )
                    })
                })
                .collect();
            threads
                .into_iter()
                .map(|thread| thread.join().unwrap().unwrap())
                .collect()
        });
        let mut contents: Vec<u32> = paths
            .iter()
            .map(|path| fs::read_to_string(path).unwrap().parse().unwrap())
            .collect();
        contents.sort_unstable();
        assert_eq!(contents, (0..16).collect::<Vec<u32>>());
        assert_eq!(names(directory.path()).len(), 16);
    }

    #[test]
    fn a_failed_encoder_leaves_no_trace() {
        let directory = tempdir().unwrap();
        let target = destination(directory.path(), CollisionPolicy::Unique);
        let result = target.write(|out| {
            out.write_all(b"half a file")
                .map_err(io_error(Path::new("test")))?;
            Err(Error::Encode {
                format: "test",
                message: "boom".into(),
            })
        });
        assert!(result.is_err());
        assert_eq!(names(directory.path()), Vec::<String>::new());
    }

    #[test]
    fn a_failed_encoder_keeps_the_previous_export() {
        let directory = tempdir().unwrap();
        let target = destination(directory.path(), CollisionPolicy::Overwrite);
        let path = write_text(&target, "good").unwrap();
        let result = target.write(|_| {
            Err(Error::Encode {
                format: "test",
                message: "boom".into(),
            })
        });
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "good");
        assert_eq!(names(directory.path()), ["photo.jpg"]);
    }

    #[test]
    fn a_failed_rename_leaves_no_temp_file() {
        let directory = tempdir().unwrap();
        fs::create_dir(directory.path().join("photo.jpg")).unwrap();
        let target = destination(directory.path(), CollisionPolicy::Overwrite);
        let error = write_text(&target, "data").unwrap_err();
        assert!(matches!(error, Error::Io { .. }), "{error}");
        assert_eq!(names(directory.path()), ["photo.jpg"]);
    }

    #[test]
    fn a_missing_directory_is_an_error_naming_it() {
        let directory = tempdir().unwrap();
        let missing = directory.path().join("nope");
        let error = write_text(&destination(&missing, CollisionPolicy::Unique), "x").unwrap_err();
        assert!(error.to_string().contains("nope"), "{error}");
    }

    #[test]
    fn check_skip_only_fires_for_skip() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("photo.jpg"), "x").unwrap();
        assert!(
            destination(directory.path(), CollisionPolicy::Skip)
                .check_skip()
                .is_err()
        );
        assert!(
            destination(directory.path(), CollisionPolicy::Overwrite)
                .check_skip()
                .is_ok()
        );
        assert!(
            destination(directory.path(), CollisionPolicy::Unique)
                .check_skip()
                .is_ok()
        );
    }

    #[test]
    #[cfg(unix)]
    fn overwrite_replaces_a_symlink_rather_than_following_it() {
        let directory = tempdir().unwrap();
        let elsewhere = directory.path().join("elsewhere");
        fs::write(&elsewhere, "precious").unwrap();
        std::os::unix::fs::symlink(&elsewhere, directory.path().join("photo.jpg")).unwrap();
        write_text(
            &destination(directory.path(), CollisionPolicy::Overwrite),
            "new",
        )
        .unwrap();
        assert_eq!(fs::read_to_string(elsewhere).unwrap(), "precious");
    }
}

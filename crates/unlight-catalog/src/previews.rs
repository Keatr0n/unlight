//! On-disk cache of decoded previews. Files are JPEGs named by fingerprint, published by
//! rename so a reader never sees a partial one, and any file that fails to decode is simply
//! rebuilt. Eviction may delete a file under a reader, which then counts as a miss.
//!
//! Decoding a full-size embedded JPEG takes around 100 ms, so the fast path depends on the
//! file already being here: generate neighbours of the current photo ahead of time.

use crate::{
    Error, Fingerprint, Result,
    error::io_error,
    files::{Durability, write_atomic},
    image::{
        RgbaImage, apply_orientation, decode_jpeg, decode_preview, encode_jpeg, fit_long_edge,
    },
};
use std::{
    collections::{BTreeMap, HashMap},
    env,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    hash::{DefaultHasher, Hash, Hasher},
    io::Read,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard, PoisonError},
    time::SystemTime,
};
use unlight_raw::load_preview_sized;

/// Bump when the stored format or rendering changes. Old entries are then just ignored.
const FORMAT_DIRECTORY: &str = "previews-v1";
const GENERATION_LOCKS: usize = 64;
pub const DEFAULT_CAPACITY_BYTES: u64 = 8 << 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PreviewSize {
    /// About 512 px on the long edge.
    Grid,
    /// About 2048 px on the long edge.
    Loupe,
}

impl PreviewSize {
    const ALL: [Self; 2] = [Self::Grid, Self::Loupe];

    pub fn long_edge(self) -> u32 {
        match self {
            Self::Grid => 512,
            Self::Loupe => 2048,
        }
    }

    fn directory(self) -> &'static str {
        match self {
            Self::Grid => "grid",
            Self::Loupe => "loupe",
        }
    }
}

/// Which photo to preview. [`crate::LibraryRow::preview_source`] builds one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreviewSource<'a> {
    pub fingerprint: Fingerprint,
    pub path: &'a Path,
    /// LibRaw flip code.
    pub orientation: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Key {
    fingerprint: Fingerprint,
    size: PreviewSize,
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    bytes: u64,
    last_used: u64,
}

/// In-memory LRU bookkeeping. File times carry the order across restarts.
#[derive(Debug, Default)]
struct Index {
    entries: HashMap<Key, Entry>,
    by_recency: BTreeMap<u64, Key>,
    clock: u64,
    total_bytes: u64,
}

impl Index {
    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn touch(&mut self, key: Key) {
        let tick = self.tick();
        if let Some(entry) = self.entries.get_mut(&key) {
            self.by_recency.remove(&entry.last_used);
            entry.last_used = tick;
            self.by_recency.insert(tick, key);
        }
    }

    fn insert(&mut self, key: Key, bytes: u64) {
        self.remove(key);
        let last_used = self.tick();
        self.entries.insert(key, Entry { bytes, last_used });
        self.by_recency.insert(last_used, key);
        self.total_bytes += bytes;
    }

    fn remove(&mut self, key: Key) {
        if let Some(entry) = self.entries.remove(&key) {
            self.by_recency.remove(&entry.last_used);
            self.total_bytes -= entry.bytes;
        }
    }

    fn oldest(&self) -> Option<Key> {
        self.by_recency.values().next().copied()
    }
}

pub struct PreviewCache {
    root: PathBuf,
    capacity_bytes: u64,
    index: Mutex<Index>,
    /// Stops several threads rendering the same preview at once.
    generation_locks: Vec<Mutex<()>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `$XDG_CACHE_HOME/unlight`, or `~/.cache/unlight` when that is unset.
pub fn default_cache_dir() -> Result<PathBuf> {
    cache_dir_from(env::var_os("XDG_CACHE_HOME"), env::var_os("HOME"))
}

fn cache_dir_from(xdg_cache_home: Option<OsString>, home: Option<OsString>) -> Result<PathBuf> {
    let xdg = xdg_cache_home.map(PathBuf::from).filter(|path| path.is_absolute());
    let base = xdg.or_else(|| home.map(|home| PathBuf::from(home).join(".cache")));
    Ok(base.ok_or(Error::NoCacheDir)?.join("unlight"))
}

impl PreviewCache {
    /// Opens the cache in `directory`, listing what's already there.
    pub fn open(directory: &Path, capacity_bytes: u64) -> Result<Self> {
        let root = directory.join(FORMAT_DIRECTORY);
        for size in PreviewSize::ALL {
            let path = root.join(size.directory());
            fs::create_dir_all(&path).map_err(io_error(&path))?;
        }
        let cache = Self {
            index: Mutex::new(load_index(&root)),
            root,
            capacity_bytes,
            generation_locks: (0..GENERATION_LOCKS).map(|_| Mutex::new(())).collect(),
        };
        cache.evict(None);
        Ok(cache)
    }

    /// Opens the cache in the default location.
    pub fn open_default() -> Result<Self> {
        Self::open(&default_cache_dir()?, DEFAULT_CAPACITY_BYTES)
    }

    /// Returns the preview, rendering and storing it first if needed. Safe to call from many
    /// threads, also for the same photo.
    pub fn get(&self, source: &PreviewSource, size: PreviewSize) -> Result<RgbaImage> {
        let key = Key { fingerprint: source.fingerprint, size };
        if let Some(image) = self.read_entry(key) {
            return Ok(image);
        }
        let _generating = lock(self.generation_lock(key));
        if let Some(image) = self.read_entry(key) {
            return Ok(image);
        }
        let image = render(source, size)?;
        // A full disk shouldn't stop the photo from showing, it just stays uncached.
        let _ = self.store(key, &image);
        Ok(image)
    }

    /// Returns the preview only if it's already on disk, for callers that must not wait.
    pub fn get_cached(&self, fingerprint: Fingerprint, size: PreviewSize) -> Option<RgbaImage> {
        self.read_entry(Key { fingerprint, size })
    }

    /// Bytes of previews currently held.
    pub fn total_bytes(&self) -> u64 {
        lock(&self.index).total_bytes
    }

    fn generation_lock(&self, key: Key) -> &Mutex<()> {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        &self.generation_locks[hasher.finish() as usize % GENERATION_LOCKS]
    }

    fn entry_path(&self, key: Key) -> PathBuf {
        entry_path(&self.root, key)
    }

    fn read_entry(&self, key: Key) -> Option<RgbaImage> {
        let path = self.entry_path(key);
        let Some(bytes) = read_and_touch(&path) else {
            // Checked under the lock, so a writer publishing this entry right now isn't undone.
            let mut index = lock(&self.index);
            if !path.exists() {
                index.remove(key);
            }
            return None;
        };
        match decode_jpeg(&bytes, true) {
            Ok(image) => {
                lock(&self.index).touch(key);
                Some(image)
            }
            Err(_) => {
                lock(&self.index).remove(key);
                let _ = fs::remove_file(&path);
                None
            }
        }
    }

    fn store(&self, key: Key, image: &RgbaImage) -> Result<()> {
        let path = self.entry_path(key);
        let directory = path.parent().unwrap_or(&self.root);
        fs::create_dir_all(directory).map_err(io_error(directory))?;
        let bytes = encode_jpeg(image)?;
        write_atomic(&path, &bytes, Durability::Cached).map_err(io_error(&path))?;
        lock(&self.index).insert(key, bytes.len() as u64);
        self.evict(Some(key));
        Ok(())
    }

    /// Drops the least recently used entries until the cache fits, sparing `keep`.
    fn evict(&self, keep: Option<Key>) {
        loop {
            let victim = {
                let mut index = lock(&self.index);
                if index.total_bytes <= self.capacity_bytes {
                    return;
                }
                let Some(oldest) = index.oldest() else { return };
                if Some(oldest) == keep {
                    return;
                }
                index.remove(oldest);
                oldest
            };
            let _ = fs::remove_file(self.entry_path(victim));
        }
    }
}

fn render(source: &PreviewSource, size: PreviewSize) -> Result<RgbaImage> {
    let preview = load_preview_sized(source.path, size.long_edge())?;
    let image = fit_long_edge(decode_preview(&preview)?, size.long_edge())?;
    Ok(apply_orientation(image, source.orientation))
}

fn entry_path(root: &Path, key: Key) -> PathBuf {
    let name = key.fingerprint.to_hex();
    root.join(key.size.directory()).join(&name[..2]).join(format!("{name}.jpg"))
}

/// Reading and touching together keeps a hit to a single open.
fn read_and_touch(path: &Path) -> Option<Vec<u8>> {
    let mut file = OpenOptions::new().read(true).write(true).open(path).ok()?;
    let mut bytes = Vec::with_capacity(file.metadata().ok()?.len() as usize);
    file.read_to_end(&mut bytes).ok()?;
    let _ = File::set_modified(&file, SystemTime::now());
    Some(bytes)
}

fn parse_fingerprint(stem: &str) -> Option<Fingerprint> {
    if stem.len() != 32 {
        return None;
    }
    let mut bytes = [0; 16];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(stem.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(Fingerprint(bytes))
}

/// Lists entries oldest first, removing temp files a crash left behind.
fn load_index(root: &Path) -> Index {
    let mut found = Vec::new();
    for size in PreviewSize::ALL {
        let shards = fs::read_dir(root.join(size.directory())).into_iter().flatten().flatten();
        for shard in shards {
            for file in fs::read_dir(shard.path()).into_iter().flatten().flatten() {
                let path = file.path();
                let stem = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or_default();
                let Some(fingerprint) = parse_fingerprint(stem) else {
                    let _ = fs::remove_file(&path);
                    continue;
                };
                if let Ok(metadata) = file.metadata() {
                    let used = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    found.push((used, Key { fingerprint, size }, metadata.len()));
                }
            }
        }
    }
    found.sort_by_key(|&(used, _, _)| used);
    let mut index = Index::default();
    for (_, key, bytes) in found {
        index.insert(key, bytes);
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xdg_cache_home_wins_over_home() {
        let dir = cache_dir_from(Some("/x/cache".into()), Some("/home/a".into())).unwrap();
        assert_eq!(dir, Path::new("/x/cache/unlight"));
    }

    #[test]
    fn home_is_the_fallback() {
        let dir = cache_dir_from(None, Some("/home/a".into())).unwrap();
        assert_eq!(dir, Path::new("/home/a/.cache/unlight"));
    }

    #[test]
    fn a_relative_xdg_path_is_ignored_as_the_spec_requires() {
        let dir = cache_dir_from(Some("relative".into()), Some("/home/a".into())).unwrap();
        assert_eq!(dir, Path::new("/home/a/.cache/unlight"));
    }

    #[test]
    fn no_home_at_all_is_an_error() {
        assert!(matches!(cache_dir_from(None, None), Err(Error::NoCacheDir)));
    }

    #[test]
    fn a_fingerprint_survives_being_a_file_name() {
        let fingerprint = Fingerprint([0xab; 16]);
        assert_eq!(parse_fingerprint(&fingerprint.to_hex()), Some(fingerprint));
        assert_eq!(parse_fingerprint("nope"), None);
    }

    #[test]
    fn the_index_tracks_recency_and_size() {
        let key = |n| Key { fingerprint: Fingerprint([n; 16]), size: PreviewSize::Grid };
        let mut index = Index::default();
        index.insert(key(1), 10);
        index.insert(key(2), 20);
        index.touch(key(1));
        assert_eq!(index.oldest(), Some(key(2)));
        assert_eq!(index.total_bytes, 30);
        index.insert(key(2), 5);
        assert_eq!(index.total_bytes, 15);
        assert_eq!(index.oldest(), Some(key(1)));
    }
}

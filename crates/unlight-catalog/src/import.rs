//! Folder scanning. A scan walks the tree, compares it with the catalog to find the little
//! that needs reading, reads that on the rayon pool, and commits the results in batches on
//! the calling thread. Everything except a database failure is recorded and survived.

use crate::{
    Catalog, Error, Fingerprint, FolderId, PhotoId, PhotoStatus, Result,
    clause::subtree_range,
    error::io_error,
    model::FileStamp,
    photos::set_status,
    sidecars::{apply_sidecar, is_dirty, record_conflict, record_synced},
    xmp::{XmpData, read_sidecar, sidecar_candidates},
};
use rayon::prelude::*;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs::{self, File},
    io,
    os::unix::{ffi::OsStrExt, fs::FileExt},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};
use unlight_raw::{PhotoInfo, read_metadata};
use walkdir::{DirEntry, WalkDir};
use xxhash_rust::xxh3::Xxh3;

/// Lowercase file extensions treated as raw photos.
pub const RAW_EXTENSIONS: &[&str] = &[
    "3fr", "ari", "arw", "bay", "braw", "cr2", "cr3", "crw", "dcr", "dcs", "dng", "drf", "eip",
    "erf", "fff", "gpr", "iiq", "k25", "kdc", "mdc", "mef", "mos", "mrw", "nef", "nrw", "orf",
    "pef", "ptx", "pxn", "raf", "raw", "rw2", "rwl", "rwz", "sr2", "srf", "srw", "ulr", "x3f",
];

const BATCH_SIZE: usize = 512;
const DISCOVERY_PROGRESS_INTERVAL: usize = 256;
const FINGERPRINT_CHUNK: u64 = 64 * 1024;

/// Cancels a scan from another thread. Clones share the same flag.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanPhase {
    /// Walking folders. `done` counts raw files found and `total` isn't known yet.
    Discovering,
    /// Reading new and changed files.
    Reading,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanProgress {
    pub phase: ScanPhase,
    pub done: usize,
    pub total: usize,
}

/// A path the scan had to skip, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanFailure {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanReport {
    /// Raw files found on disk.
    pub discovered: usize,
    pub added: usize,
    /// Known paths whose file changed.
    pub changed: usize,
    pub unchanged: usize,
    /// Files that turned up at a new path, keeping their edits.
    pub moved: usize,
    /// Photos newly marked missing.
    pub missing: usize,
    /// Missing photos found back at their old path.
    pub restored: usize,
    /// Files newly recorded as unsupported.
    pub unsupported: usize,
    pub sidecars_read: usize,
    pub sidecar_conflicts: usize,
    pub failures: Vec<ScanFailure>,
    /// A cancelled scan keeps what it committed and marks nothing missing.
    pub cancelled: bool,
}

struct FoundFile {
    path: PathBuf,
    stamp: FileStamp,
}

#[derive(Clone)]
struct FoundSidecar {
    path: PathBuf,
    stamp: FileStamp,
}

#[derive(Default)]
struct Walked {
    files: Vec<FoundFile>,
    sidecars: HashMap<PathBuf, FileStamp>,
    /// Folders we couldn't list. What's inside them is unknown, not missing.
    unreadable: Vec<PathBuf>,
    failures: Vec<ScanFailure>,
}

struct KnownPhoto {
    id: PhotoId,
    stamp: FileStamp,
    status: PhotoStatus,
    sidecar: Option<FileStamp>,
}

enum Work {
    Read { existing: Option<PhotoId> },
    Keep { photo: PhotoId, restore: bool },
}

struct Task {
    file: FoundFile,
    work: Work,
    sidecar: Option<FoundSidecar>,
}

struct FileRead {
    fingerprint: Fingerprint,
    info: std::result::Result<PhotoInfo, String>,
}

struct Outcome {
    file: FoundFile,
    work: Work,
    read: Option<io::Result<FileRead>>,
    sidecar: Option<(FoundSidecar, Result<XmpData>)>,
}

/// What batches share while storing a scan's results.
struct Store<'a> {
    folders: HashMap<PathBuf, FolderId>,
    discovered: HashSet<PathBuf>,
    moved: HashSet<PhotoId>,
    report: &'a mut ScanReport,
}

/// Skips dot folders such as `.Trash` and AppleDouble `._*` files, but never the scan root.
fn is_hidden(entry: &DirEntry) -> bool {
    entry.depth() > 0 && entry.file_name().as_bytes().starts_with(b".")
}

fn extension_of(path: &Path) -> Option<String> {
    Some(path.extension()?.to_str()?.to_ascii_lowercase())
}

impl Walked {
    fn add_entry(&mut self, entry: &DirEntry) {
        let path = entry.path();
        let Some(extension) = extension_of(path) else { return };
        let is_sidecar = extension == "xmp";
        if !is_sidecar && !RAW_EXTENSIONS.contains(&extension.as_str()) {
            return;
        }
        let stamp = match entry.metadata() {
            Ok(metadata) => FileStamp::of(&metadata),
            Err(error) => return self.add_failure(path, &error),
        };
        if is_sidecar {
            self.sidecars.insert(path.to_owned(), stamp);
        } else if path.to_str().is_some() {
            self.files.push(FoundFile { path: path.to_owned(), stamp });
        } else {
            self.add_failure(path, &"the path isn't valid UTF-8");
        }
    }

    fn add_failure(&mut self, path: &Path, error: &dyn std::fmt::Display) {
        self.failures.push(ScanFailure { path: path.to_owned(), message: error.to_string() });
    }

    fn add_walk_error(&mut self, error: &walkdir::Error) {
        let path = error.path().unwrap_or(Path::new("")).to_owned();
        let vanished = error.io_error().is_some_and(|io| io.kind() == io::ErrorKind::NotFound);
        if !vanished && error.loop_ancestor().is_none() {
            self.unreadable.push(path.clone());
        }
        self.add_failure(&path, error);
    }
}

fn walk(root: &Path, cancel: &CancelToken, on_progress: &mut impl FnMut(ScanProgress)) -> Walked {
    let mut walked = Walked::default();
    let entries =
        WalkDir::new(root).follow_links(true).into_iter().filter_entry(|entry| !is_hidden(entry));
    for entry in entries {
        if cancel.is_cancelled() {
            break;
        }
        match entry {
            Ok(entry) if entry.file_type().is_file() => walked.add_entry(&entry),
            Ok(_) => {}
            Err(error) => walked.add_walk_error(&error),
        }
        let done = walked.files.len();
        if done > 0 && done.is_multiple_of(DISCOVERY_PROGRESS_INTERVAL) {
            on_progress(ScanProgress { phase: ScanPhase::Discovering, done, total: 0 });
        }
    }
    walked
}

fn path_text(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| Error::NonUtf8Path(path.to_owned()))
}

/// Gives every folder holding raws a row, with parents linked. The scan root hangs off the
/// nearest folder the catalog already knows.
fn ensure_folders(
    tx: &Transaction,
    root: &Path,
    files: &[FoundFile],
) -> Result<HashMap<PathBuf, FolderId>> {
    let mut directories: BTreeSet<&Path> = BTreeSet::new();
    for file in files {
        let ancestors = file.path.ancestors().skip(1).take_while(|path| path.starts_with(root));
        directories.extend(ancestors);
    }
    let mut upsert = tx.prepare_cached(
        "INSERT INTO folders (path, parent_id) VALUES (?1, ?2)
         ON CONFLICT (path) DO UPDATE SET parent_id = excluded.parent_id
         RETURNING id",
    )?;
    let mut folders = HashMap::new();
    for directory in directories {
        let parent = match directory.parent() {
            Some(parent) if directory != root => folders.get(parent).copied(),
            _ => nearest_known_folder(tx, directory)?,
        };
        let id: FolderId =
            upsert.query_row(params![path_text(directory)?, parent], |row| row.get(0))?;
        folders.insert(directory.to_owned(), id);
    }
    Ok(folders)
}

fn nearest_known_folder(conn: &Connection, path: &Path) -> Result<Option<FolderId>> {
    let mut lookup = conn.prepare_cached("SELECT id FROM folders WHERE path = ?1")?;
    for ancestor in path.ancestors().skip(1) {
        let found = lookup.query_row([path_text(ancestor)?], |row| row.get(0)).optional()?;
        if found.is_some() {
            return Ok(found);
        }
    }
    Ok(None)
}

fn load_known(conn: &Connection, root: &Path) -> Result<HashMap<PathBuf, KnownPhoto>> {
    let root = path_text(root)?;
    let (low, high) = subtree_range(root);
    let mut statement = conn.prepare_cached(
        "SELECT p.id, f.path, p.file_name, p.size, p.mtime_ns, p.status, s.size, s.mtime_ns
         FROM photos p
         JOIN folders f ON f.id = p.folder_id
         LEFT JOIN sidecars s ON s.photo_id = p.id
         WHERE f.path = ?1 OR (f.path >= ?2 AND f.path < ?3)",
    )?;
    let rows = statement.query_map(params![root, low, high], |row| {
        let folder: String = row.get(1)?;
        let file_name: String = row.get(2)?;
        let sidecar_size: Option<i64> = row.get(6)?;
        let sidecar_mtime: Option<i64> = row.get(7)?;
        let known = KnownPhoto {
            id: row.get(0)?,
            stamp: FileStamp { size: row.get(3)?, mtime_ns: row.get(4)? },
            status: row.get(5)?,
            sidecar: sidecar_size
                .zip(sidecar_mtime)
                .map(|(size, mtime_ns)| FileStamp { size, mtime_ns }),
        };
        Ok((Path::new(&folder).join(file_name), known))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn find_sidecar(raw: &Path, sidecars: &HashMap<PathBuf, FileStamp>) -> Option<FoundSidecar> {
    sidecar_candidates(raw).into_iter().find_map(|path| {
        let stamp = *sidecars.get(&path)?;
        Some(FoundSidecar { path, stamp })
    })
}

/// Decides what each file needs. Whatever stays in `known` afterwards wasn't found on disk.
fn plan_tasks(
    walked: &mut Walked,
    known: &mut HashMap<PathBuf, KnownPhoto>,
    report: &mut ScanReport,
) -> Vec<Task> {
    let sidecars = std::mem::take(&mut walked.sidecars);
    let mut tasks = Vec::new();
    for file in std::mem::take(&mut walked.files) {
        let sidecar = find_sidecar(&file.path, &sidecars);
        match known.remove(&file.path) {
            None => tasks.push(Task { file, work: Work::Read { existing: None }, sidecar }),
            Some(photo) if photo.stamp != file.stamp => {
                let work = Work::Read { existing: Some(photo.id) };
                tasks.push(Task { file, work, sidecar: changed_sidecar(sidecar, &photo) });
            }
            Some(photo) => {
                let restore = photo.status == PhotoStatus::Missing;
                let sidecar = changed_sidecar(sidecar, &photo);
                if restore || sidecar.is_some() {
                    tasks.push(Task {
                        file,
                        work: Work::Keep { photo: photo.id, restore },
                        sidecar,
                    });
                } else {
                    report.unchanged += 1;
                }
            }
        }
    }
    tasks
}

fn changed_sidecar(sidecar: Option<FoundSidecar>, photo: &KnownPhoto) -> Option<FoundSidecar> {
    sidecar.filter(|found| Some(found.stamp) != photo.sidecar)
}

fn fingerprint(path: &Path) -> io::Result<Fingerprint> {
    let file = File::open(path)?;
    let size = file.metadata()?.len();
    let mut hasher = Xxh3::new();
    hasher.update(&size.to_le_bytes());
    let head_length = size.min(FINGERPRINT_CHUNK) as usize;
    let mut chunk = vec![0; head_length];
    file.read_exact_at(&mut chunk, 0)?;
    hasher.update(&chunk);
    let tail_length = (size - head_length as u64).min(FINGERPRINT_CHUNK);
    let tail = &mut chunk[..tail_length as usize];
    file.read_exact_at(tail, size - tail_length)?;
    hasher.update(tail);
    Ok(Fingerprint(hasher.digest128().to_le_bytes()))
}

fn read_file(path: &Path) -> io::Result<FileRead> {
    Ok(FileRead {
        fingerprint: fingerprint(path)?,
        info: read_metadata(path).map_err(|error| error.to_string()),
    })
}

fn run_task(task: Task) -> Outcome {
    let read = matches!(task.work, Work::Read { .. }).then(|| read_file(&task.file.path));
    let sidecar = task.sidecar.map(|found| {
        let data = read_sidecar(&found.path);
        (found, data)
    });
    Outcome { file: task.file, work: task.work, read, sidecar }
}

/// Runs on the rayon pool. A failed send means the writer gave up, so stop too.
fn run_tasks(tasks: Vec<Task>, sender: mpsc::SyncSender<Outcome>, cancel: &CancelToken) {
    let _ = tasks.into_par_iter().try_for_each_with(sender, |sender, task| {
        if cancel.is_cancelled() {
            return Err(());
        }
        sender.send(run_task(task)).map_err(|_| ())
    });
}

/// Widens through the shortest decimal form. A plain cast would store f/2.8 as
/// 2.799999952, and a rule asking for exactly 2.8 would never match.
fn exif_value(value: f32) -> Option<f64> {
    if value > 0.0 { value.to_string().parse().ok() } else { None }
}

fn store_metadata(tx: &Transaction, id: PhotoId, read: &FileRead) -> Result<()> {
    let info = match &read.info {
        Ok(info) => info,
        Err(reason) => {
            tx.prepare_cached("UPDATE photos SET status = ?2, status_detail = ?3 WHERE id = ?1")?
                .execute(params![id, PhotoStatus::Unsupported, reason])?;
            return Ok(());
        }
    };
    let meta = &info.metadata;
    tx.prepare_cached(
        "UPDATE photos SET status = ?2, status_detail = NULL, camera = ?3, lens = ?4, iso = ?5,
             shutter = ?6, aperture = ?7, focal_length = ?8, capture_time = ?9, width = ?10,
             height = ?11, orientation = ?12
         WHERE id = ?1",
    )?
    .execute(params![
        id,
        PhotoStatus::Available,
        meta.camera().trim(),
        meta.lens,
        exif_value(meta.iso),
        exif_value(meta.shutter),
        exif_value(meta.aperture),
        exif_value(meta.focal_length),
        (meta.timestamp > 0).then_some(meta.timestamp),
        info.width,
        info.height,
        meta.orientation,
    ])?;
    Ok(())
}

fn split_path(path: &Path) -> Result<(&Path, &str)> {
    let parent = path.parent().ok_or_else(|| Error::NonUtf8Path(path.to_owned()))?;
    let name = path.file_name().and_then(|name| name.to_str());
    Ok((parent, name.ok_or_else(|| Error::NonUtf8Path(path.to_owned()))?))
}

fn insert_photo(
    tx: &Transaction,
    file: &FoundFile,
    read: &FileRead,
    folders: &HashMap<PathBuf, FolderId>,
) -> Result<PhotoId> {
    let (directory, name) = split_path(&file.path)?;
    tx.prepare_cached(
        "INSERT INTO photos (folder_id, file_name, size, mtime_ns, fingerprint)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?
    .execute(params![
        folders.get(directory),
        name,
        file.stamp.size,
        file.stamp.mtime_ns,
        read.fingerprint
    ])?;
    let id = PhotoId(tx.last_insert_rowid());
    tx.prepare_cached("INSERT INTO variants (photo_id) VALUES (?1)")?.execute([id])?;
    store_metadata(tx, id, read)?;
    Ok(id)
}

fn update_file_identity(
    tx: &Transaction,
    id: PhotoId,
    file: &FoundFile,
    read: &FileRead,
    folders: &HashMap<PathBuf, FolderId>,
) -> Result<()> {
    let (directory, name) = split_path(&file.path)?;
    tx.prepare_cached(
        "UPDATE photos SET folder_id = ?2, file_name = ?3, size = ?4, mtime_ns = ?5, fingerprint = ?6
         WHERE id = ?1",
    )?
    .execute(params![id, folders.get(directory), name, file.stamp.size, file.stamp.mtime_ns, read.fingerprint])?;
    store_metadata(tx, id, read)
}

/// The photo this new path is a move of: one with the same content whose old path is gone.
/// If the old file is still there, the new path is a copy and becomes a photo of its own.
fn find_moved_from(
    tx: &Transaction,
    file: &FoundFile,
    read: &FileRead,
    discovered: &HashSet<PathBuf>,
) -> Result<Option<PhotoId>> {
    let mut statement = tx.prepare_cached(
        "SELECT p.id, f.path, p.file_name
         FROM photos p JOIN folders f ON f.id = p.folder_id
         WHERE p.fingerprint = ?1 AND p.size = ?2
         ORDER BY p.id",
    )?;
    let candidates = statement.query_map(params![read.fingerprint, file.stamp.size], |row| {
        let folder: String = row.get(1)?;
        let file_name: String = row.get(2)?;
        Ok((row.get::<_, PhotoId>(0)?, Path::new(&folder).join(file_name)))
    })?;
    for candidate in candidates {
        let (id, old_path) = candidate?;
        if !discovered.contains(&old_path) && !old_path.exists() {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

fn count_unsupported(read: &FileRead, report: &mut ScanReport) {
    if read.info.is_err() {
        report.unsupported += 1;
    }
}

fn store_read(
    tx: &Transaction,
    file: &FoundFile,
    existing: Option<PhotoId>,
    read: &FileRead,
    store: &mut Store,
) -> Result<PhotoId> {
    count_unsupported(read, store.report);
    if let Some(id) = existing {
        update_file_identity(tx, id, file, read, &store.folders)?;
        store.report.changed += 1;
        return Ok(id);
    }
    if let Some(id) = find_moved_from(tx, file, read, &store.discovered)? {
        update_file_identity(tx, id, file, read, &store.folders)?;
        store.moved.insert(id);
        store.report.moved += 1;
        return Ok(id);
    }
    store.report.added += 1;
    insert_photo(tx, file, read, &store.folders)
}

fn store_sidecar(
    tx: &Transaction,
    photo: PhotoId,
    sidecar: (FoundSidecar, Result<XmpData>),
    report: &mut ScanReport,
) -> Result<()> {
    let (found, data) = sidecar;
    match data {
        Ok(_) if is_dirty(tx, photo)? => {
            report.sidecar_conflicts += 1;
            record_conflict(tx, photo, found.stamp)
        }
        Ok(data) => {
            report.sidecars_read += 1;
            apply_sidecar(tx, photo, &data)?;
            record_synced(tx, photo, found.stamp)
        }
        Err(error) => {
            report.failures.push(ScanFailure { path: found.path, message: error.to_string() });
            record_synced(tx, photo, found.stamp)
        }
    }
}

fn store_outcome(tx: &Transaction, outcome: Outcome, store: &mut Store) -> Result<()> {
    let Outcome { file, work, read, sidecar } = outcome;
    let photo = match (work, read) {
        (Work::Read { existing }, Some(Ok(read))) => store_read(tx, &file, existing, &read, store)?,
        (Work::Keep { photo, restore }, _) => {
            if restore {
                set_status(tx, photo, PhotoStatus::Available)?;
                store.report.restored += 1;
            } else {
                store.report.unchanged += 1;
            }
            photo
        }
        (Work::Read { .. }, failed) => {
            let message = failed
                .and_then(|read| read.err())
                .map_or_else(String::new, |error| error.to_string());
            store.report.failures.push(ScanFailure { path: file.path, message });
            return Ok(());
        }
    };
    match sidecar {
        Some(sidecar) => store_sidecar(tx, photo, sidecar, store.report),
        None => Ok(()),
    }
}

fn is_under_any(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

impl Catalog {
    /// Brings the catalog up to date with `root` and everything below it. Files whose size
    /// and modification time match the catalog aren't opened. A file LibRaw can't open is
    /// recorded as unsupported, and a file that can't be read goes in the report's failures.
    ///
    /// `on_progress` runs on the calling thread. Results are committed as they arrive, so a
    /// cancelled scan keeps its progress and the next one picks up from there.
    pub fn scan(
        &self,
        root: &Path,
        cancel: &CancelToken,
        mut on_progress: impl FnMut(ScanProgress),
    ) -> Result<ScanReport> {
        let root = fs::canonicalize(root).map_err(io_error(root))?;
        path_text(&root)?;
        let _turn = self.lock_scan_turn();
        let mut walked = walk(&root, cancel, &mut on_progress);
        let mut report = ScanReport {
            discovered: walked.files.len(),
            failures: std::mem::take(&mut walked.failures),
            cancelled: cancel.is_cancelled(),
            ..ScanReport::default()
        };
        if report.cancelled {
            return Ok(report);
        }

        let folders = self.write(|tx| ensure_folders(tx, &root, &walked.files))?;
        let discovered: HashSet<PathBuf> =
            walked.files.iter().map(|file| file.path.clone()).collect();
        let mut known = self.read(|conn| load_known(conn, &root))?;
        let tasks = plan_tasks(&mut walked, &mut known, &mut report);

        let mut store = Store { folders, discovered, moved: HashSet::new(), report: &mut report };
        self.run_and_store(tasks, cancel, &mut store, &mut on_progress)?;
        let moved = store.moved;

        report.cancelled = cancel.is_cancelled();
        if !report.cancelled {
            report.missing = self.mark_missing(&known, &moved, &walked.unreadable)?;
            self.optimize()?;
        }
        Ok(report)
    }

    fn run_and_store(
        &self,
        tasks: Vec<Task>,
        cancel: &CancelToken,
        store: &mut Store,
        on_progress: &mut impl FnMut(ScanProgress),
    ) -> Result<()> {
        let total = tasks.len();
        let (sender, receiver) = mpsc::sync_channel(BATCH_SIZE);
        thread::scope(|scope| {
            scope.spawn(move || run_tasks(tasks, sender, cancel));
            // Owning the receiver here drops it on an early return, which frees any worker
            // blocked on a full channel so the scope can finish.
            let receiver = receiver;
            let mut batch = Vec::with_capacity(BATCH_SIZE);
            for (index, outcome) in receiver.iter().enumerate() {
                batch.push(outcome);
                on_progress(ScanProgress { phase: ScanPhase::Reading, done: index + 1, total });
                if batch.len() == BATCH_SIZE {
                    self.store_batch(&mut batch, store)?;
                }
            }
            self.store_batch(&mut batch, store)
        })
    }

    fn store_batch(&self, batch: &mut Vec<Outcome>, store: &mut Store) -> Result<()> {
        self.write(|tx| batch.drain(..).try_for_each(|outcome| store_outcome(tx, outcome, store)))
    }

    /// Flags photos that weren't found, except those moved during this scan or inside a
    /// folder that couldn't be read.
    fn mark_missing(
        &self,
        known: &HashMap<PathBuf, KnownPhoto>,
        moved: &HashSet<PhotoId>,
        unreadable: &[PathBuf],
    ) -> Result<usize> {
        let vanished: Vec<PhotoId> = known
            .iter()
            .filter(|(path, photo)| {
                photo.status != PhotoStatus::Missing
                    && !moved.contains(&photo.id)
                    && !is_under_any(path, unreadable)
            })
            .map(|(_, photo)| photo.id)
            .collect();
        self.write(|tx| {
            vanished.iter().try_for_each(|id| set_status(tx, *id, PhotoStatus::Missing))?;
            Ok(vanished.len())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_are_listed_in_lowercase() {
        assert!(
            RAW_EXTENSIONS.iter().all(|extension| extension.chars().all(|c| !c.is_uppercase()))
        );
    }

    #[test]
    fn fingerprints_track_content_and_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = |name| dir.path().join(name);
        fs::write(path("a"), vec![1; 300_000]).unwrap();
        fs::write(path("same"), vec![1; 300_000]).unwrap();
        fs::write(path("longer"), vec![1; 300_001]).unwrap();
        let mut tail_changed = vec![1; 300_000];
        tail_changed[299_999] = 2;
        fs::write(path("tail"), tail_changed).unwrap();

        let a = fingerprint(&path("a")).unwrap();
        assert_eq!(a, fingerprint(&path("same")).unwrap());
        assert_ne!(a, fingerprint(&path("longer")).unwrap());
        assert_ne!(a, fingerprint(&path("tail")).unwrap());
    }

    #[test]
    fn a_small_file_is_fingerprinted_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a");
        fs::write(&path, b"short").unwrap();
        let first = fingerprint(&path).unwrap();
        fs::write(&path, b"shore").unwrap();
        assert_ne!(first, fingerprint(&path).unwrap());
        fs::write(&path, b"").unwrap();
        fingerprint(&path).unwrap();
    }

    #[test]
    fn a_file_shorter_than_two_chunks_covers_its_tail_without_overrunning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a");
        let mut bytes = vec![7; FINGERPRINT_CHUNK as usize + 10];
        fs::write(&path, &bytes).unwrap();
        let first = fingerprint(&path).unwrap();
        *bytes.last_mut().unwrap() = 8;
        fs::write(&path, &bytes).unwrap();
        assert_ne!(first, fingerprint(&path).unwrap());
    }
}

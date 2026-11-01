use crate::{Result, migrations};
use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};
use std::{
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard, PoisonError},
    time::Duration,
};

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const STATEMENT_CACHE_SIZE: usize = 64;

/// The catalog. Share it across threads: writes take turns on one connection, while reads
/// use a pool of their own connections and, thanks to WAL, never wait on a writer.
pub struct Catalog {
    path: PathBuf,
    writer: Mutex<Connection>,
    readers: Mutex<Vec<Connection>>,
    /// Two scans would both see a new file as new and collide inserting it.
    scan_turn: Mutex<()>,
}

impl Catalog {
    /// Opens the catalog at `path`, creating and migrating it as needed.
    pub fn open(path: &Path) -> Result<Self> {
        let mut writer = Connection::open(path)?;
        configure(&writer)?;
        writer.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))?;
        writer.pragma_update(None, "synchronous", "NORMAL")?;
        migrations::migrate(&mut writer)?;
        Ok(Self {
            path: path.to_owned(),
            writer: Mutex::new(writer),
            readers: Mutex::default(),
            scan_turn: Mutex::new(()),
        })
    }

    /// Refreshes the query planner's statistics, which large imports leave stale.
    pub fn optimize(&self) -> Result<()> {
        self.write(|tx| {
            Ok(tx.execute_batch("PRAGMA analysis_limit = 1000; PRAGMA optimize = 0x10002;")?)
        })
    }

    pub(crate) fn lock_scan_turn(&self) -> MutexGuard<'_, ()> {
        lock(&self.scan_turn)
    }

    pub(crate) fn read<T>(&self, query: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let pooled = lock(&self.readers).pop();
        let conn = match pooled {
            Some(conn) => conn,
            None => self.open_reader()?,
        };
        let result = query(&conn);
        lock(&self.readers).push(conn);
        result
    }

    pub(crate) fn write<T>(&self, change: impl FnOnce(&Transaction) -> Result<T>) -> Result<T> {
        let mut conn = lock(&self.writer);
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let value = change(&tx)?;
        tx.commit()?;
        Ok(value)
    }

    fn open_reader(&self) -> Result<Connection> {
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(&self.path, flags)?;
        configure(&conn)?;
        Ok(conn)
    }
}

fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.set_prepared_statement_cache_capacity(STATEMENT_CACHE_SIZE);
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.pragma_update(None, "cache_size", -32_000)?;
    conn.pragma_update(None, "mmap_size", 1i64 << 30)?;
    Ok(())
}

/// A panic mid-write leaves nothing to repair, because the open transaction rolls back when
/// it drops, so a poisoned lock is safe to take.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

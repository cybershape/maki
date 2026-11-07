use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::SystemTime;

use sha2::{Digest, Sha256};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};
use tracing::{debug, warn};

const STALE_READ_MSG: &str = "file changed since last read";
const MAX_HASH_BYTES: u64 = 10 * 1024 * 1024;

/// Dead `Weak`s are only ever cleared once the map grows to this many entries,
/// so the map is bounded by construction instead of by a drop hook.
const LOCK_PRUNE_AT: usize = 512;

/// A path that has been through [`maki_storage::paths::canonical_key`]. Both
/// maps here are keyed by one, so no caller can key them with a raw path. Two
/// spellings of one file would get two entries, and for the lock map that
/// means no mutual exclusion at all.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct FileKey(PathBuf);

impl FileKey {
    pub fn new(path: &Path) -> Self {
        Self(maki_storage::paths::canonical_key(path))
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

#[derive(Clone, Debug)]
struct RecordedRead {
    mtime: SystemTime,
    len: u64,
    sha256: Option<[u8; 32]>,
}

type FileLock = Arc<AsyncMutex<()>>;
pub type FileGuard = OwnedMutexGuard<()>;
type FileLocks = Mutex<HashMap<FileKey, Weak<AsyncMutex<()>>>>;

/// What one agent read and when, plus the per-file write locks that make one
/// tool's read-modify-write safe against another's. The locks can be shared
/// with agents working next to this one, see [`FileAccess::sharing_locks`].
#[derive(Default)]
pub struct FileAccess {
    records: Mutex<HashMap<FileKey, RecordedRead>>,
    locks: Arc<FileLocks>,
}

fn compute_hash(path: &Path, len: u64) -> Option<[u8; 32]> {
    if len > MAX_HASH_BYTES {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    Some(Sha256::digest(&bytes).into())
}

impl FileAccess {
    pub fn fresh() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// For an agent that runs next to this one instead of under it. Its reads
    /// stay its own. Say the user changes a file we read earlier: if its fresh
    /// read counted for us too, we would never hear our copy is stale, and our
    /// next edit would quietly undo the user's. The locks are shared, so two
    /// edits of one file still take turns.
    pub fn sharing_locks(&self) -> Arc<Self> {
        Arc::new(Self {
            records: Mutex::default(),
            locks: Arc::clone(&self.locks),
        })
    }

    /// The write lock for one file, held for a tool's entire execution so its
    /// read-modify-write cannot interleave with another tool's on the same
    /// file. The dispatcher is the only caller: a tool that declares
    /// `mutable_path` gets this for free and must not re-implement it.
    ///
    /// A tool that declares `mutable_path` and also dispatches a child tool
    /// onto that same path would wait on itself. None does, since `batch`,
    /// `task` and `code_execution` declare no mutable path, and nothing
    /// enforces it statically. Hence the log line when a wait starts: it is
    /// the only trace such a hang would leave.
    ///
    /// One path only. A tool mutating several would need a globally sorted
    /// acquire order to avoid lock-order inversion, and none exists yet.
    pub async fn acquire(&self, key: &FileKey) -> FileGuard {
        let lock = self.lock_for(key);
        match Arc::clone(&lock).try_lock_owned() {
            Ok(guard) => guard,
            Err(_) => {
                debug!(path = %key.as_path().display(), "waiting for the file lock");
                lock.lock_owned().await
            }
        }
    }

    /// The guard and every waiter own an `Arc` clone, so an entry lives
    /// exactly as long as it is in use. [`LOCK_PRUNE_AT`] only bounds the
    /// residue of dead `Weak`s left behind.
    fn lock_for(&self, key: &FileKey) -> FileLock {
        let mut map = self.locks.lock().unwrap();
        if map.len() >= LOCK_PRUNE_AT {
            map.retain(|_, weak| weak.strong_count() > 0);
        }
        map.get(key).and_then(Weak::upgrade).unwrap_or_else(|| {
            let created = FileLock::default();
            map.insert(key.clone(), Arc::downgrade(&created));
            created
        })
    }

    pub fn record_read(&self, key: &FileKey) {
        let path = key.as_path();
        let metadata = match fs::metadata(path) {
            Ok(m) => m,
            Err(_) => {
                warn!(
                    path = %path.display(),
                    "record_read: could not get metadata, file will not be tracked"
                );
                return;
            }
        };
        let mtime = match metadata.modified() {
            Ok(t) => t,
            Err(_) => {
                warn!(
                    path = %path.display(),
                    "record_read: could not get mtime, file will not be tracked"
                );
                return;
            }
        };
        let len = metadata.len();
        let sha256 = compute_hash(path, len);
        self.records
            .lock()
            .unwrap()
            .insert(key.clone(), RecordedRead { mtime, len, sha256 });
    }

    pub fn check_before_edit(&self, key: &FileKey) -> Result<(), String> {
        let mut guard = self.records.lock().unwrap();
        let Some(recorded) = guard.get(key) else {
            return Ok(());
        };
        let Ok(metadata) = fs::metadata(key.as_path()) else {
            guard.remove(key);
            return Ok(());
        };
        let Ok(current_mtime) = metadata.modified() else {
            guard.remove(key);
            return Ok(());
        };
        if recorded.mtime == current_mtime {
            return Ok(());
        }

        let current_len = metadata.len();
        if let Some(recorded_hash) = recorded.sha256
            && current_len == recorded.len
            && let Some(current_hash) = compute_hash(key.as_path(), current_len)
            && current_hash == recorded_hash
        {
            debug!(
                path = %key.as_path().display(),
                "mtime changed but content hash matched; refreshing recorded mtime"
            );
            if let Some(entry) = guard.get_mut(key) {
                entry.mtime = current_mtime;
            }
            return Ok(());
        }

        Err(format!(
            "{STALE_READ_MSG}: {} - re-read using read tool before editing",
            key.as_path().display(),
        ))
    }
}

/// Spelling-independence belongs to [`FileKey`], which is the only way to key
/// either map, so it is tested once over `canonical_key` in `maki-storage`
/// rather than again for every map.
#[cfg(test)]
mod tests {
    use futures::FutureExt;
    use test_case::test_case;

    use super::*;
    use crate::tools::test_support::future_mtime;

    const FILE: &str = "f.rs";
    const CONTENT: &str = "content";

    /// `meanwhile` stands in for everything that can happen between a read and
    /// the edit that follows it. Only a file we read and someone else changed
    /// may be refused, since anything else would block an edit for no reason.
    #[test_case(|_, _, _| {} => true ; "never_read")]
    #[test_case(|access, key, _| access.record_read(key) => true ; "read_and_untouched")]
    #[test_case(|access, key, path| {
        access.record_read(key);
        future_mtime(path);
    } => true ; "mtime_changed_same_content")]
    #[test_case(|access, key, path| {
        access.record_read(key);
        fs::write(path, "diff_len").unwrap();
        future_mtime(path);
    } => false ; "diff_len_after_the_read")]
    #[test_case(|access, key, path| {
        access.record_read(key);
        fs::write(path, "diff_ct").unwrap();
        future_mtime(path);
    } => false ; "diff_content_same_len_after_the_read")]
    #[test_case(|access, key, path| {
        access.record_read(key);
        fs::write(path, "changed").unwrap();
        future_mtime(path);
        access.record_read(key);
    } => true ; "changed_then_read_again")]
    #[test_case(|access, key, path| {
        access.record_read(key);
        fs::remove_file(path).unwrap();
    } => true ; "deleted_after_the_read")]
    #[test_case(|access, key, path| {
        fs::remove_file(path).unwrap();
        access.record_read(key);
    } => true ; "read_of_a_file_that_is_gone")]
    fn edit_is_allowed_unless_the_file_moved_under_us(
        meanwhile: fn(&FileAccess, &FileKey, &Path),
    ) -> bool {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join(FILE);
        fs::write(&path, CONTENT).unwrap();
        let key = FileKey::new(&path);

        let access = FileAccess::default();
        meanwhile(&access, &key, &path);
        match access.check_before_edit(&key) {
            Ok(()) => true,
            Err(message) => {
                assert!(message.contains(STALE_READ_MSG), "{message}");
                false
            }
        }
    }

    #[tokio::test]
    async fn one_lock_per_file_held_until_the_guard_drops() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = FileKey::new(&dir.path().join(FILE));
        let other = FileKey::new(&dir.path().join("other.rs"));

        let access = FileAccess::default();
        let held = access.acquire(&path).await;
        assert!(
            access.acquire(&path).now_or_never().is_none(),
            "the same file must not be lockable twice"
        );
        assert!(
            access.acquire(&other).now_or_never().is_some(),
            "another file must not wait on this one"
        );
        drop(held);
        assert!(access.acquire(&path).now_or_never().is_some());
    }

    #[test]
    fn mtime_heals_after_hash_match() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join(FILE);
        fs::write(&path, CONTENT).unwrap();
        let key = FileKey::new(&path);

        let access = FileAccess::default();
        access.record_read(&key);
        future_mtime(&path);

        let new_mtime = fs::metadata(&path).unwrap().modified().unwrap();
        assert!(access.check_before_edit(&key).is_ok());
        let recorded = access.records.lock().unwrap().get(&key).unwrap().mtime;
        assert_eq!(recorded, new_mtime);
    }

    #[tokio::test]
    async fn dead_locks_do_not_accumulate() {
        let dir = tempfile::TempDir::new().unwrap();
        let access = FileAccess::default();
        for i in 0..=LOCK_PRUNE_AT {
            let key = FileKey::new(&dir.path().join(format!("{i}.rs")));
            drop(access.acquire(&key).await);
        }
        assert!(access.locks.lock().unwrap().len() <= LOCK_PRUNE_AT);
    }
}

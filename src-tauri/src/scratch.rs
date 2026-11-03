//! Scratch entries Silo makes beside a mod (#394), and the journal that proves they're Silo's:
//! - an archive move's in-flight copy (`MoveTmp`, `….silo-move-tmp`);
//! - the old copy an archive move is deleting (`MoveOld`, `….silo-move-old`);
//! - the temp a relink makes before renaming it over a mods-folder entry (`RelinkTmp`,
//!   `….silo-relink-tmp`);
//! - a download's part file (`Part`: `….silo-update.part` for an update, `….part` for an
//!   install) (#400).
//!
//! The journal also lists the update backups Silo made (`UpdateBak`, #400). Those aren't
//! scratch: no sweep looks at them. Only `update::prune_backups` removes one, and only a
//! recorded one past the newest few of its mod.
//!
//! Each gets a name of its own (`<name>.<token><suffix>`, see [`unique_beside`]), is recorded
//! in the journal *before* it is made, and is made exclusively (never over anything already
//! there). Only a recorded entry is ever deleted, and only once [`sweep_where`] proves it isn't the
//! only copy of anything. Scratch names from older versions (`<name><suffix>`, never recorded)
//! and anything else unrecorded are left in place and logged ([`log_unrecorded`]). The
//! suffixes stay what they were, so `scan::is_silo_temp` keeps skipping both styles.

use crate::db;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// An archive move's in-flight copy.
pub(crate) const MOVE_TMP: &str = ".silo-move-tmp";
/// An old copy an archive move renamed aside to delete (so a half-deleted folder never keeps
/// the mod's real name).
pub(crate) const MOVE_OLD: &str = ".silo-move-old";
/// A relink's new mods-folder entry, before it's renamed over the old one.
pub(crate) const RELINK_TMP: &str = ".silo-relink-tmp";

/// What a scratch entry is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Role {
    MoveTmp,
    MoveOld,
    RelinkTmp,
    /// A download's part file (an install or an update), made by a `PartClaim`.
    Part,
    /// An update backup Silo moved into `backups/` (never swept; see `update::prune_backups`).
    UpdateBak,
}

/// One recorded scratch entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Entry {
    pub path: PathBuf,
    pub role: Role,
    /// `MoveTmp`: the original being copied. `MoveOld`: the mod's name it was renamed from.
    /// `RelinkTmp`: the archived copy it links to (or copies). `Part`: the mod file it
    /// downloads. `UpdateBak`: the set-aside old version it was moved from.
    pub from: PathBuf,
    /// `MoveTmp`: where the copy is placed. `MoveOld`: its replacement, which must verify
    /// before the old copy may go. `RelinkTmp`: the mods-folder entry it replaces. `Part`: the
    /// mod file it becomes. `UpdateBak`: the mod file it's a previous version of.
    pub to: PathBuf,
    /// `zip` or `dir`.
    pub kind: String,
    /// `Part`, `UpdateBak`: the file Silo made there, read right after it made it. Without it
    /// nothing proves the file at `path` is still that one, so it's never removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub made: Option<Made>,
}

/// Which file is at a path, and how it was, read right after Silo made it (#400): what proves a
/// later file there is still Silo's own. The file's number on its volume (Windows: volume serial
/// and file index; Unix: device and inode) and its birth time where the system keeps one: a file
/// made, moved or copied there later is another file. Size and last change tell whether it was
/// changed since; an update backup also keeps its SHA-256 ([`Made::with_sha256`]), so its bytes
/// are checked too.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Made {
    pub volume: u64,
    pub index: u64,
    /// Birth time, ns since 1970.
    #[serde(default)]
    pub created: Option<u64>,
    pub size: u64,
    /// Last change, ns since 1970.
    #[serde(default)]
    pub modified: Option<u64>,
    /// The bytes' SHA-256 (an update backup's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// A download's part: its length and SHA-256 before the chunk being written, then after it
    /// (`PartFile`; only the first, before any chunk). A part holds only what Silo wrote while
    /// its length is one of these and its bytes hash to it, or its length lies between the two
    /// and its first bytes hash to the one before ([`Made::holds_written`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub written: Vec<Written>,
}

/// A length a download's part was written to, and the SHA-256 of its bytes then.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Written {
    pub len: u64,
    pub sha256: String,
}

fn nanos(t: std::io::Result<std::time::SystemTime>) -> Option<u64> {
    let d = t.ok()?.duration_since(std::time::UNIX_EPOCH).ok()?;
    u64::try_from(d.as_nanos()).ok()
}

impl Made {
    /// The open file `f` (just made).
    pub(crate) fn of_file(f: &std::fs::File) -> Option<Made> {
        let m = f.metadata().ok()?;
        if !m.is_file() {
            return None;
        }
        let (volume, index) = file_number(f, &m)?;
        Some(Made {
            volume,
            index,
            created: nanos(m.created()),
            size: m.len(),
            modified: nanos(m.modified()),
            sha256: None,
            written: Vec::new(),
        })
    }

    /// [`Made::of`] plus the bytes' SHA-256, read back after (`None` if the file changed while
    /// it was read).
    pub(crate) fn with_sha256(p: &Path) -> Option<Made> {
        let before = Made::of(p)?;
        let sha = crate::provenance::sha256_file(p).ok()?;
        let after = Made::of(p)?;
        before.unchanged(&after).then_some(Made {
            sha256: Some(sha),
            ..before
        })
    }

    /// The file at `p` itself: `None` for a link, a folder, or anything that can't be read.
    pub(crate) fn of(p: &Path) -> Option<Made> {
        if !p.symlink_metadata().is_ok_and(|m| m.is_file()) {
            return None;
        }
        Made::of_file(&open_no_follow(p).ok()?)
    }

    /// The same file: same number on the same volume, born at the same time.
    pub(crate) fn same_file(&self, other: &Made) -> bool {
        self.volume == other.volume && self.index == other.index && self.created == other.created
    }

    /// The same file, unchanged since (same size, same last change).
    pub(crate) fn unchanged(&self, other: &Made) -> bool {
        self.same_file(other) && self.size == other.size && self.modified == other.modified
    }

    /// The file at `p` is this one (the same file, read without following a link) and holds
    /// only what Silo wrote into it, unchanged while it was read. Never without `written`.
    /// Either:
    /// - its length is one recorded in `written`, and all its bytes hash to that length's
    ///   SHA-256; or
    /// - its length lies strictly between the length recorded before a chunk and the one
    ///   after it (a crash or a full disk cut that chunk's write off), and its first `before`
    ///   bytes hash to the SHA-256 recorded before the chunk. The bytes past those are taken to
    ///   be the start of that chunk: a write that never finished can't be hashed, and only
    ///   Silo writes into its part (#400).
    pub(crate) fn holds_written(&self, p: &Path) -> bool {
        let Some(before) = Made::of(p).filter(|now| self.same_file(now)) else {
            return false;
        };
        let len = before.size;
        let (count, want) = if let Some(w) = self.written.iter().find(|w| w.len == len) {
            (u64::MAX, &w.sha256)
        } else if let Some(w) = self
            .written
            .windows(2)
            .find(|w| w[0].len < len && len < w[1].len)
            .map(|w| &w[0])
        {
            (w.len, &w.sha256)
        } else {
            return false;
        };
        let sha = sha256_prefix(p, count);
        let after = Made::of(p);
        sha.is_some_and(|sha| &sha == want) && after.is_some_and(|a| before.unchanged(&a))
    }

    /// The file at `p` is this one, unchanged, holding the recorded bytes (when a SHA-256 was
    /// recorded; without one, never).
    pub(crate) fn holds_same_bytes(&self, p: &Path) -> bool {
        let Some(want) = &self.sha256 else {
            return false;
        };
        Made::with_sha256(p)
            .is_some_and(|now| self.unchanged(&now) && now.sha256.as_ref() == Some(want))
    }
}

/// The SHA-256 of `p`'s first `n` bytes (all of them for `u64::MAX`); `None` when it can't be
/// read or holds fewer.
fn sha256_prefix(p: &Path, n: u64) -> Option<String> {
    use sha2::Digest;
    use std::io::Read;
    let mut r = std::fs::File::open(p).ok()?.take(n);
    let mut h = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut got = 0u64;
    loop {
        let k = r.read(&mut buf).ok()?;
        if k == 0 {
            break;
        }
        h.update(&buf[..k]);
        got += k as u64;
    }
    (n == u64::MAX || got == n).then(|| format!("{:x}", h.finalize()))
}

/// The volume and file number of the entry at `p` itself, never following a link at it: a link
/// (even one whose target is gone) has its own. `None` when it can't be read.
#[cfg(windows)]
pub(crate) fn entry_number(p: &Path) -> Option<(u64, u64)> {
    let f = open_no_follow(p).ok()?;
    let m = f.metadata().ok()?;
    file_number(&f, &m)
}

#[cfg(unix)]
pub(crate) fn entry_number(p: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let m = p.symlink_metadata().ok()?;
    Some((m.dev(), m.ino()))
}

/// Open `p` for its attributes only, never following a link at it (a link opens as itself).
#[cfg(windows)]
fn open_no_follow(p: &Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
    };
    std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(p)
}

#[cfg(unix)]
fn open_no_follow(p: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    // O_NOFOLLOW: a link at `p` fails to open. Non-blocking, so a FIFO put there can't hang.
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(p)
}

#[cfg(windows)]
fn file_number(f: &std::fs::File, _: &std::fs::Metadata) -> Option<(u64, u64)> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    // SAFETY: a zeroed plain-data struct, filled by the call; the handle is open for the call.
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe { GetFileInformationByHandle(f.as_raw_handle(), &mut info) };
    (ok != 0).then(|| {
        (
            u64::from(info.dwVolumeSerialNumber),
            (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        )
    })
}

#[cfg(unix)]
fn file_number(_: &std::fs::File, m: &std::fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((m.dev(), m.ino()))
}

/// Where scratch entries are recorded. Recording comes first: an entry that couldn't be
/// recorded is never made.
pub(crate) trait Journal {
    fn record(&self, e: &Entry) -> Result<(), String>;
    /// Drop the record of `path` (it's gone, or became something else's: a placed copy).
    fn forget(&self, path: &Path);
    fn entries(&self) -> Vec<Entry>;
}

/// `app_setting` key holding the journal (a JSON list of [`Entry`]).
const JOURNAL_KEY: &str = "scratch_journal";

/// The journal in Silo's database.
pub(crate) struct DbJournal<'c>(pub &'c Connection);

impl DbJournal<'_> {
    fn write(&self, v: &[Entry]) -> Result<(), String> {
        if v.is_empty() {
            return db::set_app_setting(self.0, JOURNAL_KEY, None);
        }
        let j = serde_json::to_string(v).map_err(|e| e.to_string())?;
        db::set_app_setting(self.0, JOURNAL_KEY, Some(&j))
    }
}

impl DbJournal<'_> {
    /// Read, change and write the list as one step (`BEGIN IMMEDIATE`, unless the caller's
    /// transaction is already open): a download records its part outside the library lock, so
    /// a sweep's `forget` on another connection must never write back a list without it.
    /// `change` returns whether it changed anything.
    fn change(&self, change: impl FnOnce(&mut Vec<Entry>) -> bool) -> Result<(), String> {
        let own = self.0.is_autocommit();
        if own {
            self.0
                .execute_batch("BEGIN IMMEDIATE")
                .map_err(|e| e.to_string())?;
        }
        let mut v = self.entries();
        let r = if change(&mut v) {
            self.write(&v)
        } else {
            Ok(())
        };
        if own {
            let end = if r.is_ok() { "COMMIT" } else { "ROLLBACK" };
            if let Err(e) = self.0.execute_batch(end) {
                let _ = self.0.execute_batch("ROLLBACK");
                return r.and(Err(e.to_string()));
            }
        }
        r
    }
}

impl Journal for DbJournal<'_> {
    fn record(&self, e: &Entry) -> Result<(), String> {
        self.change(|v| {
            v.retain(|x| x.path != e.path);
            v.push(e.clone());
            true
        })
    }

    fn forget(&self, path: &Path) {
        let r = self.change(|v| {
            let before = v.len();
            v.retain(|x| x.path != path);
            v.len() != before
        });
        if let Err(e) = r {
            // Only costs a sweep that finds the path gone (or proves it again).
            log::warn!("couldn't update the scratch journal: {e}");
        }
    }

    /// An unreadable journal reads as empty: nothing is then deleted.
    fn entries(&self) -> Vec<Entry> {
        db::get_app_setting(self.0, JOURNAL_KEY)
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default()
    }
}

/// No journal: records nothing (so nothing is ever swept), lists nothing.
pub(crate) struct NoJournal;

impl Journal for NoJournal {
    fn record(&self, _: &Entry) -> Result<(), String> {
        Err("Silo has no scratch journal here".into())
    }
    fn forget(&self, _: &Path) {}
    fn entries(&self) -> Vec<Entry> {
        Vec::new()
    }
}

/// A fresh scratch name beside `target`: `<name>.<16 hex digits><suffix>`. Unique to this call
/// (a per-process random seed, the time and a counter); the caller still creates it
/// exclusively, so a clash can only fail, never replace anything.
pub(crate) fn unique_beside(target: &Path, suffix: &str) -> PathBuf {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    h.write_u128(nanos);
    h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    h.write_u32(std::process::id());
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "mod".into());
    target.with_file_name(format!("{name}.{:016x}{suffix}", h.finish()))
}

/// Record a new scratch entry, then return its path (not yet made).
pub(crate) fn begin(
    journal: &dyn Journal,
    role: Role,
    beside: &Path,
    suffix: &str,
    from: &Path,
    to: &Path,
    kind: &str,
) -> Result<PathBuf, String> {
    let path = unique_beside(beside, suffix);
    journal.record(&Entry {
        path: path.clone(),
        role,
        from: from.to_path_buf(),
        to: to.to_path_buf(),
        kind: kind.to_string(),
        made: None,
    })?;
    Ok(path)
}

/// Forget `path` if nothing is there any more (placed, renamed, or removed). Something still
/// there stays recorded, for a later [`sweep_where`].
pub(crate) fn settle(journal: &dyn Journal, path: &Path) {
    if path.symlink_metadata().is_err() {
        journal.forget(path);
    }
}

/// `p` is itself (not a link) a file for a zip, a folder for a folder mod.
pub(crate) fn real_copy(p: &Path, kind: &str) -> bool {
    p.symlink_metadata().is_ok_and(|m| {
        !m.file_type().is_symlink()
            && if kind == "dir" {
                m.is_dir()
            } else {
                m.is_file()
            }
    })
}

/// Two names for files that are not one name reached twice: both resolve, to different paths.
/// (Two hardlinks of one file are distinct names: removing one keeps the data under the other.
/// One name reached through a link, or a folder link into the tree being removed, is not.)
fn distinct(a: &Path, b: &Path) -> bool {
    matches!(
        (std::fs::canonicalize(a), std::fs::canonicalize(b)),
        (Ok(x), Ok(y)) if x != y
    )
}

/// `rel` under `root` is reached without following any link: every folder on the way is a real
/// folder, and the end is a real file.
fn real_file_under(root: &Path, rel: &Path) -> bool {
    let mut p = root.to_path_buf();
    let parts: Vec<_> = rel.components().collect();
    for (i, c) in parts.iter().enumerate() {
        p.push(c);
        let last = i + 1 == parts.len();
        let ok = p.symlink_metadata().is_ok_and(|m| {
            !m.file_type().is_symlink() && if last { m.is_file() } else { m.is_dir() }
        });
        if !ok {
            return false;
        }
    }
    !parts.is_empty()
}

/// `a` and `b` are distinct names (`distinct`) holding the same bytes: same size, then SHA-256.
/// `false` when either can't be read.
fn same_bytes(a: &Path, b: &Path) -> bool {
    let size = |p: &Path| std::fs::metadata(p).ok().map(|m| m.len());
    if !distinct(a, b) || size(a).is_none() || size(a) != size(b) {
        return false;
    }
    matches!(
        (crate::provenance::sha256_file(a), crate::provenance::sha256_file(b)),
        (Ok(x), Ok(y)) if x == y
    )
}

/// Every byte of `part` (a copy that may have stopped part-way) is in `whole` at the same
/// offset, and the two are distinct names (`distinct`). `false` when either can't be read.
fn prefix_of(part: &Path, whole: &Path) -> bool {
    use std::io::Read;
    if !distinct(part, whole) {
        return false;
    }
    let (Ok(mut a), Ok(mut b)) = (std::fs::File::open(part), std::fs::File::open(whole)) else {
        return false;
    };
    let (mut x, mut y) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
    loop {
        let n = match a.read(&mut x) {
            Ok(n) => n,
            Err(_) => return false,
        };
        if n == 0 {
            return true;
        }
        if b.read_exact(&mut y[..n]).is_err() || x[..n] != y[..n] {
            return false;
        }
    }
}

/// How [`covered_by`] compares each file.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cover {
    /// The same bytes (an old copy: its files are whole, a delete only removes some of them).
    Exact,
    /// A prefix of the other's bytes (a copy that may have stopped part-way through a file).
    Prefix,
}

/// Every file in `part` (a folder that may be half-deleted, or half-copied) has a counterpart
/// at the same relative path in `whole`, reached without following any link
/// (`real_file_under`), a distinct name, with its bytes (`cover`). A link anywhere in `part`
/// fails it. `skip` is a top-level file name not compared (Silo's marker in a marked copy).
fn covered_by(part: &Path, whole: &Path, cover: Cover, skip: Option<&str>) -> bool {
    fn walk(root: &Path, dir: &Path, whole: &Path, cover: Cover, skip: Option<&str>) -> bool {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return false;
        };
        for e in rd {
            let Ok(e) = e else { return false };
            let Ok(ft) = e.file_type() else { return false };
            let p = e.path();
            let rel = p.strip_prefix(root).unwrap_or(&p).to_path_buf();
            let ok = if ft.is_symlink() {
                false
            } else if ft.is_dir() {
                walk(root, &p, whole, cover, None)
            } else if dir == root && skip.is_some_and(|s| e.file_name() == s) {
                true
            } else {
                let other = whole.join(&rel);
                real_file_under(whole, &rel)
                    && match cover {
                        Cover::Exact => same_bytes(&p, &other),
                        Cover::Prefix => prefix_of(&p, &other),
                    }
            };
            if !ok {
                return false;
            }
        }
        true
    }
    distinct(part, whole) && walk(part, part, whole, cover, skip)
}

/// `copy` holds nothing that isn't also in `of`: a file whose every byte is at the same offset
/// in `of` (a copy that stopped part-way is a prefix), or a folder whose every file is (Silo's
/// marker in a marked copy aside). What a failed copy's temp must be before it's removed.
pub(crate) fn redundant_copy(copy: &Path, of: &Path) -> bool {
    match copy.symlink_metadata() {
        Ok(m) if m.file_type().is_symlink() => false,
        Ok(m) if m.is_dir() => {
            real_copy(of, "dir")
                && covered_by(copy, of, Cover::Prefix, Some(crate::organize::SILO_MARKER))
        }
        Ok(_) => real_copy(of, "zip") && prefix_of(copy, of),
        Err(_) => false,
    }
}

/// Every byte of `part` is in `whole`: the same bytes for a file, and for a folder every file
/// at the same place with the same bytes. What an original must be before it's deleted.
pub(crate) fn held_by(part: &Path, whole: &Path) -> bool {
    match part.symlink_metadata() {
        Ok(m) if m.file_type().is_symlink() => false,
        Ok(m) if m.is_dir() => {
            real_copy(whole, "dir") && covered_by(part, whole, Cover::Exact, None)
        }
        Ok(_) => real_copy(whole, "zip") && same_bytes(part, whole),
        Err(_) => false,
    }
}

/// `copy` (a copy Silo made in the mods folder, not a link) holds nothing its archived copy
/// `archived` doesn't: a zip with the same bytes (or the same file under another name, a
/// hardlink), or a marked folder copy whose every file but the marker is in `archived` with the
/// same bytes. Both must be real copies of the same kind. What a relink temp ([`sweep_where`])
/// and a mods-folder copy on park or flatten (#399) must be before they're removed.
pub(crate) fn copy_held_by_archive(copy: &Path, archived: &Path) -> bool {
    match copy.symlink_metadata() {
        Ok(m) if m.file_type().is_symlink() => false,
        Ok(m) if m.is_dir() => {
            real_copy(archived, "dir")
                && covered_by(
                    copy,
                    archived,
                    Cover::Exact,
                    Some(crate::organize::SILO_MARKER),
                )
        }
        Ok(m) if m.is_file() => {
            real_copy(archived, "zip") && (one_file(copy, archived) || same_bytes(copy, archived))
        }
        _ => false,
    }
}

/// Two distinct names (`distinct`) of one file: hardlinks, so the bytes are the same without
/// reading them.
fn one_file(a: &Path, b: &Path) -> bool {
    distinct(a, b)
        && matches!(
            (same_file::Handle::from_path(a), same_file::Handle::from_path(b)),
            (Ok(x), Ok(y)) if x == y
        )
}

/// Clean up a scratch entry right after the operation that made it failed: the same proof as
/// [`sweep_where`], so it's removed only when redundant; otherwise it's kept (and stays
/// recorded in `journal`, when there is one, for a later sweep).
pub(crate) fn discard(journal: Option<&dyn Journal>, e: &Entry) -> Swept {
    let s = sweep_one(e);
    if let Some(j) = journal {
        if s != Swept::Kept {
            j.forget(&e.path);
        }
    }
    if s == Swept::Kept {
        log::warn!(
            "kept {}: Silo couldn't prove it's safe to remove (from {})",
            e.path.display(),
            e.from.display()
        );
    }
    s
}

/// [`sweep_where`] over every entry.
#[cfg(test)]
pub(crate) fn sweep(journal: &dyn Journal) -> Vec<(Entry, Swept)> {
    sweep_where(journal, &|_| true)
}

/// Whether `p` is inside `dir` (compared as normalized keys, so a different spelling of the
/// same path counts).
pub(crate) fn inside(p: &Path, dir: &Path) -> bool {
    let key = |p: &Path| crate::modsroot::normalize_key(&p.to_string_lossy(), cfg!(windows));
    Path::new(&key(p)).starts_with(key(dir))
}

/// What [`sweep_where`] did with one entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Swept {
    /// Nothing was there; the record is dropped.
    Gone,
    /// Proven redundant and removed.
    Removed,
    /// An old copy whose replacement didn't verify, put back under the mod's name.
    PutBack,
    /// No longer the file Silo recorded making (another file is there, or Silo never read
    /// which file it made): left in place, logged, and the record dropped, since it proves
    /// nothing any more.
    Disowned,
    /// Kept: Silo couldn't prove it's safe to remove (logged).
    Kept,
}

/// Go through the journal: drop records of entries that are gone, remove each entry proven
/// redundant, and keep (and log) the rest. Never removes anything unrecorded. Run under the
/// library lock (no operation is making scratch right now). An entry whose folder can't be
/// reached (a drive that isn't plugged in) is kept recorded for later.
///
/// Proof, by role. Every comparison is against a distinct name reached without following a
/// link into the entry itself (`distinct`, `real_file_under`):
/// - `RelinkTmp`: a link (it holds no data); or a copy whose archived copy (`from`) holds the
///   same bytes (a file), or every file of Silo's untouched marked copy (a folder).
/// - `MoveTmp`: every byte it holds is at the same offset in the original (`from`), file by
///   file for a folder (a copy cut off part-way is a prefix); or (a zip) it holds the same bytes
///   as the placed copy (`to`): a hardlink left by a crash right after placing.
/// - `MoveOld`: its replacement (`to`) is a real copy holding the same bytes (a folder: every
///   file still left). With no replacement at all it may be the only copy left: a zip is put
///   back under its name (`from`) when that's free; a folder (perhaps half-deleted) is kept.
///
/// - `Part`: the very file the claim made (`made`: the same file number and birth time, read
///   without following a link), holding only what the download wrote (`Made::holds_written`:
///   its length is one it recorded and its bytes hash to that, or a chunk's write was cut off
///   and its bytes before that chunk hash to what was recorded), that no download running now
///   claims (`crate::remove_unclaimed_part` holds the claims across the check and the
///   removal). A recorded part nothing claims is a download that stopped (a crash, or a
///   removal that failed). Any other file there, or one with bytes Silo can't prove it wrote
///   (changed since, or longer than the chunk being written), is left in place and the record
///   dropped (`Disowned`).
///
/// Only the entries `which` picks are looked at (a relink pass sweeps relink temps; a move
/// sweeps those and its own folders' move scratch, never an older move's). Update backups
/// (`UpdateBak`) are never swept.
pub(crate) fn sweep_where(
    journal: &dyn Journal,
    which: &dyn Fn(&Entry) -> bool,
) -> Vec<(Entry, Swept)> {
    let mut out = Vec::new();
    for e in journal
        .entries()
        .into_iter()
        .filter(|e| e.role != Role::UpdateBak && which(e))
        // A running download's part: not looked at (not even whether it's there yet, so its
        // record is never dropped before the download makes it).
        .filter(|e| e.role != Role::Part || !crate::part_in_flight(&e.path))
    {
        let s = sweep_one(&e);
        match s {
            Swept::Gone | Swept::Removed | Swept::PutBack | Swept::Disowned => {
                journal.forget(&e.path)
            }
            Swept::Kept => {}
        }
        match s {
            Swept::Gone => {}
            Swept::Disowned => log::warn!(
                "left {} in place: it's no longer the file Silo made there",
                e.path.display()
            ),
            Swept::Removed => crate::fslog::changed("remove-scratch", &e.path),
            Swept::PutBack => crate::fslog::moved("restore", &e.path, &e.from),
            Swept::Kept => log::warn!(
                "kept {}: Silo couldn't prove it's safe to remove (from {}, for {})",
                e.path.display(),
                e.from.display(),
                e.to.display()
            ),
        }
        out.push((e, s));
    }
    out
}

/// Whether a recorded scratch path may still hold something: it's there, or its folder can't be
/// reached right now (so whether it's there can't be told).
fn may_hold(path: &Path) -> bool {
    path.symlink_metadata().is_ok() || !path.parent().is_some_and(|d| d.symlink_metadata().is_ok())
}

fn sweep_one(e: &Entry) -> Swept {
    let Ok(meta) = e.path.symlink_metadata() else {
        // Missing — unless its folder can't be reached right now.
        return if may_hold(&e.path) {
            Swept::Kept
        } else {
            Swept::Gone
        };
    };
    let link = meta.file_type().is_symlink();
    let removed = match e.role {
        Role::RelinkTmp if link => Some(remove_link_entry(&e.path)),
        // A copy (where no link could be made): its archived copy still holds every byte.
        Role::RelinkTmp if meta.is_file() => {
            copy_held_by_archive(&e.path, &e.from).then(|| std::fs::remove_file(&e.path))
        }
        // A marked folder copy: every file but the marker is still in the archived copy, and
        // `remove_link` checks the marker's listing (an untouched copy).
        Role::RelinkTmp => (meta.is_dir() && copy_held_by_archive(&e.path, &e.from))
            .then(|| crate::organize::remove_link(&e.path)),
        // Silo never makes a link as move scratch or a part: a link here isn't its own.
        Role::MoveTmp | Role::MoveOld | Role::Part if link => None,
        // A download that stopped: the file it made, which no download running now claims.
        Role::Part => match &e.made {
            Some(made) => match crate::remove_unclaimed_part(&e.path, made) {
                crate::PartSweep::Removed(r) => Some(r),
                crate::PartSweep::Claimed => None,
                crate::PartSweep::NotIt => return Swept::Disowned,
            },
            None => return Swept::Disowned,
        },
        // Never swept (`sweep_where` skips them); kept if one ever gets here.
        Role::UpdateBak => None,
        // A copy that may have stopped part-way: every byte it holds is still in the original
        // (or, a zip placed by hardlink, in the placed copy).
        Role::MoveTmp => {
            let redundant = if e.kind == "dir" {
                meta.is_dir()
                    && real_copy(&e.from, "dir")
                    && covered_by(&e.path, &e.from, Cover::Prefix, None)
            } else {
                meta.is_file()
                    && ((real_copy(&e.from, "zip") && prefix_of(&e.path, &e.from))
                        || (real_copy(&e.to, "zip") && same_bytes(&e.path, &e.to)))
            };
            redundant.then(|| remove_real(&e.path, meta.is_dir()))
        }
        Role::MoveOld => {
            let verified = real_copy(&e.to, &e.kind)
                && if e.kind == "dir" {
                    meta.is_dir() && covered_by(&e.path, &e.to, Cover::Exact, None)
                } else {
                    meta.is_file() && same_bytes(&e.path, &e.to)
                };
            if verified {
                Some(remove_real(&e.path, meta.is_dir()))
            } else if e.kind != "dir"
                && meta.is_file()
                && e.to.symlink_metadata().is_err()
                && crate::paths::rename_no_replace(&e.path, &e.from).is_ok()
            {
                return Swept::PutBack;
            } else {
                None
            }
        }
    };
    match removed {
        Some(Ok(())) => Swept::Removed,
        Some(Err(err)) => {
            log::warn!("couldn't remove {}: {err}", e.path.display());
            Swept::Kept
        }
        None => Swept::Kept,
    }
}

fn remove_real(p: &Path, dir: bool) -> std::io::Result<()> {
    if dir {
        std::fs::remove_dir_all(p)
    } else {
        std::fs::remove_file(p)
    }
}

/// Remove a symlink or junction itself (never what it points at). On Windows a folder link
/// needs `remove_dir`, and a dangling one can't tell which it is.
fn remove_link_entry(p: &Path) -> std::io::Result<()> {
    std::fs::remove_file(p).or_else(|_| std::fs::remove_dir(p))
}

/// Recorded scratch entries tied to any of `paths` (as their `from` or `to`) that are still
/// there, plus older-style scratch names beside them (`<path><suffix>`): what to point the user
/// at when a mod's copy is missing. A download's part and an update backup are never a copy
/// of the mod being moved, so they aren't listed.
pub(crate) fn kept_for(journal: &dyn Journal, paths: &[&Path]) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = journal
        .entries()
        .into_iter()
        .filter(|e| !matches!(e.role, Role::Part | Role::UpdateBak))
        .filter(|e| paths.iter().any(|p| e.from == *p || e.to == *p))
        .map(|e| e.path)
        .filter(|p| p.symlink_metadata().is_ok())
        .collect();
    for p in paths {
        for suffix in [MOVE_OLD, MOVE_TMP] {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let legacy = p.with_file_name(format!("{name}{suffix}"));
            if legacy.symlink_metadata().is_ok() && !v.contains(&legacy) {
                v.push(legacy);
            }
        }
    }
    v
}

/// Log each entry of `dir` whose name looks like Silo scratch (`is_scratch`) but isn't recorded
/// in the journal: an older version's leftover, or not Silo's at all. Left in place. A leftover
/// is noted once while it stays the same (the scan sweeps every time, #428); a new or replaced
/// one is noted again. Returns them all.
pub(crate) fn log_unrecorded(
    journal: &dyn Journal,
    dir: &Path,
    is_scratch: &dyn Fn(&str) -> bool,
) -> Vec<PathBuf> {
    let recorded: Vec<PathBuf> = journal.entries().into_iter().map(|e| e.path).collect();
    let mut v = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if is_scratch(&e.file_name().to_string_lossy()) && !recorded.contains(&p) {
                v.push(p);
            }
        }
    }
    static NOTED: std::sync::OnceLock<std::sync::Mutex<NotedLeftovers>> =
        std::sync::OnceLock::new();
    let fresh = match NOTED.get_or_init(Default::default).lock() {
        Ok(mut noted) => to_note(&mut noted, dir, is_scratch, &v, &leftover_stamp),
        Err(_) => v.clone(),
    };
    for p in fresh {
        log::warn!(
            "left {} in place: it looks like a leftover from an earlier version of Silo, and \
             Silo can't prove it made it",
            p.display()
        );
    }
    v
}

/// A leftover's file number (which file it is) with its size and times (whether it changed), to
/// tell a file that's still there from one put in its place.
type LeftoverStamp = (
    Option<(u64, u64)>,
    u64,
    Option<std::time::SystemTime>,
    Option<std::time::SystemTime>,
);

/// The unrecorded leftovers already noted this session, with how each looked then.
type NotedLeftovers = std::collections::HashMap<PathBuf, Option<LeftoverStamp>>;

fn leftover_stamp(p: &Path) -> Option<LeftoverStamp> {
    let m = std::fs::symlink_metadata(p).ok()?;
    Some((
        entry_number(p),
        m.len(),
        m.modified().ok(),
        m.created().ok(),
    ))
}

/// Which of `found` (the unrecorded leftovers in `dir` this sweep's `is_scratch` matches) to
/// note: one not noted yet, or one that changed since (another file put at its name). A noted one
/// in `dir` that this sweep would have matched but didn't find is forgotten, so it's noted again
/// if it comes back. Others (another sweep's kind, another folder) are left as they are.
/// `stamp` reads how a leftover looks now ([`leftover_stamp`]; a test passes its own).
fn to_note(
    noted: &mut NotedLeftovers,
    dir: &Path,
    is_scratch: &dyn Fn(&str) -> bool,
    found: &[PathBuf],
    stamp: &dyn Fn(&Path) -> Option<LeftoverStamp>,
) -> Vec<PathBuf> {
    noted.retain(|p, _| {
        let covered = p.parent() == Some(dir)
            && p.file_name()
                .is_some_and(|n| is_scratch(&n.to_string_lossy()));
        !covered || found.contains(p)
    });
    let mut fresh = Vec::new();
    for p in found {
        let now = stamp(p);
        if noted.get(p) != Some(&now) {
            noted.insert(p.clone(), now);
            fresh.push(p.clone());
        }
    }
    fresh
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::RefCell;

    /// An in-memory journal.
    #[derive(Default)]
    pub(crate) struct MemJournal(pub RefCell<Vec<Entry>>);

    impl Journal for MemJournal {
        fn record(&self, e: &Entry) -> Result<(), String> {
            let mut v = self.0.borrow_mut();
            v.retain(|x| x.path != e.path);
            v.push(e.clone());
            Ok(())
        }
        fn forget(&self, path: &Path) {
            self.0.borrow_mut().retain(|x| x.path != path);
        }
        fn entries(&self) -> Vec<Entry> {
            self.0.borrow().clone()
        }
    }

    fn base(tag: &str) -> PathBuf {
        let b = std::env::temp_dir().join(format!("silo_scratch_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&b);
        std::fs::create_dir_all(&b).unwrap();
        b
    }

    fn entry(path: &Path, role: Role, from: &Path, to: &Path, kind: &str) -> Entry {
        Entry {
            path: path.into(),
            role,
            from: from.into(),
            to: to.into(),
            kind: kind.into(),
            made: None,
        }
    }

    #[test]
    fn names_are_unique_and_keep_the_suffix() {
        let t = Path::new("/x/FS25_A.zip");
        let a = unique_beside(t, MOVE_TMP);
        let b = unique_beside(t, MOVE_TMP);
        assert_ne!(a, b);
        let n = a.file_name().unwrap().to_string_lossy().into_owned();
        assert!(n.starts_with("FS25_A.zip.") && n.ends_with(MOVE_TMP), "{n}");
        assert!(crate::scan::is_silo_temp(&n));
    }

    #[test]
    fn the_db_journal_records_and_forgets() {
        let b = base("db");
        let conn = db::open(&b.join("silo.db")).unwrap();
        let j = DbJournal(&conn);
        assert!(j.entries().is_empty());
        let p = begin(&j, Role::MoveTmp, &b.join("A.zip"), MOVE_TMP, &b, &b, "zip").unwrap();
        assert_eq!(j.entries().len(), 1);
        assert_eq!(j.entries()[0].path, p);
        settle(&j, &p); // nothing there: forgotten
        assert!(j.entries().is_empty());
        assert_eq!(db::get_app_setting(&conn, JOURNAL_KEY), None);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn an_unreadable_journal_deletes_nothing() {
        let b = base("corrupt");
        let conn = db::open(&b.join("silo.db")).unwrap();
        db::set_app_setting(&conn, JOURNAL_KEY, Some("not json")).unwrap();
        let p = b.join("A.zip.0123456789abcdef.silo-move-tmp");
        std::fs::write(&p, b"x").unwrap();
        assert!(sweep(&DbJournal(&conn)).is_empty());
        assert!(p.exists());
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn unrecorded_scratch_is_never_swept() {
        let b = base("unrecorded");
        let legacy = b.join("FS25_A.zip.silo-move-old");
        std::fs::write(&legacy, b"the only copy").unwrap();
        let j = MemJournal::default();
        assert!(sweep(&j).is_empty());
        assert!(legacy.exists());
        let found = log_unrecorded(&j, &b, &|n: &str| n.ends_with(MOVE_OLD));
        assert_eq!(found, vec![legacy.clone()]);
        assert!(legacy.exists(), "logged, never deleted");
        // Found again on the next sweep: still returned (the log note is to_note's call).
        let again = log_unrecorded(&j, &b, &|n: &str| n.ends_with(MOVE_OLD));
        assert_eq!(again, vec![legacy.clone()]);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn a_replacement_with_the_same_size_and_times_is_noted_again() {
        // Deterministic: only the file number differs between the two looks.
        let dir = PathBuf::from("mods");
        let p = dir.join("FS25_A.zip.part");
        let t = Some(std::time::SystemTime::UNIX_EPOCH);
        let file = std::cell::Cell::new(1u64);
        let stamp = |_: &Path| Some((Some((7, file.get())), 3, t, t));
        let part = |n: &str| n.ends_with(".part");
        let mut noted = NotedLeftovers::new();
        let found = std::slice::from_ref(&p);
        assert_eq!(
            to_note(&mut noted, &dir, &part, found, &stamp),
            vec![p.clone()]
        );
        assert!(to_note(&mut noted, &dir, &part, found, &stamp).is_empty());
        file.set(2);
        assert_eq!(
            to_note(&mut noted, &dir, &part, found, &stamp),
            vec![p.clone()]
        );
    }

    #[test]
    fn a_leftover_is_noted_once_while_unchanged_and_again_when_new() {
        let b = base("noted");
        let other = b.join("other");
        std::fs::create_dir_all(&other).unwrap();
        let part = |n: &str| n.ends_with(".part");
        let bak = |n: &str| n.ends_with(".bak");
        let p = b.join("FS25_A.zip.part");
        let q = other.join("FS25_A.zip.part");
        let r = b.join("FS25_A.zip.1.bak");
        std::fs::write(&p, b"one").unwrap();
        std::fs::write(&q, b"one").unwrap();
        std::fs::write(&r, b"one").unwrap();
        let mut noted = NotedLeftovers::new();
        let mut sweep = |dir: &Path, kind: &dyn Fn(&str) -> bool, found: &[PathBuf]| {
            to_note(&mut noted, dir, kind, found, &leftover_stamp)
        };

        assert_eq!(sweep(&b, &part, std::slice::from_ref(&p)), vec![p.clone()]);
        assert!(
            sweep(&b, &part, std::slice::from_ref(&p)).is_empty(),
            "unchanged: not again"
        );
        // The same name in another folder is another leftover.
        assert_eq!(
            sweep(&other, &part, std::slice::from_ref(&q)),
            vec![q.clone()]
        );
        // Another kind of sweep in the same folder neither forgets nor re-notes this one.
        assert_eq!(sweep(&b, &bak, std::slice::from_ref(&r)), vec![r.clone()]);
        assert!(
            sweep(&b, &part, std::slice::from_ref(&p)).is_empty(),
            "other sweeps forgot nothing"
        );
        assert!(sweep(&b, &bak, std::slice::from_ref(&r)).is_empty());

        // Changed in place between sweeps: noted again.
        std::fs::write(&p, b"two").unwrap();
        assert_eq!(sweep(&b, &part, std::slice::from_ref(&p)), vec![p.clone()]);

        // Replaced by another file with the same bytes and modified time: noted again.
        let modified = std::fs::metadata(&p).unwrap().modified().unwrap();
        let swap = b.join("swap");
        std::fs::write(&swap, b"two").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&swap)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        let before = entry_number(&p);
        std::fs::rename(&swap, &p).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().modified().unwrap(), modified);
        assert_ne!(entry_number(&p), before, "a different file now");
        assert_eq!(sweep(&b, &part, std::slice::from_ref(&p)), vec![p.clone()]);

        // Gone, then back: noted again.
        std::fs::remove_file(&p).unwrap();
        assert!(sweep(&b, &part, &[]).is_empty());
        std::fs::write(&p, b"two").unwrap();
        assert_eq!(sweep(&b, &part, std::slice::from_ref(&p)), vec![p.clone()]);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn a_move_temp_goes_only_while_its_original_is_there() {
        let b = base("movetmp");
        let src = b.join("FS25_A.zip");
        std::fs::write(&src, b"PK A").unwrap();
        let dst = b.join("new").join("FS25_A.zip");
        let tmp = unique_beside(&dst, MOVE_TMP);
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        std::fs::write(&tmp, b"PK").unwrap(); // a partial copy
        let j = MemJournal::default();
        j.record(&entry(&tmp, Role::MoveTmp, &src, &dst, "zip"))
            .unwrap();
        // The original is gone (and nothing placed): the temp may be all that's left.
        std::fs::rename(&src, b.join("elsewhere.zip")).unwrap();
        assert_eq!(sweep(&j)[0].1, Swept::Kept);
        assert!(tmp.exists() && j.entries().len() == 1);
        // The original is back: the temp is redundant.
        std::fs::rename(b.join("elsewhere.zip"), &src).unwrap();
        assert_eq!(sweep(&j)[0].1, Swept::Removed);
        assert!(!tmp.exists() && j.entries().is_empty());
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn a_move_temp_hardlinked_to_its_placed_copy_goes() {
        let b = base("movetmplink");
        let dst = b.join("FS25_A.zip");
        std::fs::write(&dst, b"PK A").unwrap();
        let tmp = unique_beside(&dst, MOVE_TMP);
        std::fs::hard_link(&dst, &tmp).unwrap();
        let j = MemJournal::default();
        j.record(&entry(
            &tmp,
            Role::MoveTmp,
            &b.join("gone.zip"),
            &dst,
            "zip",
        ))
        .unwrap();
        assert_eq!(sweep(&j)[0].1, Swept::Removed);
        assert!(dst.exists() && !tmp.exists());
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn an_old_copy_goes_only_once_its_replacement_verifies() {
        let b = base("moveold");
        let src = b.join("FS25_A.zip");
        let dst = b.join("new.zip");
        let old = unique_beside(&src, MOVE_OLD);
        std::fs::write(&old, b"PK A").unwrap();
        let j = MemJournal::default();
        j.record(&entry(&old, Role::MoveOld, &src, &dst, "zip"))
            .unwrap();
        // Replacement different: kept where it is (never put back beside a changed mod).
        std::fs::write(&dst, b"PK B").unwrap();
        assert_eq!(sweep(&j)[0].1, Swept::Kept);
        assert_eq!(std::fs::read(&old).unwrap(), b"PK A");
        assert!(!src.exists() && j.entries().len() == 1);
        // Replacement verified: removed.
        std::fs::write(&dst, b"PK A").unwrap();
        assert_eq!(sweep(&j)[0].1, Swept::Removed);
        assert!(!old.exists() && dst.exists());
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn the_issue_crash_keeps_the_only_copy() {
        // #394: the original is renamed to its old name, Silo stops, the new copy is then
        // lost. On restart the old copy is the only one: it's put back, never deleted.
        let b = base("onlycopy");
        let src = b.join("FS25_A.zip");
        let dst = b.join("new").join("FS25_A.zip");
        let old = unique_beside(&src, MOVE_OLD);
        std::fs::write(&old, b"PK A").unwrap();
        let j = MemJournal::default();
        j.record(&entry(&old, Role::MoveOld, &src, &dst, "zip"))
            .unwrap();
        assert_eq!(sweep(&j)[0].1, Swept::PutBack);
        assert_eq!(std::fs::read(&src).unwrap(), b"PK A");
        // The name taken by something else: kept under the scratch name instead.
        std::fs::rename(&src, &old).unwrap();
        std::fs::write(&src, b"someone else's").unwrap();
        j.record(&entry(&old, Role::MoveOld, &src, &dst, "zip"))
            .unwrap();
        assert_eq!(sweep(&j)[0].1, Swept::Kept);
        assert_eq!(std::fs::read(&old).unwrap(), b"PK A");
        assert_eq!(std::fs::read(&src).unwrap(), b"someone else's");
        assert_eq!(kept_for(&j, &[src.as_path()]), vec![old.clone()]);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn a_half_deleted_old_folder_goes_only_when_its_replacement_holds_every_file_left() {
        let b = base("moveolddir");
        let src = b.join("FS25_D");
        let dst = b.join("new").join("FS25_D");
        let old = unique_beside(&src, MOVE_OLD);
        std::fs::create_dir_all(old.join("sub")).unwrap();
        std::fs::write(old.join("sub/b.xml"), b"b").unwrap(); // a.xml was already deleted
        let j = MemJournal::default();
        j.record(&entry(&old, Role::MoveOld, &src, &dst, "dir"))
            .unwrap();
        // No replacement: a folder is never put back under its name half-deleted; kept.
        assert_eq!(sweep(&j)[0].1, Swept::Kept);
        assert!(old.join("sub/b.xml").exists() && !src.exists());
        // A replacement missing the file left: kept.
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(dst.join("modDesc.xml"), b"d").unwrap();
        assert_eq!(sweep(&j)[0].1, Swept::Kept);
        // Holding it (same bytes): removed.
        std::fs::create_dir_all(dst.join("sub")).unwrap();
        std::fs::write(dst.join("sub/b.xml"), b"b").unwrap();
        assert_eq!(sweep(&j)[0].1, Swept::Removed);
        assert!(!old.exists() && dst.join("sub/b.xml").exists());
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn a_relink_temp_copy_goes_only_while_its_archived_copy_matches() {
        let b = base("relinktmp");
        let archived = b.join("archive.zip");
        std::fs::write(&archived, b"PK A").unwrap();
        let link = b.join("FS25_A.zip");
        let tmp = unique_beside(&link, RELINK_TMP);
        std::fs::write(&tmp, b"PK A").unwrap(); // a copy
        let j = MemJournal::default();
        j.record(&entry(&tmp, Role::RelinkTmp, &archived, &link, "zip"))
            .unwrap();
        std::fs::write(&archived, b"PK changed").unwrap();
        assert_eq!(sweep(&j)[0].1, Swept::Kept);
        std::fs::write(&archived, b"PK A").unwrap();
        assert_eq!(sweep(&j)[0].1, Swept::Removed);
        assert!(!tmp.exists() && archived.exists());
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn a_missing_entry_on_an_unreachable_folder_stays_recorded() {
        let b = base("unreachable");
        let p = b
            .join("gone-drive")
            .join("FS25_A.zip.0123456789abcdef.silo-move-tmp");
        let j = MemJournal::default();
        j.record(&entry(&p, Role::MoveTmp, &b, &b, "zip")).unwrap();
        assert_eq!(sweep(&j)[0].1, Swept::Kept);
        assert_eq!(j.entries().len(), 1);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        assert_eq!(sweep(&j)[0].1, Swept::Gone);
        assert!(j.entries().is_empty());
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn a_move_temp_stays_when_its_original_was_replaced_by_something_else() {
        let b = base("movetmpswap");
        let src = b.join("FS25_A.zip");
        std::fs::write(&src, b"PK mod B, same name").unwrap();
        let dst = b.join("new.zip");
        let tmp = unique_beside(&dst, MOVE_TMP);
        std::fs::write(&tmp, b"PK mod A").unwrap(); // a full copy of what was there before
        let j = MemJournal::default();
        j.record(&entry(&tmp, Role::MoveTmp, &src, &dst, "zip"))
            .unwrap();
        assert_eq!(sweep(&j)[0].1, Swept::Kept);
        assert_eq!(std::fs::read(&tmp).unwrap(), b"PK mod A");
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn a_folder_move_temp_goes_only_when_the_original_holds_every_byte() {
        let b = base("movetmpdir");
        let src = b.join("FS25_D");
        std::fs::create_dir_all(src.join("xml")).unwrap();
        std::fs::write(src.join("modDesc.xml"), b"<modDesc/>").unwrap();
        std::fs::write(src.join("xml/a.xml"), b"<a>long</a>").unwrap();
        let dst = b.join("new").join("FS25_D");
        let tmp = unique_beside(&src, MOVE_TMP);
        std::fs::create_dir_all(tmp.join("xml")).unwrap();
        std::fs::write(tmp.join("modDesc.xml"), b"<modDesc/>").unwrap();
        std::fs::write(tmp.join("xml/a.xml"), b"<a>lo").unwrap(); // cut off part-way
        std::fs::write(tmp.join("extra.txt"), b"not in the original").unwrap();
        let j = MemJournal::default();
        j.record(&entry(&tmp, Role::MoveTmp, &src, &dst, "dir"))
            .unwrap();
        assert_eq!(
            sweep(&j)[0].1,
            Swept::Kept,
            "a file the original doesn't have"
        );
        std::fs::remove_file(tmp.join("extra.txt")).unwrap();
        assert_eq!(sweep(&j)[0].1, Swept::Removed);
        assert!(!tmp.exists() && src.join("xml/a.xml").is_file());
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn a_marked_folder_relink_temp_goes_only_while_the_archive_holds_its_files() {
        let b = base("relinkdir");
        let archived = b.join("archive").join("FS25_D");
        std::fs::create_dir_all(&archived).unwrap();
        std::fs::write(archived.join("modDesc.xml"), b"<modDesc/>").unwrap();
        std::fs::write(archived.join("a.xml"), b"<a/>").unwrap();
        let link = b.join("mods").join("FS25_D");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        let tmp = unique_beside(&link, RELINK_TMP);
        crate::organize::copy_dir_projection(&archived, &tmp).unwrap();
        let j = MemJournal::default();
        j.record(&entry(&tmp, Role::RelinkTmp, &archived, &link, "dir"))
            .unwrap();
        // The archive lost a file: the temp now holds its only copy.
        std::fs::remove_file(archived.join("a.xml")).unwrap();
        assert_eq!(sweep(&j)[0].1, Swept::Kept);
        assert!(tmp.join("a.xml").is_file());
        std::fs::write(archived.join("a.xml"), b"<a/>").unwrap();
        assert_eq!(sweep(&j)[0].1, Swept::Removed);
        assert!(!tmp.exists());
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn an_old_folder_is_never_verified_against_itself_through_a_link() {
        let b = base("moveoldloop");
        let src = b.join("FS25_D");
        let old = unique_beside(&src, MOVE_OLD);
        std::fs::create_dir_all(old.join("sub")).unwrap();
        std::fs::write(old.join("sub/data.xml"), b"the only copy").unwrap();
        let dst = b.join("new").join("FS25_D");
        std::fs::create_dir_all(&dst).unwrap();
        // The replacement's `sub` is a link back into the old copy.
        if crate::organize::link_dir_native(&old.join("sub"), &dst.join("sub")).is_err() {
            let _ = std::fs::remove_dir_all(&b);
            return; // no folder links here
        }
        let j = MemJournal::default();
        j.record(&entry(&old, Role::MoveOld, &src, &dst, "dir"))
            .unwrap();
        assert_eq!(sweep(&j)[0].1, Swept::Kept);
        assert_eq!(
            std::fs::read(old.join("sub/data.xml")).unwrap(),
            b"the only copy"
        );
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn a_file_is_never_proven_redundant_by_itself() {
        let b = base("selfproof");
        let f = b.join("FS25_A.zip");
        std::fs::write(&f, b"PK").unwrap();
        assert!(!same_bytes(&f, &f) && !prefix_of(&f, &f));
        let h = b.join("hard.zip");
        std::fs::hard_link(&f, &h).unwrap();
        assert!(
            same_bytes(&f, &h),
            "a second name of the same file is a distinct name"
        );
        let _ = std::fs::remove_dir_all(&b);
    }

    /// A copy that failed is cleaned up with the same proof as a sweep: one whose original
    /// changed while it was copied is kept (it may be all that's left of what was there).
    #[test]
    fn a_failed_copy_is_removed_only_while_its_original_holds_it() {
        let b = base("discard");
        let src = b.join("FS25_A.zip");
        std::fs::write(&src, b"PK version B").unwrap();
        let tmp = unique_beside(&b.join("new.zip"), MOVE_TMP);
        std::fs::write(&tmp, b"PK version A").unwrap();
        let e = entry(&tmp, Role::MoveTmp, &src, &b.join("new.zip"), "zip");
        let j = MemJournal::default();
        j.record(&e).unwrap();
        assert_eq!(discard(Some(&j), &e), Swept::Kept);
        assert!(tmp.exists() && j.entries().len() == 1);
        std::fs::write(&src, b"PK version A, whole").unwrap();
        assert_eq!(discard(Some(&j), &e), Swept::Removed);
        assert!(!tmp.exists() && j.entries().is_empty());
        // `held_by` (an original before it's deleted) wants every byte, not a prefix.
        let part = b.join("part.zip");
        std::fs::write(&part, b"PK version A").unwrap();
        assert!(redundant_copy(&part, &src) && !held_by(&part, &src));
        let _ = std::fs::remove_dir_all(&b);
    }
}

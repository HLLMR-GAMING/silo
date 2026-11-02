//! SQLite persistence. First use: a scan cache keyed by path+mtime+size that lets
//! warm rescans skip opening/parsing archives entirely. Real indexed tables (a PK
//! on path), not `LIKE 'mods_%'` key-value scans. Versioned, additive migrations
//! (`SCHEMA_VERSION`); we never wipe the cache on version bumps.

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// A cached scan row: the invalidation key (mtime+size) plus the serialized entry.
pub struct CacheEntry {
    pub mtime_ms: u64,
    pub size: u64,
    pub json: String,
}

/// User curation flags, keyed by mod tech-name (stable across moves/versions).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurationRow {
    pub tech_name: String,
    pub favorite: bool,
    pub hidden: bool,
    pub broken: bool,
    #[serde(default)]
    pub rating: i64,
    pub note: Option<String>,
    /// Leave this mod out of update checks and Update all (#333): the user changed it themselves.
    #[serde(default)]
    pub skip_updates: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TagRow {
    pub tech_name: String,
    pub tag: String,
}

/// A mod linked to its GitHub repo for update-checking.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoRow {
    pub tech_name: String,
    pub owner: String,
    pub repo: String,
}

/// The web page a mod came from, saved by the user (#196). Silo never requests it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRow {
    pub tech_name: String,
    pub url: String,
    /// When the user last said they checked the page (unix seconds).
    pub last_checked: Option<i64>,
}

/// A manual category reassignment, keyed by tech-name.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryOverride {
    pub tech_name: String,
    pub category: String,
    pub subcategory: Option<String>,
}

/// Manifest row: a mod Silo has moved into `mods/archive/<category>/`. `active`
/// means it's currently projected (linked) into the flat root for the game.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrganizedRow {
    pub tech_name: String,
    pub file_name: String,
    pub kind: String,
    pub category: String,
    pub subcategory: Option<String>,
    pub active: bool,
}

/// A named, saved set of active mods (a "mod set" in the app).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Loadout {
    pub id: i64,
    pub name: String,
    pub mods: Vec<String>,
    /// `"user"` for a set the user saved, `"auto"` for a "Before …" snapshot Silo took before a
    /// switch (only the newest `AUTO_SNAPSHOTS_KEPT` are kept).
    pub kind: String,
    /// When it was created (unix ms); None for sets saved before schema version 2.
    pub created_at: Option<i64>,
}

/// The kind of a set the user saved.
pub const KIND_USER: &str = "user";
/// The kind of an automatic "Before …" snapshot.
pub const KIND_AUTO: &str = "auto";
/// How many automatic snapshots are kept; saving another drops the oldest.
pub const AUTO_SNAPSHOTS_KEPT: i64 = 5;

/// The schema this build reads and writes. Every migration added to `migrate` bumps it. Version
/// 0 is every database from before versioning (Silo 0.9 and older), whatever its shape.
///
/// Keep migrations additive where possible so an older Silo can still read the file; when one
/// can't be, the version gate in `open` is what protects the user.
pub const SCHEMA_VERSION: i64 = 5;

fn newer_schema(found: i64) -> String {
    format!(
        "Your Silo library was saved by a newer version of Silo (database version {found}; this \
         version reads up to {SCHEMA_VERSION}). Install the latest Silo to keep using it. Nothing \
         was changed."
    )
}

fn schema_version(conn: &Connection) -> Result<i64, String> {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
        .map_err(|e| e.to_string())
}

/// Open the Silo database, bringing an older one up to date first. A database from a newer
/// Silo is refused before anything writes to it.
pub fn open(db_path: &Path) -> Result<Connection, String> {
    if let Some(dir) = db_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    check_once(db_path)?;
    let mut conn = Connection::open(db_path).map_err(|e| e.to_string())?;
    match schema_version(&conn)? {
        v if v == SCHEMA_VERSION => Ok(conn),
        v if v > SCHEMA_VERSION => Err(newer_schema(v)),
        _ => {
            migrate(&mut conn)?;
            Ok(conn)
        }
    }
}

/// Step an older database up to `SCHEMA_VERSION`. Each step runs in a `BEGIN IMMEDIATE`
/// transaction that re-reads the version first, so when two connections (two commands, or two
/// copies of Silo) open an old database at once, one migrates and the other waits for it and
/// then finds nothing to do. A step and its version stamp commit together, so a failed step
/// leaves the database as it was.
fn migrate(conn: &mut Connection) -> Result<(), String> {
    keep_copy_before_migrating(conn);
    // WAL is a property of the file and can't be switched inside a transaction.
    enable_wal(conn)?;
    loop {
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let v = schema_version(&tx)?;
        if v == SCHEMA_VERSION {
            return Ok(()); // someone else finished it; dropping `tx` just ends the read
        }
        if v > SCHEMA_VERSION {
            return Err(newer_schema(v));
        }
        #[cfg(test)]
        tests::count_migration_step(&tx);
        match v {
            0 => migrate_0_to_1(&tx)?,
            1 => migrate_1_to_2(&tx)?,
            2 => migrate_2_to_3(&tx)?,
            3 => migrate_3_to_4(&tx)?,
            4 => migrate_4_to_5(&tx)?,
            other => return Err(format!("no migration from database version {other}")),
        }
        tx.pragma_update(None, "user_version", v + 1)
            .map_err(|e| e.to_string())?;
        tx.commit()
            .map_err(|e| format!("database migration failed: {e}"))?;
    }
}

/// Before the first migration step touches an existing library, keep a verified copy of it as
/// it was: `db-snapshots/silo-before-v<version>.db`. It is never restored automatically or
/// rotated out; it's there if a migration ever goes wrong. One per version (an earlier attempt's
/// copy is kept). Best effort: `VACUUM INTO` can't run inside the migration's transaction, and
/// a copy that fails (no space, or another connection migrated first) doesn't block the upgrade,
/// which is itself all-or-nothing.
fn keep_copy_before_migrating(conn: &Connection) {
    let Ok(from) = schema_version(conn) else {
        return;
    };
    let Some(db_path) = conn.path().filter(|p| !p.is_empty()).map(PathBuf::from) else {
        return;
    };
    if from >= SCHEMA_VERSION || !has_tables(conn).unwrap_or(false) {
        return; // up to date, or a brand-new database: nothing to keep
    }
    let dir = snapshot_dir(&db_path);
    let target = dir.join(format!("silo-before-v{from}.db"));
    if target.exists() {
        return;
    }
    let kept = std::fs::create_dir_all(&dir)
        .map_err(|e| e.to_string())
        .and_then(|()| write_copy(conn, &target, &|v| v == from));
    match kept {
        Ok(()) => log::info!("kept a copy before upgrading: {}", target.display()),
        Err(e) => log::warn!("couldn't keep a copy before upgrading: {e}"),
    }
}

/// Switch the file to WAL so readers can run beside a writer. The switch needs the file to
/// itself for a moment, and SQLite returns "database is locked" at once rather than waiting
/// when another connection is busy with it, so retry for up to the usual 5 second wait.
fn enable_wal(conn: &Connection) -> Result<(), String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match conn.execute_batch("PRAGMA journal_mode=WAL;") {
            Ok(()) => return Ok(()),
            Err(e) if is_busy(&e) && std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20))
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn is_busy(e: &rusqlite::Error) -> bool {
    matches!(
        e.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy) | Some(rusqlite::ErrorCode::DatabaseLocked)
    )
}

/// Version 1 = everything Silo 0.9 had: the base tables, `curation.rating`, and the manifest
/// scoped by mods folder.
fn migrate_0_to_1(tx: &Connection) -> Result<(), String> {
    tx.execute_batch(BASE_SCHEMA).map_err(|e| e.to_string())?;
    if !has_column(tx, "curation", "rating") {
        tx.execute(
            "ALTER TABLE curation ADD COLUMN rating INTEGER NOT NULL DEFAULT 0",
            [],
        )
        .map_err(|e| e.to_string())?;
    }
    scope_by_root(tx)
}

/// Version 2 = mod set kinds: `loadout.kind` (`user` / `auto`, every existing set is `user`)
/// and `loadout.created_at`. Additive, so an older Silo's queries still work on the file.
fn migrate_1_to_2(tx: &Connection) -> Result<(), String> {
    if !has_column(tx, "loadout", "kind") {
        tx.execute(
            "ALTER TABLE loadout ADD COLUMN kind TEXT NOT NULL DEFAULT 'user'",
            [],
        )
        .map_err(|e| e.to_string())?;
    }
    if !has_column(tx, "loadout", "created_at") {
        tx.execute("ALTER TABLE loadout ADD COLUMN created_at INTEGER", [])
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Version 3 = `curation.skip_updates` (#333): mods the user leaves out of update checks. Additive;
/// every existing mod keeps being checked.
fn migrate_2_to_3(tx: &Connection) -> Result<(), String> {
    if !has_column(tx, "curation", "skip_updates") {
        tx.execute(
            "ALTER TABLE curation ADD COLUMN skip_updates INTEGER NOT NULL DEFAULT 0",
            [],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Version 4 = `catalog_alias` (#386): which catalog entry a zip Silo installed from Browse came
/// from, when the file isn't named after the entry's techName (`FS25_ToDo` downloads as
/// `FS25_FieldToDoList.zip`). Keyed by the zip's own name and its whole-file SHA-256, so the
/// link belongs to those exact bytes: a different build under the same name never matches.
/// `size` only spares hashing a file that can't match; `download_url` is where the file came
/// from (an update from the same GitHub repository keeps the link). Additive.
fn migrate_3_to_4(tx: &Connection) -> Result<(), String> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS catalog_alias (
             tech_name         TEXT NOT NULL,
             archive_sha256    TEXT NOT NULL,
             size              INTEGER NOT NULL,
             catalog_id        TEXT NOT NULL,
             catalog_tech_name TEXT,
             download_url      TEXT,
             created_at        INTEGER NOT NULL,
             PRIMARY KEY (tech_name, archive_sha256)
         );",
    )
    .map_err(|e| e.to_string())
}

/// Version 5 = `alias_check` (#402): what the catalog-link backfill noted about each library zip,
/// keyed by path with the size and modified time it saw (as the scan cache is). `decided` = that
/// version of the file is settled and isn't hashed or looked up again; a changed file has another
/// size or time and is looked at once more. `sha256` keeps the hash once taken, so a lookup that
/// failed is retried without hashing the file again. `unnamed_ms` is when the catalog last said it
/// has no entry with the file's techName, so the next runs hash it without asking again (for a
/// day). `attempted_ms` orders retries behind files never looked at. Rows for files the scan no longer has are dropped with `prune_alias_checks`.
/// Additive.
fn migrate_4_to_5(tx: &Connection) -> Result<(), String> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS alias_check (
             path         TEXT PRIMARY KEY,
             size         INTEGER NOT NULL,
             mtime_ms     INTEGER NOT NULL,
             sha256       TEXT,
             unnamed_ms   INTEGER,
             decided      INTEGER NOT NULL DEFAULT 0,
             attempted_ms INTEGER NOT NULL
         );",
    )
    .map_err(|e| e.to_string())
}

// ── damaged-database recovery and snapshots ──

/// Where startup snapshots of the database live (beside it, in the app data folder).
pub fn snapshot_dir(db_path: &Path) -> PathBuf {
    db_path.with_file_name("db-snapshots")
}

/// Database files checked this run (`check_once`), each with its refusal: `Some(why)` when the
/// file was judged damaged but couldn't be set aside, so it must not be opened this run.
type Checked = Vec<(PathBuf, Option<String>)>;
static CHECKED: std::sync::Mutex<Checked> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
fn forget_checked(path: &Path) {
    CHECKED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(p, _)| p != path);
}

/// Once per run per file, before first use (`examine`). Every later open of a file that was
/// judged damaged but couldn't be set aside gets the same refusal instead of opening it.
fn check_once(db_path: &Path) -> Result<(), String> {
    let mut checked = CHECKED.lock().unwrap_or_else(|e| e.into_inner());
    let refusal = match checked.iter().find(|(p, _)| p == db_path) {
        Some((_, refusal)) => refusal.clone(),
        None => {
            let refusal = examine(db_path);
            checked.push((db_path.to_path_buf(), refusal.clone()));
            refusal
        }
    };
    refusal.map_or(Ok(()), Err)
}

/// If SQLite reports the file damaged, set it aside, put back the newest good snapshot (or let
/// `open` start a new database), and leave a notice. A database that is busy, locked or
/// unreadable for any other reason is never touched. Returns a refusal only when the damaged
/// file is still in place.
fn examine(db_path: &Path) -> Option<String> {
    let snapshots = snapshot_dir(db_path);
    if !db_path.exists() {
        // Gone, not damaged: maybe on purpose. Say where the backups are; don't restore one.
        if newest_good_snapshot(&snapshots).is_some() {
            crate::notices::push(format!(
                "Silo's library database wasn't found, so Silo started a new one. Automatic \
                 backups of the old one are in {}; Silo didn't put one back on its own.",
                snapshots.display()
            ));
        }
        return None;
    }
    if !db_path.is_file() {
        return None;
    }
    // A newer Silo's file may use schema this SQLite can't parse, which reads as "malformed".
    // That's not damage: leave it for `open` to refuse untouched.
    if stamped_version(db_path).is_some_and(|v| v > SCHEMA_VERSION) {
        return None;
    }
    // An emptied file (0 bytes, or no tables at all) is as good as damaged when a backup holds
    // the library; with no backup it's simply a new database.
    let damaged = if is_empty_db(db_path) {
        newest_good_snapshot(&snapshots).is_some()
    } else {
        is_damaged(db_path)
    };
    if !damaged {
        return None;
    }
    match recover(db_path, &snapshots) {
        Ok(notice) => {
            crate::notices::push(notice);
            None
        }
        Err(refusal) => {
            crate::notices::push(refusal.clone());
            Some(refusal)
        }
    }
}

/// `std::fs::rename`, with failures injectable in tests.
fn rename(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if tests::rename_fails(from) {
        return Err(std::io::Error::other("injected rename failure"));
    }
    std::fs::rename(from, to)
}

fn is_corruption(e: &rusqlite::Error) -> bool {
    matches!(
        e.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseCorrupt) | Some(rusqlite::ErrorCode::NotADatabase)
    )
}

/// A read-only connection: it never writes, and never checkpoints a -wal into the main file.
fn open_read_only(path: &Path) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
}

/// The version stamped in the file, without parsing its schema: `PRAGMA user_version` on a
/// read-only connection (which also sees a stamp still in the -wal), else the header's bytes
/// 60-63.
fn stamped_version(path: &Path) -> Option<i64> {
    let from_sqlite = open_read_only(path)
        .ok()
        .and_then(|c| schema_version(&c).ok());
    from_sqlite.or_else(|| {
        use std::io::Read;
        let mut header = [0u8; 64];
        std::fs::File::open(path)
            .and_then(|mut f| f.read_exact(&mut header))
            .ok()?;
        let version = i32::from_be_bytes([header[60], header[61], header[62], header[63]]);
        header
            .starts_with(b"SQLite format 3\0")
            .then_some(i64::from(version))
    })
}

/// 0 bytes, or a readable pre-versioning file with no tables: nothing of the library in it.
fn is_empty_db(path: &Path) -> bool {
    if std::fs::metadata(path).is_ok_and(|m| m.len() == 0) {
        return true;
    }
    let Ok(c) = open_read_only(path) else {
        return false;
    };
    schema_version(&c).is_ok_and(|v| v == 0) && has_tables(&c).is_ok_and(|t| !t)
}

fn is_damaged(path: &Path) -> bool {
    let Ok(c) = open_read_only(path) else {
        return false;
    };
    match c.query_row("PRAGMA quick_check(1)", [], |r| r.get::<_, String>(0)) {
        Ok(verdict) => verdict != "ok",
        Err(e) => is_corruption(&e),
    }
}

fn unix_now() -> std::time::Duration {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
}

/// `silo.db` (and its -wal / -shm) renamed to `silo.db.damaged-<unix secs>`, never over an
/// earlier damaged file; the main file's new path is returned. The -wal goes first: left
/// beside a restored database, SQLite would replay it into that database. If any rename fails,
/// the ones already done are undone, so the file and its journal stay together.
fn set_db_aside(path: &Path) -> std::io::Result<PathBuf> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "silo.db".into());
    let secs = unix_now().as_secs();
    let taken = |stem: &str| {
        ["", "-wal", "-shm"]
            .iter()
            .any(|s| path.with_file_name(format!("{stem}{s}")).exists())
    };
    let stem = (0u32..)
        .map(|n| match n {
            0 => format!("{name}.damaged-{secs}"),
            n => format!("{name}.damaged-{secs}.{n}"),
        })
        .find(|stem| !taken(stem))
        .unwrap_or_else(|| format!("{name}.damaged-{secs}"));
    let mut moves: Vec<(PathBuf, PathBuf)> = ["-wal", "-shm"]
        .iter()
        .map(|s| {
            (
                path.with_file_name(format!("{name}{s}")),
                path.with_file_name(format!("{stem}{s}")),
            )
        })
        .filter(|(from, _)| from.exists())
        .collect();
    moves.push((path.to_path_buf(), path.with_file_name(&stem)));
    for (i, (from, to)) in moves.iter().enumerate() {
        if let Err(e) = rename(from, to) {
            for (back_to, back_from) in moves[..i].iter().rev() {
                let _ = rename(back_from, back_to);
            }
            return Err(e);
        }
    }
    Ok(path.with_file_name(&stem))
}

fn now_ms() -> u64 {
    unix_now().as_millis() as u64
}

/// The number in a snapshot's name, `silo-<unix_ms>.db`; None for any other file (a `.tmp`
/// still being written, a pre-migration copy).
fn snapshot_ms(name: &str) -> Option<u64> {
    let digits = name.strip_prefix("silo-")?.strip_suffix(".db")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Snapshot files in `dir`, newest first. One dated after `now_ms` (written while the clock was
/// wrong) is suspect: those sort last, so they're never "the newest" and are pruned first.
fn snapshots_newest_first(dir: &Path, now_ms: u64) -> Vec<(u64, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(u64, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let ms = snapshot_ms(&e.file_name().to_string_lossy())?;
            let path = e.path();
            path.is_file().then_some((ms, path))
        })
        .collect();
    found.sort_by_key(|(ms, _)| (*ms > now_ms, std::cmp::Reverse(*ms)));
    found
}

/// Read-only check of a database copy: SQLite reads it cleanly, it holds Silo's tables (an
/// empty file passes `quick_check`), and its version passes `version_ok`.
fn verify_copy(path: &Path, version_ok: &dyn Fn(i64) -> bool) -> Result<(), String> {
    let c = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| e.to_string())?;
    let verdict: String = c
        .query_row("PRAGMA quick_check(1)", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if verdict != "ok" {
        return Err(format!("the copy is damaged ({verdict})"));
    }
    if !has_tables(&c)? {
        return Err("the copy is empty".into());
    }
    let v = schema_version(&c)?;
    if !version_ok(v) {
        return Err(format!("the copy has database version {v}"));
    }
    Ok(())
}

fn has_tables(conn: &Connection) -> Result<bool, String> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table')",
        [],
        |r| r.get(0),
    )
    .map_err(|e| e.to_string())
}

/// A snapshot that can stand in for the database: clean, not empty, and a version this Silo
/// reads (a pre-versioning 0 never comes from `snapshot`, so it means something went wrong).
fn is_usable_snapshot(path: &Path) -> bool {
    verify_copy(path, &|v| (1..=SCHEMA_VERSION).contains(&v)).is_ok()
}

fn newest_good_snapshot(dir: &Path) -> Option<PathBuf> {
    snapshots_newest_first(dir, now_ms())
        .into_iter()
        .map(|(_, path)| path)
        .find(|path| is_usable_snapshot(path))
}

/// A unique scratch name beside `target` for a copy being written.
fn tmp_beside(target: &Path) -> PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    target.with_file_name(format!("{name}.{}-{n}.tmp", std::process::id()))
}

/// Write a consistent copy of `conn`'s database to `target`: `VACUUM INTO` a scratch file
/// beside it, drop secrets, verify it, and only then rename it into place. On any failure the
/// scratch file is removed and nothing is left at `target`.
fn write_copy(
    conn: &Connection,
    target: &Path,
    version_ok: &dyn Fn(i64) -> bool,
) -> Result<(), String> {
    let tmp = tmp_beside(target);
    let made = conn
        .execute("VACUUM INTO ?1", [tmp.to_string_lossy().into_owned()])
        .map_err(|e| e.to_string())
        .and_then(|_| scrub_secrets(&tmp))
        .and_then(|()| verify_copy(&tmp, version_ok))
        .and_then(|()| std::fs::rename(&tmp, target).map_err(|e| e.to_string()));
    if made.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    made
}

/// Scratch copies (`silo-*.tmp`) an earlier run left behind. Only ones older than an hour, so a
/// copy another thread is writing right now is never touched.
fn remove_stale_tmps(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > std::time::Duration::from_secs(3600));
        if name.starts_with("silo-") && name.ends_with(".tmp") && old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Set the damaged file aside and put a snapshot in its place. `Err` (a refusal) only when the
/// damaged file couldn't be moved; every other outcome is a notice.
fn recover(db_path: &Path, snapshots: &Path) -> Result<String, String> {
    let kept = set_db_aside(db_path).map_err(|e| {
        format!(
            "Silo's library database ({}) is damaged and couldn't be set aside ({e}). Close any \
             other copy of Silo and start it again.",
            db_path.display()
        )
    })?;
    match newest_good_snapshot(snapshots) {
        Some(snap) => match restore(&snap, db_path) {
            Ok(()) => Ok(format!(
                "Silo's library database was damaged, so Silo restored its latest automatic backup \
                 ({}). Tags, notes or mod sets changed after that backup may be missing. The \
                 damaged file was kept at {}.",
                snap.file_name().unwrap_or_default().to_string_lossy(),
                kept.display()
            )),
            Err(e) => Ok(format!(
                "Silo's library database was damaged and Silo couldn't put back its latest \
                 automatic backup ({e}), so it started a new one. The damaged file was kept at {}, \
                 and the backups are in {}.",
                kept.display(),
                snapshots.display()
            )),
        },
        None => Ok(format!(
            "Silo's library database was damaged and no backup copy could be used, so Silo \
             started a new one. Your mods are untouched and the next scan finds them again; tags, \
             notes, ratings and mod sets couldn't be recovered. The damaged file was kept at {}.",
            kept.display()
        )),
    }
}

/// Copy `snap` to `<db>.restore-tmp`, then rename it into place, so `db_path` is never a
/// half-written copy. A leftover scratch file is removed.
fn restore(snap: &Path, db_path: &Path) -> Result<(), String> {
    let name = db_path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = db_path.with_file_name(format!("{name}.restore-tmp"));
    let _ = std::fs::remove_file(&tmp);
    let done = std::fs::copy(snap, &tmp)
        .and_then(|_| rename(&tmp, db_path))
        .map_err(|e| e.to_string());
    if done.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    done?;
    // A snapshot isn't in WAL mode, and an up-to-date database never migrates again.
    if let Ok(c) = Connection::open(db_path) {
        let _ = enable_wal(&c);
    }
    Ok(())
}

/// Copy the database to `dir/silo-<unix_ms>.db` (`write_copy`: consistent even while Silo runs,
/// verified before it gets that name) unless the newest snapshot is younger than `min_gap`,
/// then keep only the newest `keep`.
pub fn snapshot(
    conn: &Connection,
    dir: &Path,
    keep: usize,
    min_gap: std::time::Duration,
) -> Result<Option<PathBuf>, String> {
    let now = now_ms();
    let newest = snapshots_newest_first(dir, now)
        .first()
        .map(|(ms, _)| *ms)
        .filter(|ms| *ms <= now);
    if newest.is_some_and(|ms| now - ms < min_gap.as_millis() as u64) {
        return Ok(None);
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    remove_stale_tmps(dir);
    let (ms, target) = (now..)
        .map(|ms| (ms, dir.join(format!("silo-{ms}.db"))))
        .find(|(_, t)| !t.exists())
        .ok_or("no free snapshot name")?;
    write_copy(conn, &target, &|v| v == SCHEMA_VERSION)?;
    for (_, old) in snapshots_newest_first(dir, now_ms().max(ms))
        .into_iter()
        .skip(keep)
    {
        let _ = std::fs::remove_file(old);
    }
    Ok(Some(target))
}

/// Drop secrets from a fresh copy, free pages included. A token is only in the database where
/// there's no OS keychain (or as an older build's copy not yet moved); a backup must not keep
/// it after the user logs out.
fn scrub_secrets(copy: &Path) -> Result<(), String> {
    let c = Connection::open(copy).map_err(|e| e.to_string())?;
    let has_settings: bool = c
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'app_setting')",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if !has_settings {
        return Ok(()); // a very old database: nothing to drop
    }
    let mut removed = 0;
    for key in crate::secrets::SECRET_KEYS {
        removed += c
            .execute("DELETE FROM app_setting WHERE key = ?1", [key])
            .map_err(|e| e.to_string())?;
    }
    if removed > 0 {
        c.execute_batch("VACUUM").map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Every table as of schema version 1. Only `migrate_0_to_1` runs it.
const BASE_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS mod_cache (
             path      TEXT PRIMARY KEY,
             mtime_ms  INTEGER NOT NULL,
             size      INTEGER NOT NULL,
             json      TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS curation (
             tech_name TEXT PRIMARY KEY,
             favorite  INTEGER NOT NULL DEFAULT 0,
             hidden    INTEGER NOT NULL DEFAULT 0,
             broken    INTEGER NOT NULL DEFAULT 0,
             note      TEXT
         );
         CREATE TABLE IF NOT EXISTS category_override (
             tech_name   TEXT PRIMARY KEY,
             category    TEXT NOT NULL,
             subcategory TEXT
         );
         CREATE TABLE IF NOT EXISTS organized (
             root        TEXT NOT NULL DEFAULT '',
             tech_name   TEXT NOT NULL,
             file_name   TEXT NOT NULL,
             kind        TEXT NOT NULL,
             category    TEXT NOT NULL,
             subcategory TEXT,
             active      INTEGER NOT NULL DEFAULT 0,
             PRIMARY KEY (root, tech_name)
         );
         CREATE TABLE IF NOT EXISTS loadout (
             id   INTEGER PRIMARY KEY AUTOINCREMENT,
             name TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS loadout_mod (
             loadout_id INTEGER NOT NULL,
             tech_name  TEXT NOT NULL,
             PRIMARY KEY (loadout_id, tech_name)
         );
         CREATE TABLE IF NOT EXISTS mod_tag (
             tech_name TEXT NOT NULL,
             tag       TEXT NOT NULL,
             PRIMARY KEY (tech_name, tag)
         );
         CREATE TABLE IF NOT EXISTS mod_repo (
             tech_name TEXT PRIMARY KEY,
             owner     TEXT NOT NULL,
             repo      TEXT NOT NULL
         );
         -- The web page each mod came from, saved by the user (#196). Silo never requests it;
         -- the browser opens it. last_checked is when the user said they looked (unix secs).
         CREATE TABLE IF NOT EXISTS mod_source (
             tech_name    TEXT PRIMARY KEY,
             url          TEXT NOT NULL,
             last_checked INTEGER
         );
         CREATE TABLE IF NOT EXISTS app_setting (
             key   TEXT PRIMARY KEY,
             value TEXT NOT NULL
         );
         -- Provenance for projected files: when Silo writes a mod into the flat root it records
         -- the file's (size, mtime). If those still match, the file is provably Silo's own
         -- untouched projection — so it can be removed without a full content-hash. A file the
         -- user swapped in has a different mtime and won't match, so it's never wrongly deleted.
         CREATE TABLE IF NOT EXISTS projection (
             root      TEXT NOT NULL DEFAULT '',
             file_name TEXT NOT NULL,
             size      INTEGER NOT NULL,
             mtime_ms  INTEGER NOT NULL,
             PRIMARY KEY (root, file_name)
         );";

fn has_column(conn: &Connection, table: &str, col: &str) -> bool {
    let Ok(mut st) = conn.prepare(&format!("PRAGMA table_info({table})")) else {
        return false;
    };
    let names = st.query_map([], |r| r.get::<_, String>(1));
    names
        .map(|it| it.flatten().any(|n| n == col))
        .unwrap_or(false)
}

/// v0.8 (#162): scope the organize manifest and projection records to a mods folder. SQLite
/// can't change a primary key in place, so both tables are rebuilt in one transaction; old
/// rows get `root = ''` until `claim_unscoped` stamps them with the default folder. Skipped
/// when already migrated. Runs inside the caller's migration transaction, so a failure rolls
/// back with it and leaves the old schema intact.
fn scope_by_root(conn: &Connection) -> Result<(), String> {
    if has_column(conn, "organized", "root") && has_column(conn, "projection", "root") {
        return Ok(());
    }
    conn.execute_batch(
        "CREATE TABLE organized_new (
             root TEXT NOT NULL DEFAULT '', tech_name TEXT NOT NULL, file_name TEXT NOT NULL,
             kind TEXT NOT NULL, category TEXT NOT NULL, subcategory TEXT,
             active INTEGER NOT NULL DEFAULT 0, PRIMARY KEY (root, tech_name));
         INSERT INTO organized_new(root, tech_name, file_name, kind, category, subcategory, active)
             SELECT '', tech_name, file_name, kind, category, subcategory, active FROM organized;
         DROP TABLE organized;
         ALTER TABLE organized_new RENAME TO organized;
         CREATE TABLE projection_new (
             root TEXT NOT NULL DEFAULT '', file_name TEXT NOT NULL, size INTEGER NOT NULL,
             mtime_ms INTEGER NOT NULL, PRIMARY KEY (root, file_name));
         INSERT INTO projection_new(root, file_name, size, mtime_ms)
             SELECT '', file_name, size, mtime_ms FROM projection;
         DROP TABLE projection;
         ALTER TABLE projection_new RENAME TO projection;",
    )
    .map_err(|e| format!("database migration failed: {e}"))
}

/// Stamp rows written before folders were tracked (`root = ''`) with `root`. Before v0.8 Silo
/// only ever managed the default folder, so callers pass that folder's key. Idempotent; a
/// stale unscoped row that collides with an existing scoped one is dropped.
pub fn claim_unscoped(conn: &Connection, root: &str) -> Result<(), String> {
    for sql in [
        "UPDATE OR IGNORE organized SET root = ?1 WHERE root = ''",
        "UPDATE OR IGNORE projection SET root = ?1 WHERE root = ''",
    ] {
        conn.execute(sql, [root]).map_err(|e| e.to_string())?;
    }
    conn.execute_batch(
        "DELETE FROM organized WHERE root = ''; DELETE FROM projection WHERE root = '';",
    )
    .map_err(|e| e.to_string())
}

/// Every mods folder (identity key) Silo has organized mods for — also one it only reached
/// through FS25's override, which never joined the known list. A failed read is an error, never
/// "none".
pub fn organized_roots(conn: &Connection) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare("SELECT DISTINCT root FROM organized WHERE root <> '' ORDER BY root")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

/// How many mods Silo has organized in this folder.
pub fn count_organized(conn: &Connection, root: &str) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM organized WHERE root = ?1",
        [root],
        |r| r.get(0),
    )
    .unwrap_or(0)
}

/// Load all tags, grouped later by the caller.
pub fn load_tags(conn: &Connection) -> Vec<TagRow> {
    let Ok(mut stmt) =
        conn.prepare("SELECT tech_name, tag FROM mod_tag ORDER BY tag COLLATE NOCASE")
    else {
        return Vec::new();
    };
    let rows = stmt.query_map([], |r| {
        Ok(TagRow {
            tech_name: r.get(0)?,
            tag: r.get(1)?,
        })
    });
    rows.map(|r| r.flatten().collect()).unwrap_or_default()
}

/// Generic app setting get/set (GitHub client id, token, user, …).
pub fn get_app_setting(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM app_setting WHERE key = ?1", [key], |r| {
        r.get(0)
    })
    .ok()
}

/// `get_app_setting`, but a failed read is an error, never "not set" (#415: Play's move check
/// must not read a broken DB as "no move").
pub fn try_get_app_setting(conn: &Connection, key: &str) -> Result<Option<String>, String> {
    use rusqlite::OptionalExtension;
    conn.query_row("SELECT value FROM app_setting WHERE key = ?1", [key], |r| {
        r.get(0)
    })
    .optional()
    .map_err(|e| e.to_string())
}

pub fn set_app_setting(conn: &Connection, key: &str, value: Option<&str>) -> Result<(), String> {
    match value {
        None => conn.execute("DELETE FROM app_setting WHERE key = ?1", [key]),
        Some(v) => conn.execute(
            "INSERT INTO app_setting(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![key, v],
        ),
    }
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Every app setting whose key starts with `prefix`, as (key, value) pairs. Matched exactly
/// (no `LIKE` wildcards), in key order. A failed read is an error, never "none".
pub fn app_settings_with_prefix(
    conn: &Connection,
    prefix: &str,
) -> Result<Vec<(String, String)>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT key, value FROM app_setting WHERE substr(key, 1, length(?1)) = ?1 ORDER BY key",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([prefix], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

/// Rewrite the database file, reclaiming free pages. SQLite leaves deleted row bytes in
/// free pages until this runs — so after scrubbing a legacy plaintext secret we VACUUM to
/// ensure the old token can't be carved out of the file. In WAL mode the old pages also sit
/// in `silo.db-wal` until a checkpoint, so it checkpoints and truncates the WAL too.
/// Best-effort; never fatal (a checkpoint another connection blocks just runs later).
pub fn vacuum(conn: &Connection) {
    let _ = conn.execute_batch("VACUUM");
    let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
}

/// Load all mod→repo links.
pub fn load_repos(conn: &Connection) -> Vec<RepoRow> {
    let Ok(mut stmt) = conn.prepare("SELECT tech_name, owner, repo FROM mod_repo") else {
        return Vec::new();
    };
    let rows = stmt.query_map([], |r| {
        Ok(RepoRow {
            tech_name: r.get(0)?,
            owner: r.get(1)?,
            repo: r.get(2)?,
        })
    });
    rows.map(|r| r.flatten().collect()).unwrap_or_default()
}

/// Set (or clear, when owner is empty) a mod's GitHub repo link.
pub fn set_repo(conn: &Connection, tech_name: &str, owner: &str, repo: &str) -> Result<(), String> {
    if owner.trim().is_empty() || repo.trim().is_empty() {
        conn.execute("DELETE FROM mod_repo WHERE tech_name = ?1", [tech_name])
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    conn.execute(
        "INSERT INTO mod_repo(tech_name, owner, repo) VALUES(?1, ?2, ?3)
         ON CONFLICT(tech_name) DO UPDATE SET owner = excluded.owner, repo = excluded.repo",
        rusqlite::params![tech_name, owner.trim(), repo.trim()],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// True when the mod has a GitHub repo linked.
pub fn has_repo(conn: &Connection, tech_name: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM mod_repo WHERE tech_name = ?1",
        [tech_name],
        |_| Ok(()),
    )
    .is_ok()
}

/// Load every saved source page.
pub fn load_sources(conn: &Connection) -> Vec<SourceRow> {
    let Ok(mut stmt) = conn.prepare("SELECT tech_name, url, last_checked FROM mod_source") else {
        return Vec::new();
    };
    let rows = stmt.query_map([], |r| {
        Ok(SourceRow {
            tech_name: r.get(0)?,
            url: r.get(1)?,
            last_checked: r.get(2)?,
        })
    });
    rows.map(|r| r.flatten().collect()).unwrap_or_default()
}

/// Save a mod's source page, or remove it when `url` is None or blank. The caller validates.
/// Saving the same page keeps its last-checked date; a different page starts unchecked.
pub fn set_source(conn: &Connection, tech_name: &str, url: Option<&str>) -> Result<(), String> {
    let url = url.map(str::trim).unwrap_or("");
    if url.is_empty() {
        conn.execute("DELETE FROM mod_source WHERE tech_name = ?1", [tech_name])
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    conn.execute(
        "INSERT INTO mod_source(tech_name, url, last_checked) VALUES(?1, ?2, NULL)
         ON CONFLICT(tech_name) DO UPDATE SET
             last_checked = CASE WHEN mod_source.url = excluded.url
                                 THEN mod_source.last_checked ELSE NULL END,
             url = excluded.url",
        rusqlite::params![tech_name, url],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Record that the user checked a mod's source page at `now` (unix seconds).
/// @return false when the mod has no saved page
pub fn mark_source_checked(conn: &Connection, tech_name: &str, now: i64) -> Result<bool, String> {
    let n = conn
        .execute(
            "UPDATE mod_source SET last_checked = ?2 WHERE tech_name = ?1",
            rusqlite::params![tech_name, now],
        )
        .map_err(|e| e.to_string())?;
    Ok(n > 0)
}

/// One `catalog_alias` row (#386): the zip `tech_name` (its file name without `.zip`) with these
/// exact bytes was installed by Silo from catalog entry `catalog_id`. See `catalogalias`.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogAliasRow {
    pub tech_name: String,
    /// Lowercase hex SHA-256 of the whole zip as installed.
    pub archive_sha256: String,
    pub size: i64,
    pub catalog_id: String,
    /// The entry's techName when it was installed (None when the catalog had none).
    pub catalog_tech_name: Option<String>,
    /// The address the file was downloaded from.
    pub download_url: Option<String>,
}

/// Save a catalog alias, replacing the row for the same file name and bytes.
pub fn record_catalog_alias(conn: &Connection, row: &CatalogAliasRow) -> Result<(), String> {
    let now = unix_now().as_secs() as i64;
    conn.execute(
        "INSERT INTO catalog_alias(tech_name, archive_sha256, size, catalog_id,
                                   catalog_tech_name, download_url, created_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(tech_name, archive_sha256) DO UPDATE SET
             size = excluded.size, catalog_id = excluded.catalog_id,
             catalog_tech_name = excluded.catalog_tech_name,
             download_url = excluded.download_url, created_at = excluded.created_at",
        rusqlite::params![
            row.tech_name,
            row.archive_sha256.to_ascii_lowercase(),
            row.size,
            row.catalog_id,
            row.catalog_tech_name,
            row.download_url,
            now
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Every catalog alias recorded for a zip named `tech_name` (one per build installed), newest
/// first. An error when the table or a row can't be read: a link that can't be checked must
/// not read as no link.
pub fn catalog_aliases(conn: &Connection, tech_name: &str) -> Result<Vec<CatalogAliasRow>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT tech_name, archive_sha256, size, catalog_id, catalog_tech_name, download_url
             FROM catalog_alias WHERE tech_name = ?1 ORDER BY created_at DESC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt.query_map([tech_name], |r| {
        Ok(CatalogAliasRow {
            tech_name: r.get(0)?,
            archive_sha256: r.get(1)?,
            size: r.get(2)?,
            catalog_id: r.get(3)?,
            catalog_tech_name: r.get(4)?,
            download_url: r.get(5)?,
        })
    });
    rows.and_then(|r| r.collect::<rusqlite::Result<Vec<_>>>())
        .map_err(|e| e.to_string())
}

/// Remove the catalog alias for this zip name and hash, if there is one.
pub fn forget_catalog_alias(
    conn: &Connection,
    tech_name: &str,
    archive_sha256: &str,
) -> Result<(), String> {
    conn.execute(
        "DELETE FROM catalog_alias WHERE tech_name = ?1 AND archive_sha256 = ?2",
        rusqlite::params![tech_name, archive_sha256],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// The mods the user left out of update checks (#333). An error when that can't be read, so a
/// caller that must leave them out never takes "none".
pub fn skip_update_names(conn: &Connection) -> Result<HashSet<String>, String> {
    let mut stmt = conn
        .prepare("SELECT tech_name FROM curation WHERE skip_updates != 0")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    rows.collect::<rusqlite::Result<HashSet<_>>>()
        .map_err(|e| e.to_string())
}

/// What the catalog-link backfill (#402) noted about one version of a library file.
#[derive(Debug, Clone, PartialEq)]
pub struct AliasCheck {
    pub size: u64,
    pub mtime_ms: u64,
    /// The file's whole-file SHA-256 at that size and time, once taken (lowercase hex).
    pub sha256: Option<String>,
    /// When the catalog last said it has no entry with the file's techName (unix milliseconds).
    pub unnamed_ms: Option<i64>,
    /// Settled: not looked at again until the file changes.
    pub decided: bool,
    /// When it was last looked at (unix milliseconds).
    pub attempted_ms: i64,
}

/// What the catalog-link backfill noted about the file at `path`, for whatever version of it it
/// last saw.
pub fn alias_check(conn: &Connection, path: &str) -> Result<Option<AliasCheck>, String> {
    conn.query_row(
        "SELECT size, mtime_ms, sha256, unnamed_ms, decided, attempted_ms
         FROM alias_check WHERE path = ?1",
        [path],
        |r| {
            Ok(AliasCheck {
                size: r.get::<_, i64>(0)? as u64,
                mtime_ms: r.get::<_, i64>(1)? as u64,
                sha256: r.get(2)?,
                unnamed_ms: r.get(3)?,
                decided: r.get(4)?,
                attempted_ms: r.get(5)?,
            })
        },
    )
    .optional()
    .map_err(|e| e.to_string())
}

/// Save what the catalog-link backfill noted about the file at `path`, replacing what it noted
/// for an earlier version of the file.
pub fn note_alias_check(conn: &Connection, path: &str, check: &AliasCheck) -> Result<(), String> {
    put_alias_check(conn, path, check)
}

/// The latest try the catalog-link backfill noted for any file, if any.
pub fn last_alias_attempt(conn: &Connection) -> Result<Option<i64>, String> {
    conn.query_row("SELECT MAX(attempted_ms) FROM alias_check", [], |r| {
        r.get(0)
    })
    .map_err(|e| e.to_string())
}

/// `note_alias_check` for several files in one transaction (one wait for the database, not one
/// per file).
pub fn note_alias_checks(conn: &Connection, checks: &[(String, AliasCheck)]) -> Result<(), String> {
    if checks.is_empty() {
        return Ok(());
    }
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    for (path, check) in checks {
        put_alias_check(&tx, path, check)?;
    }
    tx.commit().map_err(|e| e.to_string())
}

fn put_alias_check(conn: &Connection, path: &str, check: &AliasCheck) -> Result<(), String> {
    conn.execute(
        "INSERT INTO alias_check(path, size, mtime_ms, sha256, unnamed_ms, decided, attempted_ms)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(path) DO UPDATE SET
             size = excluded.size, mtime_ms = excluded.mtime_ms, sha256 = excluded.sha256,
             unnamed_ms = excluded.unnamed_ms, decided = excluded.decided,
             attempted_ms = excluded.attempted_ms",
        rusqlite::params![
            path,
            check.size as i64,
            check.mtime_ms as i64,
            check.sha256.as_deref().map(str::to_ascii_lowercase),
            check.unnamed_ms,
            check.decided,
            check.attempted_ms
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Drop the backfill's notes for files the scan cache no longer has (removed or moved), so the
/// table stays the size of the library.
/// @returns how many were dropped
pub fn prune_alias_checks(conn: &Connection) -> Result<usize, String> {
    conn.execute(
        "DELETE FROM alias_check WHERE path NOT IN (SELECT path FROM mod_cache)",
        [],
    )
    .map_err(|e| e.to_string())
}

/// Replace all tags for a mod.
pub fn set_tags(conn: &mut Connection, tech_name: &str, tags: &[String]) -> Result<(), String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM mod_tag WHERE tech_name = ?1", [tech_name])
        .map_err(|e| e.to_string())?;
    {
        let mut stmt = tx
            .prepare("INSERT OR IGNORE INTO mod_tag(tech_name, tag) VALUES(?1, ?2)")
            .map_err(|e| e.to_string())?;
        for t in tags {
            let t = t.trim();
            if !t.is_empty() {
                stmt.execute(rusqlite::params![tech_name, t])
                    .map_err(|e| e.to_string())?;
            }
        }
    }
    tx.commit().map_err(|e| e.to_string())
}

/// Load all saved loadouts with their mod lists.
pub fn load_loadouts(conn: &Connection) -> Vec<Loadout> {
    let Ok(mut stmt) = conn
        .prepare("SELECT id, name, kind, created_at FROM loadout ORDER BY name COLLATE NOCASE, id")
    else {
        return Vec::new();
    };
    let heads: Vec<(i64, String, String, Option<i64>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .map(|r| r.flatten().collect())
        .unwrap_or_default();

    heads
        .into_iter()
        .map(|(id, name, kind, created_at)| {
            let mods = conn
                .prepare("SELECT tech_name FROM loadout_mod WHERE loadout_id = ?1")
                .and_then(|mut s| {
                    s.query_map([id], |r| r.get::<_, String>(0))
                        .map(|rows| rows.flatten().collect::<Vec<_>>())
                })
                .unwrap_or_default();
            Loadout {
                id,
                name,
                mods,
                kind,
                created_at,
            }
        })
        .collect()
}

/// Create (id None) or update (id Some) a loadout; returns its id. A new one is a user set; an
/// update keeps the set's kind.
pub fn save_loadout(
    conn: &mut Connection,
    id: Option<i64>,
    name: &str,
    mods: &[String],
) -> Result<i64, String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let id = write_loadout(&tx, id, name, mods, KIND_USER)?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(id)
}

/// Replace the mods of several saved sets in one transaction (the Library's "Mod sets…" dialog,
/// #323): every set is written or none is. Names and kinds stay as they are. Fails, writing
/// nothing, when one of the sets no longer exists.
pub fn replace_loadout_mods(
    conn: &mut Connection,
    edits: &[(i64, Vec<String>)],
) -> Result<(), String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    for (id, mods) in edits {
        let name: Option<String> = tx
            .query_row("SELECT name FROM loadout WHERE id = ?1", [id], |r| r.get(0))
            .optional()
            .map_err(|e| e.to_string())?;
        let Some(name) = name else {
            return Err(format!("mod set {id} no longer exists"));
        };
        write_loadout(&tx, Some(*id), &name, mods, KIND_USER)?;
    }
    tx.commit().map_err(|e| e.to_string())
}

/// Save an automatic "Before …" snapshot of the active mods, then drop all but the newest
/// `AUTO_SNAPSHOTS_KEPT` automatic ones, in one transaction. User sets are never touched.
pub fn save_auto_snapshot(
    conn: &mut Connection,
    name: &str,
    mods: &[String],
) -> Result<i64, String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let id = write_loadout(&tx, None, name, mods, KIND_AUTO)?;
    let stale: Vec<i64> = {
        let mut stmt = tx
            .prepare("SELECT id FROM loadout WHERE kind = ?1 ORDER BY id DESC LIMIT -1 OFFSET ?2")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(rusqlite::params![KIND_AUTO, AUTO_SNAPSHOTS_KEPT], |r| {
                r.get(0)
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<rusqlite::Result<_>>()
            .map_err(|e| e.to_string())?
    };
    for old in stale {
        remove_loadout(&tx, old)?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(id)
}

/// Refusal when saving to a mod set that no longer exists.
const LOADOUT_GONE: &str = "That mod set no longer exists, so nothing was saved to it.";

fn write_loadout(
    tx: &Connection,
    id: Option<i64>,
    name: &str,
    mods: &[String],
    kind: &str,
) -> Result<i64, String> {
    let id = match id {
        Some(id) => {
            let updated = tx
                .execute(
                    "UPDATE loadout SET name = ?2 WHERE id = ?1",
                    rusqlite::params![id, name],
                )
                .map_err(|e| e.to_string())?;
            // Deleted (or pruned as an old automatic set) since the UI read it: refuse, before
            // writing mod rows no set would own.
            if updated != 1 {
                return Err(LOADOUT_GONE.into());
            }
            tx.execute("DELETE FROM loadout_mod WHERE loadout_id = ?1", [id])
                .map_err(|e| e.to_string())?;
            id
        }
        None => {
            tx.execute(
                "INSERT INTO loadout(name, kind, created_at) VALUES(?1, ?2, ?3)",
                rusqlite::params![name, kind, now_ms() as i64],
            )
            .map_err(|e| e.to_string())?;
            tx.last_insert_rowid()
        }
    };
    let mut stmt = tx
        .prepare("INSERT OR IGNORE INTO loadout_mod(loadout_id, tech_name) VALUES(?1, ?2)")
        .map_err(|e| e.to_string())?;
    for m in mods {
        stmt.execute(rusqlite::params![id, m])
            .map_err(|e| e.to_string())?;
    }
    Ok(id)
}

fn remove_loadout(tx: &Connection, id: i64) -> Result<(), String> {
    tx.execute("DELETE FROM loadout_mod WHERE loadout_id = ?1", [id])
        .map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM loadout WHERE id = ?1", [id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn delete_loadout(conn: &mut Connection, id: i64) -> Result<(), String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    remove_loadout(&tx, id)?;
    tx.commit().map_err(|e| e.to_string())
}

/// Load the organize manifest for one mods folder (`root` = its `modsroot::canonical_key`).
pub fn load_organized(conn: &Connection, root: &str) -> Vec<OrganizedRow> {
    let Ok(mut stmt) = conn.prepare(
        "SELECT tech_name, file_name, kind, category, subcategory, active
         FROM organized WHERE root = ?1",
    ) else {
        return Vec::new();
    };
    let rows = stmt.query_map([root], |r| {
        Ok(OrganizedRow {
            tech_name: r.get(0)?,
            file_name: r.get(1)?,
            kind: r.get(2)?,
            category: r.get(3)?,
            subcategory: r.get(4)?,
            active: r.get::<_, i64>(5)? != 0,
        })
    });
    rows.map(|r| r.flatten().collect()).unwrap_or_default()
}

/// `load_organized`, but a failed read (of the table or of any row) is an error, never fewer
/// rows (#415).
pub fn try_load_organized(conn: &Connection, root: &str) -> Result<Vec<OrganizedRow>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT tech_name, file_name, kind, category, subcategory, active
             FROM organized WHERE root = ?1",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([root], |r| {
            Ok(OrganizedRow {
                tech_name: r.get(0)?,
                file_name: r.get(1)?,
                kind: r.get(2)?,
                category: r.get(3)?,
                subcategory: r.get(4)?,
                active: r.get::<_, i64>(5)? != 0,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

pub fn upsert_organized(conn: &Connection, root: &str, row: &OrganizedRow) -> Result<(), String> {
    conn.execute(
        "INSERT INTO organized(root, tech_name, file_name, kind, category, subcategory, active)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(root, tech_name) DO UPDATE SET
             file_name = excluded.file_name, kind = excluded.kind,
             category = excluded.category, subcategory = excluded.subcategory,
             active = excluded.active",
        rusqlite::params![
            root,
            row.tech_name,
            row.file_name,
            row.kind,
            row.category,
            row.subcategory,
            row.active as i64
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn set_organized_active(
    conn: &Connection,
    root: &str,
    tech_name: &str,
    active: bool,
) -> Result<(), String> {
    conn.execute(
        "UPDATE organized SET active = ?3 WHERE root = ?1 AND tech_name = ?2",
        rusqlite::params![root, tech_name, active as i64],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn delete_organized(conn: &Connection, root: &str, tech_name: &str) -> Result<(), String> {
    conn.execute(
        "DELETE FROM organized WHERE root = ?1 AND tech_name = ?2",
        rusqlite::params![root, tech_name],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Record a projected file's identity (size + mtime) so a later remove can confirm it's Silo's
/// own untouched copy without hashing it.
pub fn record_projection(conn: &Connection, root: &str, file_name: &str, size: u64, mtime_ms: i64) {
    let _ = conn.execute(
        "INSERT INTO projection(root, file_name, size, mtime_ms) VALUES(?1, ?2, ?3, ?4)
         ON CONFLICT(root, file_name) DO UPDATE SET size = excluded.size, mtime_ms = excluded.mtime_ms",
        rusqlite::params![root, file_name, size as i64, mtime_ms],
    );
}

/// The recorded (size, mtime_ms) for a projected file in this folder, if any.
pub fn projection_stats(conn: &Connection, root: &str, file_name: &str) -> Option<(u64, i64)> {
    conn.query_row(
        "SELECT size, mtime_ms FROM projection WHERE root = ?1 AND file_name = ?2",
        rusqlite::params![root, file_name],
        |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)?)),
    )
    .ok()
}

/// Every recorded projection in this folder: file name to (size, mtime_ms), as
/// [`projection_stats`] reads one. Empty when none (or unreadable).
pub fn load_projections(conn: &Connection, root: &str) -> HashMap<String, (u64, i64)> {
    let Ok(mut stmt) =
        conn.prepare("SELECT file_name, size, mtime_ms FROM projection WHERE root = ?1")
    else {
        return HashMap::new();
    };
    let rows = stmt.query_map([root], |r| {
        Ok((
            r.get::<_, String>(0)?,
            (r.get::<_, i64>(1)? as u64, r.get::<_, i64>(2)?),
        ))
    });
    rows.map(|r| r.flatten().collect()).unwrap_or_default()
}

/// Drop a projection's provenance (it's been removed from the flat root).
pub fn forget_projection(conn: &Connection, root: &str, file_name: &str) {
    let _ = conn.execute(
        "DELETE FROM projection WHERE root = ?1 AND file_name = ?2",
        rusqlite::params![root, file_name],
    );
}

/// Make one folder's `organized` manifest match what's physically in its `archive/`. Upserts a
/// row for every archived mod (so `set_active` can project it) and drops rows for mods that are
/// no longer archived. This heals a manifest that has drifted from disk — e.g. a fresh DB with
/// a populated `archive/` — which otherwise leaves archived mods visible but un-activatable.
/// Other folders' rows are never touched.
pub fn reconcile_organized(
    conn: &mut Connection,
    root: &str,
    desired: &[OrganizedRow],
) -> Result<(), String> {
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let keep: std::collections::HashSet<&str> =
        desired.iter().map(|r| r.tech_name.as_str()).collect();
    for existing in load_organized(&tx, root) {
        if !keep.contains(existing.tech_name.as_str()) {
            delete_organized(&tx, root, &existing.tech_name)?;
        }
    }
    for row in desired {
        upsert_organized(&tx, root, row)?;
    }
    tx.commit().map_err(|e| e.to_string())
}

/// Load all curation rows.
pub fn load_curation(conn: &Connection) -> Vec<CurationRow> {
    let Ok(mut stmt) = conn.prepare(
        "SELECT tech_name, favorite, hidden, broken, note, rating, skip_updates FROM curation",
    ) else {
        return Vec::new();
    };
    let rows = stmt.query_map([], |r| {
        Ok(CurationRow {
            tech_name: r.get(0)?,
            favorite: r.get::<_, i64>(1)? != 0,
            hidden: r.get::<_, i64>(2)? != 0,
            broken: r.get::<_, i64>(3)? != 0,
            note: r.get(4)?,
            rating: r.get::<_, i64>(5).unwrap_or(0),
            skip_updates: r.get::<_, i64>(6).unwrap_or(0) != 0,
        })
    });
    rows.map(|r| r.flatten().collect()).unwrap_or_default()
}

/// Upsert one curation row. If all flags are false and note is empty, delete it.
pub fn set_curation(conn: &Connection, row: &CurationRow) -> Result<(), String> {
    let empty = !row.favorite
        && !row.hidden
        && !row.broken
        && row.rating == 0
        && !row.skip_updates
        && row.note.as_deref().unwrap_or("").is_empty();
    if empty {
        conn.execute(
            "DELETE FROM curation WHERE tech_name = ?1",
            [&row.tech_name],
        )
        .map_err(|e| e.to_string())?;
        return Ok(());
    }
    conn.execute(
        "INSERT INTO curation(tech_name, favorite, hidden, broken, note, rating, skip_updates)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(tech_name) DO UPDATE SET
             favorite     = excluded.favorite,
             hidden       = excluded.hidden,
             broken       = excluded.broken,
             note         = excluded.note,
             rating       = excluded.rating,
             skip_updates = excluded.skip_updates",
        rusqlite::params![
            row.tech_name,
            row.favorite as i64,
            row.hidden as i64,
            row.broken as i64,
            row.note,
            row.rating,
            row.skip_updates as i64,
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Load all category overrides.
pub fn load_overrides(conn: &Connection) -> Vec<CategoryOverride> {
    let Ok(mut stmt) =
        conn.prepare("SELECT tech_name, category, subcategory FROM category_override")
    else {
        return Vec::new();
    };
    let rows = stmt.query_map([], |r| {
        Ok(CategoryOverride {
            tech_name: r.get(0)?,
            category: r.get(1)?,
            subcategory: r.get(2)?,
        })
    });
    rows.map(|r| r.flatten().collect()).unwrap_or_default()
}

/// Upsert (or clear) a category override.
pub fn set_override(conn: &Connection, row: &CategoryOverride) -> Result<(), String> {
    if row.category.is_empty() {
        conn.execute(
            "DELETE FROM category_override WHERE tech_name = ?1",
            [&row.tech_name],
        )
        .map_err(|e| e.to_string())?;
        return Ok(());
    }
    conn.execute(
        "INSERT INTO category_override(tech_name, category, subcategory)
         VALUES(?1, ?2, ?3)
         ON CONFLICT(tech_name) DO UPDATE SET
             category = excluded.category,
             subcategory = excluded.subcategory",
        rusqlite::params![row.tech_name, row.category, row.subcategory],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Load the entire scan cache into memory (one query) for lock-free lookups
/// during the parallel scan.
/// Drop every cached scan row, forcing the next scan to re-parse (and re-categorize)
/// every mod from scratch. Used when categorization logic improves and the user wants
/// it applied to their existing library.
pub fn clear_cache(conn: &Connection) -> usize {
    conn.execute("DELETE FROM mod_cache", []).unwrap_or(0)
}

pub fn load_cache(conn: &Connection) -> HashMap<String, CacheEntry> {
    let mut map = HashMap::new();
    let Ok(mut stmt) = conn.prepare("SELECT path, mtime_ms, size, json FROM mod_cache") else {
        return map;
    };
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)? as u64,
            r.get::<_, i64>(2)? as u64,
            r.get::<_, String>(3)?,
        ))
    });
    if let Ok(rows) = rows {
        for (path, mtime_ms, size, json) in rows.flatten() {
            map.insert(
                path,
                CacheEntry {
                    mtime_ms,
                    size,
                    json,
                },
            );
        }
    }
    map
}

/// Upsert freshly-parsed rows in a single transaction.
pub fn upsert_many(
    conn: &mut Connection,
    rows: &[(String, u64, u64, String)],
) -> Result<(), String> {
    if rows.is_empty() {
        return Ok(());
    }
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    {
        let mut stmt = tx
            .prepare(
                "INSERT INTO mod_cache(path, mtime_ms, size, json) VALUES(?1, ?2, ?3, ?4)
                 ON CONFLICT(path) DO UPDATE SET
                     mtime_ms = excluded.mtime_ms,
                     size     = excluded.size,
                     json     = excluded.json",
            )
            .map_err(|e| e.to_string())?;
        for (path, mtime, size, json) in rows {
            stmt.execute(rusqlite::params![path, *mtime as i64, *size as i64, json])
                .map_err(|e| e.to_string())?;
        }
    }
    tx.commit().map_err(|e| e.to_string())
}

/// Carry a mod's scan-cache row from `from` to `to` when Silo moves the file (filing it into the
/// library). A rename keeps mtime and size, so the next scan reuses the row instead of parsing
/// the mod again; without this, filing a whole library made the scan after it re-read every
/// mod (#322). The row's own `path` field is rewritten too. Best effort: no row, or a row that
/// can't be read, is left alone (the scan parses that mod fresh).
/// @returns whether a row was carried over
pub fn move_cache_row(conn: &Connection, from: &str, to: &str) -> Result<bool, String> {
    let row = conn
        .query_row(
            "SELECT mtime_ms, size, json FROM mod_cache WHERE path = ?1",
            [from],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some((mtime, size, json)) = row else {
        return Ok(false);
    };
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&json) else {
        return Ok(false);
    };
    let Some(obj) = value.as_object_mut() else {
        return Ok(false);
    };
    obj.insert("path".into(), serde_json::Value::String(to.to_string()));
    let json = value.to_string();
    conn.execute(
        "INSERT INTO mod_cache(path, mtime_ms, size, json) VALUES(?1, ?2, ?3, ?4)
         ON CONFLICT(path) DO UPDATE SET
             mtime_ms = excluded.mtime_ms,
             size     = excluded.size,
             json     = excluded.json",
        rusqlite::params![to, mtime, size, json],
    )
    .map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM mod_cache WHERE path = ?1", [from])
        .map_err(|e| e.to_string())?;
    Ok(true)
}

/// Delete cache rows for mods that are no longer present on disk.
pub fn prune_missing(conn: &mut Connection, present: &HashSet<String>) -> Result<usize, String> {
    let existing: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT path FROM mod_cache")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.flatten().collect()
    };
    let stale: Vec<&String> = existing.iter().filter(|p| !present.contains(*p)).collect();
    if stale.is_empty() {
        return Ok(0);
    }
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    {
        let mut stmt = tx
            .prepare("DELETE FROM mod_cache WHERE path = ?1")
            .map_err(|e| e.to_string())?;
        for p in &stale {
            stmt.execute([p]).map_err(|e| e.to_string())?;
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(stale.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Migration steps started, by database file (tests run in parallel, so per file).
    static MIGRATION_STEPS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    /// Files whose migration step lingers, so a concurrent open gets every chance to start a
    /// second, overlapping migration if nothing stops it.
    static SLOW_MIGRATIONS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    pub(super) fn count_migration_step(conn: &Connection) {
        let path = conn.path().unwrap_or_default().to_string();
        let slow = SLOW_MIGRATIONS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&path);
        MIGRATION_STEPS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(path);
        if slow {
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
    }

    /// Renames that fail on purpose, by source path.
    static FAILING_RENAMES: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

    pub(super) fn rename_fails(from: &Path) -> bool {
        FAILING_RENAMES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|p| p == from)
    }

    fn fail_renames_from(from: &Path) {
        FAILING_RENAMES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(from.to_path_buf());
    }

    fn sqlite_path(path: &Path) -> String {
        let opened = Connection::open(path).unwrap();
        opened.path().unwrap_or_default().to_string()
    }

    fn migration_steps(path: &Path) -> usize {
        let path = sqlite_path(path);
        MIGRATION_STEPS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|p| **p == path)
            .count()
    }

    fn temp_db(tag: &str) -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!("silo_db_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base.join("silo.db")
    }

    /// Every folder with organized mods, once each, never the legacy '' root; a failed read is
    /// an error, never "none".
    #[test]
    fn organized_roots_lists_each_folder_once() {
        let path = temp_db("roots");
        let conn = open(&path).unwrap();
        let row = |tech: &str| OrganizedRow {
            tech_name: tech.into(),
            file_name: format!("{tech}.zip"),
            kind: "zip".into(),
            category: "Tools".into(),
            subcategory: None,
            active: false,
        };
        upsert_organized(&conn, "/b", &row("FS25_A")).unwrap();
        upsert_organized(&conn, "/b", &row("FS25_B")).unwrap();
        upsert_organized(&conn, "/a", &row("FS25_A")).unwrap();
        upsert_organized(&conn, "", &row("FS25_C")).unwrap();
        assert_eq!(organized_roots(&conn).unwrap(), vec!["/a", "/b"]);
        conn.execute_batch("DROP TABLE organized").unwrap();
        assert!(organized_roots(&conn).is_err());
        drop(conn);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn settings_by_prefix_match_the_prefix_exactly() {
        let path = temp_db("prefix");
        let conn = open(&path).unwrap();
        set_app_setting(&conn, "archive_root:b", Some("2")).unwrap();
        set_app_setting(&conn, "archive_root:a", Some("1")).unwrap();
        // `_` and `%` would be LIKE wildcards: neither may widen the match.
        set_app_setting(&conn, "archiveXroot:c", Some("3")).unwrap();
        set_app_setting(&conn, "archive_root", Some("4")).unwrap();
        set_app_setting(&conn, "a%:d", Some("5")).unwrap();
        assert_eq!(
            app_settings_with_prefix(&conn, "archive_root:").unwrap(),
            vec![
                ("archive_root:a".to_string(), "1".to_string()),
                ("archive_root:b".to_string(), "2".to_string()),
            ]
        );
        assert!(app_settings_with_prefix(&conn, "a%").unwrap().len() == 1);
        // A failed query is an error, never "no settings".
        conn.execute_batch("DROP TABLE app_setting").unwrap();
        assert!(app_settings_with_prefix(&conn, "archive_root:").is_err());
        drop(conn);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn migrates_a_pre_root_database_and_is_idempotent() {
        let path = temp_db("migrate");
        {
            // The v0.7 schema, with one row in each table.
            let c = rusqlite::Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE organized (tech_name TEXT PRIMARY KEY, file_name TEXT NOT NULL,
                     kind TEXT NOT NULL, category TEXT NOT NULL, subcategory TEXT,
                     active INTEGER NOT NULL DEFAULT 0);
                 CREATE TABLE projection (file_name TEXT PRIMARY KEY, size INTEGER NOT NULL,
                     mtime_ms INTEGER NOT NULL);
                 INSERT INTO organized VALUES ('FS25_A','FS25_A.zip','zip','Vehicles',NULL,1);
                 INSERT INTO projection VALUES ('FS25_A.zip', 10, 20);",
            )
            .unwrap();
        }
        let conn = open(&path).unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        assert!(has_column(&conn, "curation", "rating"));
        // Old rows arrive unscoped (root = '') until claimed.
        assert_eq!(load_organized(&conn, "").len(), 1);
        claim_unscoped(&conn, "c:\\mods").unwrap();
        assert_eq!(load_organized(&conn, "c:\\mods")[0].tech_name, "FS25_A");
        assert_eq!(
            projection_stats(&conn, "c:\\mods", "FS25_A.zip"),
            Some((10, 20))
        );
        assert!(load_organized(&conn, "").is_empty());
        drop(conn);
        // Re-opening must not rebuild again or lose rows.
        let conn = open(&path).unwrap();
        assert_eq!(load_organized(&conn, "c:\\mods").len(), 1);
    }

    #[test]
    fn a_new_database_is_stamped_with_the_current_schema() {
        let path = temp_db("fresh_version");
        let conn = open(&path).unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
    }

    #[test]
    fn a_database_from_a_newer_silo_is_refused_untouched() {
        let path = temp_db("newer");
        {
            let c = rusqlite::Connection::open(&path).unwrap();
            c.execute_batch(&format!(
                "CREATE TABLE future (x); PRAGMA user_version = {};",
                SCHEMA_VERSION + 1
            ))
            .unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        let err = open(&path).unwrap_err();
        assert!(err.contains("newer version of Silo"), "{err}");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "the file is not modified"
        );
    }

    #[test]
    fn a_copy_is_kept_before_a_database_is_migrated() {
        let path = temp_db("premigrate");
        {
            let c = rusqlite::Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE organized (tech_name TEXT PRIMARY KEY, file_name TEXT NOT NULL,
                     kind TEXT NOT NULL, category TEXT NOT NULL, subcategory TEXT,
                     active INTEGER NOT NULL DEFAULT 0);
                 CREATE TABLE projection (file_name TEXT PRIMARY KEY, size INTEGER NOT NULL,
                     mtime_ms INTEGER NOT NULL);
                 INSERT INTO organized VALUES ('FS25_A','FS25_A.zip','zip','Vehicles',NULL,1);",
            )
            .unwrap();
        }
        drop(open(&path).unwrap());
        let copy = snapshot_dir(&path).join("silo-before-v0.db");
        {
            let c = Connection::open(&copy).unwrap();
            assert_eq!(schema_version(&c).unwrap(), 0);
            assert!(
                !has_column(&c, "organized", "root"),
                "taken before the migration"
            );
            let n: i64 = c
                .query_row("SELECT COUNT(*) FROM organized", [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 1);
        }
        // Kept for the user: never restored automatically, never rotated out.
        assert!(!is_usable_snapshot(&copy));
        assert!(snapshot_ms("silo-before-v0.db").is_none());
    }

    /// A schema version 1 database (Silo 0.11) holding one loadout with two mods.
    fn v1_database(path: &Path) {
        let c = rusqlite::Connection::open(path).unwrap();
        c.execute_batch(BASE_SCHEMA).unwrap();
        c.execute_batch(
            "ALTER TABLE curation ADD COLUMN rating INTEGER NOT NULL DEFAULT 0;
             INSERT INTO loadout(id, name) VALUES (7, 'Harvest 2026');
             INSERT INTO loadout_mod VALUES (7, 'FS25_A'), (7, 'FS25_B');
             PRAGMA user_version = 1;",
        )
        .unwrap();
    }

    #[test]
    fn migrating_to_v3_keeps_curation_and_checks_every_mod() {
        // #333: a v2 library's flags survive, and nothing starts out skipped.
        let path = temp_db("v2_to_v3");
        v1_database(&path);
        {
            let conn = open(&path).unwrap(); // brings it to the current version
            set_curation(
                &conn,
                &CurationRow {
                    tech_name: "FS25_A".into(),
                    favorite: true,
                    hidden: false,
                    broken: false,
                    rating: 4,
                    note: Some("mine".into()),
                    skip_updates: false,
                },
            )
            .unwrap();
            // Wind it back to a v2 file: drop the column the way a 1.0.2 library lacks it.
            conn.execute_batch(
                "ALTER TABLE curation DROP COLUMN skip_updates; PRAGMA user_version = 2;",
            )
            .unwrap();
        }
        let conn = open(&path).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), SCHEMA_VERSION);
        let rows = load_curation(&conn);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].favorite && rows[0].rating == 4 && !rows[0].skip_updates);
        assert_eq!(rows[0].note.as_deref(), Some("mine"));
        // The flag saves, and a row with only it set is kept (not treated as empty).
        let mut r = rows[0].clone();
        r.favorite = false;
        r.rating = 0;
        r.note = None;
        r.skip_updates = true;
        set_curation(&conn, &r).unwrap();
        let rows = load_curation(&conn);
        assert!(rows.len() == 1 && rows[0].skip_updates);
        r.skip_updates = false;
        set_curation(&conn, &r).unwrap();
        assert!(
            load_curation(&conn).is_empty(),
            "back to defaults: the row goes"
        );
    }

    #[test]
    fn migrating_to_v4_adds_catalog_aliases_and_keeps_the_library() {
        let path = temp_db("v3_to_v4");
        {
            let conn = open(&path).unwrap();
            set_source(&conn, "FS25_A", Some("https://example.com/a")).unwrap();
            // Wind it back to a v3 file, as Silo 1.0.x left it.
            conn.execute_batch(
                "DROP TABLE alias_check; DROP TABLE catalog_alias; PRAGMA user_version = 3;",
            )
            .unwrap();
        }
        let conn = open(&path).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), SCHEMA_VERSION);
        assert_eq!(load_sources(&conn).len(), 1, "the library is kept");
        assert!(catalog_aliases(&conn, "FS25_FieldToDoList")
            .unwrap()
            .is_empty());

        let row = CatalogAliasRow {
            tech_name: "FS25_FieldToDoList".into(),
            archive_sha256: "ABCD".into(),
            size: 10,
            catalog_id: "cat-todo".into(),
            catalog_tech_name: Some("FS25_ToDo".into()),
            download_url: Some("https://github.com/k/r/releases/download/v1/x.zip".into()),
        };
        record_catalog_alias(&conn, &row).unwrap();
        // The same file again replaces its row; another build gets its own.
        record_catalog_alias(&conn, &row).unwrap();
        let other = CatalogAliasRow {
            archive_sha256: "ef01".into(),
            catalog_tech_name: None,
            ..row.clone()
        };
        record_catalog_alias(&conn, &other).unwrap();
        let rows = catalog_aliases(&conn, "FS25_FieldToDoList").unwrap();
        assert_eq!(rows.len(), 2);
        let first = rows.iter().find(|r| r.archive_sha256 == "abcd").unwrap();
        assert_eq!(first.catalog_id, "cat-todo", "the hash is kept lowercase");
        assert_eq!(first.catalog_tech_name.as_deref(), Some("FS25_ToDo"));
        assert_eq!(first.size, 10);
        assert!(first.download_url.is_some());
        assert!(catalog_aliases(&conn, "FS25_fieldtodolist")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn migrating_to_v5_adds_alias_checks_and_keeps_the_library() {
        let path = temp_db("v4_to_v5");
        {
            let conn = open(&path).unwrap();
            set_source(&conn, "FS25_A", Some("https://example.com/a")).unwrap();
            record_catalog_alias(
                &conn,
                &CatalogAliasRow {
                    tech_name: "FS25_FieldToDoList".into(),
                    archive_sha256: "abcd".into(),
                    size: 10,
                    catalog_id: "cat-todo".into(),
                    catalog_tech_name: Some("FS25_ToDo".into()),
                    download_url: None,
                },
            )
            .unwrap();
            // Wind it back to a v4 file, as Silo with #388 left it.
            conn.execute_batch("DROP TABLE alias_check; PRAGMA user_version = 4;")
                .unwrap();
        }
        let conn = open(&path).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), 5);
        assert_eq!(load_sources(&conn).len(), 1, "the library is kept");
        assert_eq!(
            catalog_aliases(&conn, "FS25_FieldToDoList").unwrap().len(),
            1,
            "the links are kept"
        );
        assert_eq!(alias_check(&conn, "m/FS25_X.zip").unwrap(), None);
    }

    #[test]
    fn the_mods_left_out_of_update_checks_are_listed() {
        let path = temp_db("skip_names");
        let conn = open(&path).unwrap();
        let row = |name: &str, skip: bool| CurationRow {
            tech_name: name.into(),
            favorite: !skip,
            hidden: false,
            broken: false,
            rating: 0,
            note: None,
            skip_updates: skip,
        };
        set_curation(&conn, &row("FS25_A", true)).unwrap();
        set_curation(&conn, &row("FS25_B", false)).unwrap();
        let names = skip_update_names(&conn).unwrap();
        assert_eq!(names, HashSet::from(["FS25_A".to_string()]));
        conn.execute_batch("DROP TABLE curation").unwrap();
        assert!(
            skip_update_names(&conn).is_err(),
            "unreadable is an error, not none"
        );
    }

    #[test]
    fn an_alias_check_round_trips_and_is_pruned_with_the_scan_cache() {
        let path = temp_db("alias_check");
        let mut conn = open(&path).unwrap();
        let first = AliasCheck {
            size: 10,
            mtime_ms: 5,
            sha256: Some("ABCD".into()),
            unnamed_ms: Some(50),
            decided: false,
            attempted_ms: 100,
        };
        note_alias_check(&conn, "m/FS25_X.zip", &first).unwrap();
        let got = alias_check(&conn, "m/FS25_X.zip").unwrap().unwrap();
        assert_eq!(
            got.sha256.as_deref(),
            Some("abcd"),
            "the hash is kept lowercase"
        );
        assert_eq!(
            (got.size, got.mtime_ms, got.decided, got.attempted_ms),
            (10, 5, false, 100)
        );
        assert_eq!(alias_check(&conn, "m/FS25_Y.zip").unwrap(), None);
        // A newer version of the file replaces the note for the older one.
        let next = AliasCheck {
            size: 11,
            mtime_ms: 6,
            sha256: None,
            unnamed_ms: None,
            decided: true,
            attempted_ms: 200,
        };
        note_alias_check(&conn, "m/FS25_X.zip", &next).unwrap();
        assert_eq!(
            alias_check(&conn, "m/FS25_X.zip").unwrap(),
            Some(next.clone())
        );
        // Several at once, in one transaction.
        let many: Vec<(String, AliasCheck)> = ["m/A.zip", "m/B.zip"]
            .iter()
            .map(|p| (p.to_string(), first.clone()))
            .collect();
        note_alias_checks(&conn, &many).unwrap();
        assert!(alias_check(&conn, "m/B.zip").unwrap().is_some());
        note_alias_checks(&conn, &[]).unwrap();
        assert_eq!(last_alias_attempt(&conn).unwrap(), Some(200));
        conn.execute(
            "DELETE FROM alias_check WHERE path IN ('m/A.zip', 'm/B.zip')",
            [],
        )
        .unwrap();
        // Notes for files the scan cache no longer has are dropped; the rest stay.
        note_alias_check(&conn, "m/FS25_Gone.zip", &first).unwrap();
        upsert_many(&mut conn, &[("m/FS25_X.zip".into(), 6, 11, "{}".into())]).unwrap();
        assert_eq!(prune_alias_checks(&conn).unwrap(), 1);
        assert_eq!(alias_check(&conn, "m/FS25_X.zip").unwrap(), Some(next));
        assert_eq!(alias_check(&conn, "m/FS25_Gone.zip").unwrap(), None);
    }

    #[test]
    fn migrating_to_v2_keeps_every_set_as_a_user_set() {
        let path = temp_db("v1_to_v2");
        v1_database(&path);
        let conn = open(&path).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), SCHEMA_VERSION);
        let sets = load_loadouts(&conn);
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].id, 7);
        assert_eq!(sets[0].name, "Harvest 2026");
        assert_eq!(sets[0].kind, KIND_USER);
        assert_eq!(sets[0].created_at, None, "no date was recorded before v2");
        let mut mods = sets[0].mods.clone();
        mods.sort();
        assert_eq!(mods, ["FS25_A", "FS25_B"]);
        drop(conn);
        // The pre-migration copy is the v1 file as it was.
        let copy = snapshot_dir(&path).join("silo-before-v1.db");
        let c = Connection::open(&copy).unwrap();
        assert_eq!(schema_version(&c).unwrap(), 1);
        assert!(
            !has_column(&c, "loadout", "kind"),
            "taken before the migration"
        );
        drop(c);
        // Opening again changes nothing.
        let conn = open(&path).unwrap();
        assert_eq!(load_loadouts(&conn).len(), 1);
    }

    #[test]
    fn only_the_newest_five_automatic_snapshots_are_kept() {
        let path = temp_db("auto_snapshots");
        let mut c = open(&path).unwrap();
        let user = save_loadout(&mut c, None, "Harvest 2026", &["FS25_A".to_string()]).unwrap();
        let ids: Vec<i64> = (0..8)
            .map(|i| {
                save_auto_snapshot(&mut c, &format!("Before {i}"), &[format!("FS25_{i}")]).unwrap()
            })
            .collect();
        let sets = load_loadouts(&c);
        let autos: Vec<&Loadout> = sets.iter().filter(|l| l.kind == KIND_AUTO).collect();
        assert_eq!(autos.len(), AUTO_SNAPSHOTS_KEPT as usize);
        let mut kept: Vec<i64> = autos.iter().map(|l| l.id).collect();
        kept.sort();
        assert_eq!(kept, ids[3..], "the oldest three were dropped");
        assert!(autos.iter().all(|l| l.created_at.is_some()));
        let u = sets
            .iter()
            .find(|l| l.id == user)
            .expect("user sets are never pruned");
        assert_eq!(u.kind, KIND_USER);
        // A dropped snapshot's mod rows go with it.
        let orphans: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM loadout_mod WHERE loadout_id NOT IN (SELECT id FROM loadout)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(orphans, 0);
    }

    #[test]
    fn updating_a_set_keeps_its_kind() {
        let path = temp_db("update_kind");
        let mut c = open(&path).unwrap();
        let auto = save_auto_snapshot(&mut c, "Before X", &["FS25_A".to_string()]).unwrap();
        save_loadout(&mut c, Some(auto), "Before X", &["FS25_B".to_string()]).unwrap();
        let l = load_loadouts(&c)
            .into_iter()
            .find(|l| l.id == auto)
            .unwrap();
        assert_eq!(l.kind, KIND_AUTO);
        assert_eq!(l.mods, ["FS25_B"]);
    }

    #[test]
    fn saving_a_set_that_was_pruned_fails_and_writes_nothing() {
        let path = temp_db("save_gone");
        let mut c = open(&path).unwrap();
        // The UI still holds the oldest automatic set; five more switches prune it.
        let stale = save_auto_snapshot(&mut c, "Before 0", &["FS25_A".to_string()]).unwrap();
        for i in 1..=AUTO_SNAPSHOTS_KEPT {
            save_auto_snapshot(&mut c, &format!("Before {i}"), &[]).unwrap();
        }
        assert!(load_loadouts(&c).iter().all(|l| l.id != stale));
        let err =
            save_loadout(&mut c, Some(stale), "Before 0", &["FS25_B".to_string()]).unwrap_err();
        assert_eq!(err, LOADOUT_GONE);
        assert!(
            load_loadouts(&c).iter().all(|l| l.id != stale),
            "not recreated"
        );
        let orphans: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM loadout_mod WHERE loadout_id NOT IN (SELECT id FROM loadout)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(orphans, 0, "no mod rows without a set");
        // An existing set still saves.
        let ok = save_loadout(&mut c, None, "Harvest", &[]).unwrap();
        assert_eq!(save_loadout(&mut c, Some(ok), "Harvest", &[]), Ok(ok));
    }

    #[test]
    fn replacing_several_sets_mods_writes_them_all() {
        let path = temp_db("replace_mods");
        let mut c = open(&path).unwrap();
        let a = save_loadout(&mut c, None, "Riverbend", &["FS25_A".to_string()]).unwrap();
        let b = save_loadout(&mut c, None, "Hutan", &["FS25_B".to_string()]).unwrap();
        replace_loadout_mods(
            &mut c,
            &[
                (a, vec!["FS25_A".to_string(), "FS25_C".to_string()]),
                (b, vec![]),
            ],
        )
        .unwrap();
        let sets = load_loadouts(&c);
        let get = |id: i64| sets.iter().find(|l| l.id == id).unwrap();
        let mut am = get(a).mods.clone();
        am.sort();
        assert_eq!(am, ["FS25_A", "FS25_C"]);
        assert_eq!(get(a).name, "Riverbend", "names are kept");
        assert!(get(b).mods.is_empty());
        assert_eq!(get(b).kind, KIND_USER);
    }

    #[test]
    fn replacing_sets_mods_writes_nothing_when_one_set_is_gone() {
        let path = temp_db("replace_mods_gone");
        let mut c = open(&path).unwrap();
        let a = save_loadout(&mut c, None, "Riverbend", &["FS25_A".to_string()]).unwrap();
        let err = replace_loadout_mods(
            &mut c,
            &[(a, vec!["FS25_Z".to_string()]), (a + 999, vec![])],
        )
        .unwrap_err();
        assert!(err.contains("no longer exists"), "{err}");
        let sets = load_loadouts(&c);
        assert_eq!(sets[0].mods, ["FS25_A"], "the first edit was rolled back");
        let orphans: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM loadout_mod WHERE loadout_id NOT IN (SELECT id FROM loadout)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(orphans, 0);
    }

    #[test]
    fn a_new_database_needs_no_pre_migration_copy() {
        let path = temp_db("premigrate_fresh");
        drop(open(&path).unwrap());
        assert!(!snapshot_dir(&path).exists());
    }

    #[test]
    fn concurrent_first_opens_migrate_exactly_once() {
        // A pre-versioning (v0.7-shaped) database, opened by several connections at once.
        // Without a write lock around "read version, migrate, stamp", two of them would both
        // rebuild `organized` and one would fail or lose rows.
        for round in 0..5 {
            let path = temp_db(&format!("concurrent_{round}"));
            {
                let c = rusqlite::Connection::open(&path).unwrap();
                c.execute_batch(
                    "CREATE TABLE organized (tech_name TEXT PRIMARY KEY, file_name TEXT NOT NULL,
                         kind TEXT NOT NULL, category TEXT NOT NULL, subcategory TEXT,
                         active INTEGER NOT NULL DEFAULT 0);
                     CREATE TABLE projection (file_name TEXT PRIMARY KEY, size INTEGER NOT NULL,
                         mtime_ms INTEGER NOT NULL);
                     INSERT INTO organized VALUES ('FS25_A','FS25_A.zip','zip','Vehicles',NULL,1);
                     INSERT INTO projection VALUES ('FS25_A.zip', 10, 20);",
                )
                .unwrap();
            }
            SLOW_MIGRATIONS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(sqlite_path(&path));
            let start = std::sync::Arc::new(std::sync::Barrier::new(6));
            let handles: Vec<_> = (0..6)
                .map(|_| {
                    let (path, start) = (path.clone(), start.clone());
                    std::thread::spawn(move || {
                        start.wait();
                        open(&path).map(drop)
                    })
                })
                .collect();
            for h in handles {
                h.join().unwrap().expect("every concurrent open succeeds");
            }
            assert_eq!(
                migration_steps(&path),
                SCHEMA_VERSION as usize,
                "each step ran exactly once"
            );
            let conn = open(&path).unwrap();
            let v: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(v, SCHEMA_VERSION);
            assert_eq!(load_organized(&conn, "").len(), 1, "no row lost or doubled");
            assert_eq!(projection_stats(&conn, "", "FS25_A.zip"), Some((10, 20)));
        }
    }

    fn wal_shm(path: &Path) -> [std::path::PathBuf; 2] {
        let s = path.to_string_lossy();
        [format!("{s}-wal").into(), format!("{s}-shm").into()]
    }

    fn damage(path: &Path) {
        forget_checked(path);
        for p in wal_shm(path) {
            let _ = std::fs::remove_file(p);
        }
        std::fs::write(
            path,
            b"this is not a database, just bytes that used to be one",
        )
        .unwrap();
    }

    fn set_aside(path: &Path) -> Vec<std::path::PathBuf> {
        std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("silo.db.damaged-")
            })
            .collect()
    }

    /// This test's own notices (the shared queue also holds other tests').
    fn notices_for(path: &Path) -> Vec<String> {
        let dir = path.parent().unwrap().to_string_lossy().into_owned();
        crate::notices::take_mentioning(&dir)
    }

    #[test]
    fn a_damaged_database_is_set_aside_and_the_newest_snapshot_restored() {
        let path = temp_db("damaged_restore");
        let dir = snapshot_dir(&path);
        {
            let mut c = open(&path).unwrap();
            save_loadout(&mut c, None, "Before", &["FS25_A".to_string()]).unwrap();
            assert!(snapshot(&c, &dir, 5, std::time::Duration::ZERO)
                .unwrap()
                .is_some());
        }
        damage(&path);
        let conn = open(&path).unwrap();
        assert_eq!(load_loadouts(&conn)[0].name, "Before");
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal", "the restored copy is back in WAL mode");
        let notices = notices_for(&path);
        assert!(
            notices.iter().any(|n| n.contains("restored")),
            "{notices:?}"
        );
        let kept = set_aside(&path);
        assert_eq!(kept.len(), 1, "the damaged file is kept, never deleted");
        assert!(std::fs::read(&kept[0])
            .unwrap()
            .starts_with(b"this is not a database"));
    }

    #[test]
    fn with_no_snapshot_a_damaged_database_starts_fresh() {
        let path = temp_db("damaged_fresh");
        drop(open(&path).unwrap());
        damage(&path);
        let conn = open(&path).unwrap();
        assert!(load_loadouts(&conn).is_empty());
        let notices = notices_for(&path);
        assert!(
            notices.iter().any(|n| n.contains("started a new one")),
            "{notices:?}"
        );
        assert_eq!(set_aside(&path).len(), 1);
    }

    #[test]
    fn a_second_recovery_never_replaces_the_first_damaged_file() {
        let path = temp_db("damaged_twice");
        drop(open(&path).unwrap());
        damage(&path);
        drop(open(&path).unwrap());
        damage(&path);
        drop(open(&path).unwrap());
        assert_eq!(set_aside(&path).len(), 2, "both damaged files are kept");
    }

    #[test]
    fn a_damaged_database_that_cant_be_set_aside_is_left_whole_and_not_opened() {
        let path = temp_db("aside_fails");
        drop(open(&path).unwrap());
        damage(&path);
        let wal = wal_shm(&path)[0].clone();
        std::fs::write(&wal, b"the damaged file's journal").unwrap();
        fail_renames_from(&path); // the main file can't be moved (another program has it open)
        for _ in 0..2 {
            let err = open(&path).unwrap_err();
            assert!(err.contains("couldn't be set aside"), "{err}");
        }
        assert!(
            set_aside(&path).is_empty(),
            "the -wal was put back beside its file"
        );
        assert_eq!(std::fs::read(&wal).unwrap(), b"the damaged file's journal");
        assert!(std::fs::read(&path)
            .unwrap()
            .starts_with(b"this is not a database"));
        let notices = notices_for(&path);
        assert!(notices.iter().any(|n| n.contains("couldn't be set aside")));
    }

    #[test]
    fn a_backup_that_cant_be_put_back_says_so_and_starts_fresh() {
        let path = temp_db("restore_fails");
        let dir = snapshot_dir(&path);
        {
            let mut c = open(&path).unwrap();
            save_loadout(&mut c, None, "Backed up", &[]).unwrap();
            snapshot(&c, &dir, 5, std::time::Duration::ZERO).unwrap();
        }
        damage(&path);
        let tmp = path.with_file_name("silo.db.restore-tmp");
        fail_renames_from(&tmp);
        let conn = open(&path).unwrap();
        assert!(load_loadouts(&conn).is_empty(), "a new database");
        assert!(!tmp.exists(), "the half-restored copy is cleaned up");
        assert_eq!(set_aside(&path).len(), 1);
        let notices = notices_for(&path);
        assert!(
            notices.iter().any(|n| n.contains("couldn't put back")),
            "{notices:?}"
        );
    }

    /// Replace the database with an empty one: 0 bytes, or a valid file with no tables.
    fn empty_out(path: &Path, zero_bytes: bool) {
        forget_checked(path);
        for p in wal_shm(path) {
            let _ = std::fs::remove_file(p);
        }
        std::fs::remove_file(path).unwrap();
        if zero_bytes {
            std::fs::write(path, b"").unwrap();
        } else {
            let c = Connection::open(path).unwrap();
            c.execute_batch("CREATE TABLE x (a); DROP TABLE x;")
                .unwrap();
        }
    }

    #[test]
    fn an_emptied_database_is_restored_from_a_snapshot() {
        for zero_bytes in [true, false] {
            let path = temp_db(&format!("emptied_{zero_bytes}"));
            {
                let mut c = open(&path).unwrap();
                save_loadout(&mut c, None, "Kept", &[]).unwrap();
                snapshot(&c, &snapshot_dir(&path), 5, std::time::Duration::ZERO).unwrap();
            }
            empty_out(&path, zero_bytes);
            let conn = open(&path).unwrap();
            assert_eq!(
                load_loadouts(&conn)[0].name,
                "Kept",
                "zero bytes: {zero_bytes}"
            );
            assert_eq!(set_aside(&path).len(), 1, "the empty file is kept too");
            assert!(notices_for(&path).iter().any(|n| n.contains("restored")));
        }
    }

    #[test]
    fn an_empty_database_without_a_snapshot_is_just_opened() {
        let path = temp_db("empty_no_snap");
        drop(open(&path).unwrap());
        empty_out(&path, true);
        let conn = open(&path).unwrap();
        assert!(load_loadouts(&conn).is_empty());
        assert!(set_aside(&path).is_empty());
        assert!(notices_for(&path).is_empty());
    }

    #[test]
    fn a_missing_database_with_snapshots_is_not_restored_on_its_own() {
        let path = temp_db("missing_with_snaps");
        let dir = snapshot_dir(&path);
        {
            let mut c = open(&path).unwrap();
            save_loadout(&mut c, None, "Old", &[]).unwrap();
            snapshot(&c, &dir, 5, std::time::Duration::ZERO).unwrap();
        }
        forget_checked(&path);
        for p in wal_shm(&path) {
            let _ = std::fs::remove_file(p);
        }
        std::fs::remove_file(&path).unwrap();
        let conn = open(&path).unwrap();
        assert!(load_loadouts(&conn).is_empty(), "a new database");
        let notices = notices_for(&path);
        let dir = dir.display().to_string();
        assert!(notices.iter().any(|n| n.contains(&dir)), "{notices:?}");
    }

    #[test]
    fn a_newer_silos_database_is_refused_not_set_aside() {
        // A newer Silo's schema this SQLite can't parse reads as "malformed" (SQLITE_CORRUPT).
        let path = temp_db("newer_unparseable");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(&format!(
                "CREATE TABLE t (x); PRAGMA user_version = {};",
                SCHEMA_VERSION + 1
            ))
            .unwrap();
            c.execute_batch(
                "PRAGMA writable_schema = ON;
                 INSERT INTO sqlite_master VALUES
                     ('table', 'f', 'f', 0, 'CREATE TABLE f (x) WITH SOME FUTURE SYNTAX');
                 PRAGMA writable_schema = OFF;",
            )
            .unwrap();
        }
        assert!(is_damaged(&path), "the fixture really reads as damaged");
        let before = std::fs::read(&path).unwrap();
        let err = open(&path).unwrap_err();
        assert!(err.contains("newer version of Silo"), "{err}");
        assert!(set_aside(&path).is_empty());
        assert!(std::fs::read(&path).unwrap() == before, "not modified");
    }

    #[test]
    fn the_damage_check_never_writes_the_wal_into_the_file() {
        let path = temp_db("check_readonly");
        drop(open(&path).unwrap());
        {
            let mut c = Connection::open(&path).unwrap();
            c.set_db_config(
                rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
                true,
            )
            .unwrap();
            c.execute_batch("PRAGMA wal_autocheckpoint = 0;").unwrap();
            save_loadout(&mut c, None, "In the WAL", &[]).unwrap();
        }
        assert!(wal_shm(&path)[0].is_file(), "the change sits in the -wal");
        let before = std::fs::read(&path).unwrap();
        forget_checked(&path);
        assert!(!is_damaged(&path));
        assert!(std::fs::read(&path).unwrap() == before, "no checkpoint");
    }

    #[test]
    fn a_healthy_database_is_left_alone() {
        let path = temp_db("healthy");
        drop(open(&path).unwrap());
        forget_checked(&path);
        drop(open(&path).unwrap());
        assert!(set_aside(&path).is_empty());
        assert!(notices_for(&path).is_empty());
    }

    #[test]
    fn snapshots_rotate_and_skip_when_recent() {
        let path = temp_db("snap_rotate");
        let dir = snapshot_dir(&path);
        let c = open(&path).unwrap();
        // A copy an earlier run left half-written, an hour and more ago.
        std::fs::create_dir_all(&dir).unwrap();
        let stale = dir.join("silo-1.db.1-1.tmp");
        std::fs::write(&stale, b"half a copy").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&stale)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(7200))
            .unwrap();
        for _ in 0..7 {
            snapshot(&c, &dir, 5, std::time::Duration::ZERO).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(3));
        }
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            5,
            "keeps the newest five"
        );
        assert!(
            snapshot(&c, &dir, 5, std::time::Duration::from_secs(3600))
                .unwrap()
                .is_none(),
            "a recent snapshot is enough"
        );
    }

    #[test]
    fn a_snapshot_never_keeps_a_token() {
        let path = temp_db("snap_secret");
        let c = open(&path).unwrap();
        set_app_setting(&c, "gh_token", Some("ghp_secret_value")).unwrap();
        set_app_setting(&c, "gh_write", Some("1")).unwrap();
        let snap = snapshot(&c, &snapshot_dir(&path), 5, std::time::Duration::ZERO)
            .unwrap()
            .unwrap();
        let copy = Connection::open(&snap).unwrap();
        assert_eq!(get_app_setting(&copy, "gh_token"), None);
        assert_eq!(get_app_setting(&copy, "gh_write").as_deref(), Some("1"));
        drop(copy);
        let bytes = std::fs::read(&snap).unwrap();
        assert!(
            !bytes.windows(16).any(|w| w == b"ghp_secret_value"),
            "not even in free pages"
        );
    }

    #[test]
    fn an_empty_newer_snapshot_is_skipped_for_an_older_good_one() {
        let path = temp_db("snap_empty_newer");
        let dir = snapshot_dir(&path);
        {
            let mut c = open(&path).unwrap();
            save_loadout(&mut c, None, "Good", &[]).unwrap();
            let good = snapshot(&c, &dir, 5, std::time::Duration::ZERO)
                .unwrap()
                .unwrap();
            let ms = snapshot_ms(&good.file_name().unwrap().to_string_lossy()).unwrap();
            // The good copy is older; a 0-byte file (what a failed VACUUM INTO used to leave)
            // carries the newer, already-past date.
            std::fs::rename(&good, dir.join(format!("silo-{}.db", ms - 10_000))).unwrap();
            std::fs::write(dir.join(format!("silo-{ms}.db")), b"").unwrap();
        }
        damage(&path);
        let conn = open(&path).unwrap();
        assert_eq!(load_loadouts(&conn)[0].name, "Good");
    }

    #[test]
    fn a_failed_snapshot_leaves_nothing_behind() {
        let path = temp_db("snap_fail");
        let dir = snapshot_dir(&path);
        // Nothing to copy: the copy would be an empty database, which is never kept.
        let empty = Connection::open_in_memory().unwrap();
        assert!(snapshot(&empty, &dir, 5, std::time::Duration::ZERO).is_err());
        let left: Vec<_> = std::fs::read_dir(&dir)
            .map(|rd| rd.flatten().map(|e| e.file_name()).collect())
            .unwrap_or_default();
        assert!(left.is_empty(), "{left:?}");
    }

    #[test]
    fn a_snapshot_dated_in_the_future_is_never_the_newest() {
        let path = temp_db("snap_future");
        let dir = snapshot_dir(&path);
        {
            let mut c = open(&path).unwrap();
            let id = save_loadout(&mut c, None, "Future", &[]).unwrap();
            let f = snapshot(&c, &dir, 5, std::time::Duration::ZERO)
                .unwrap()
                .unwrap();
            let far = unix_now().as_millis() as u64 + 10 * 86_400_000;
            std::fs::rename(&f, dir.join(format!("silo-{far}.db"))).unwrap();
            delete_loadout(&mut c, id).unwrap();
            save_loadout(&mut c, None, "Past", &[]).unwrap();
            assert!(
                snapshot(&c, &dir, 5, std::time::Duration::from_secs(3600))
                    .unwrap()
                    .is_some(),
                "a future date doesn't count as a recent snapshot"
            );
        }
        damage(&path);
        let conn = open(&path).unwrap();
        assert_eq!(load_loadouts(&conn)[0].name, "Past");
    }

    #[test]
    fn a_damaged_snapshot_is_skipped_for_an_older_good_one() {
        let path = temp_db("snap_skip_bad");
        let dir = snapshot_dir(&path);
        {
            let mut c = open(&path).unwrap();
            save_loadout(&mut c, None, "Good", &[]).unwrap();
            let good = snapshot(&c, &dir, 5, std::time::Duration::ZERO)
                .unwrap()
                .unwrap();
            let newer = good.with_file_name(format!(
                "silo-{}.db",
                snapshot_ms(&good.file_name().unwrap().to_string_lossy()).unwrap() + 1000
            ));
            std::fs::write(newer, b"not a database").unwrap();
        }
        damage(&path);
        let conn = open(&path).unwrap();
        assert_eq!(load_loadouts(&conn)[0].name, "Good");
    }

    #[test]
    fn the_same_mod_can_be_organized_in_two_folders() {
        let conn = open(&temp_db("twofolders")).unwrap();
        let row = |active| OrganizedRow {
            tech_name: "FS25_A".into(),
            file_name: "FS25_A.zip".into(),
            kind: "zip".into(),
            category: "Vehicles".into(),
            subcategory: None,
            active,
        };
        upsert_organized(&conn, "a", &row(true)).unwrap();
        upsert_organized(&conn, "b", &row(false)).unwrap();
        assert!(load_organized(&conn, "a")[0].active);
        assert!(!load_organized(&conn, "b")[0].active);
        assert_eq!(count_organized(&conn, "a"), 1);
        delete_organized(&conn, "b", "FS25_A").unwrap();
        assert_eq!(
            count_organized(&conn, "a"),
            1,
            "deleting in b leaves a alone"
        );
    }

    #[test]
    fn reconcile_adopts_disk_and_prunes_gone() {
        let base = std::env::temp_dir().join(format!("silo_reconcile_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let mut conn = open(&base.join("silo.db")).unwrap();

        // A stale manifest row for a mod no longer archived on disk.
        upsert_organized(
            &conn,
            "r",
            &OrganizedRow {
                tech_name: "FS25_Gone".into(),
                file_name: "FS25_Gone.zip".into(),
                kind: "zip".into(),
                category: "Old".into(),
                subcategory: None,
                active: true,
            },
        )
        .unwrap();

        // Disk truth: one archived mod, not yet in the manifest.
        let desired = vec![OrganizedRow {
            tech_name: "FS25_New".into(),
            file_name: "FS25_New.zip".into(),
            kind: "zip".into(),
            category: "Objects".into(),
            subcategory: None,
            active: false,
        }];
        reconcile_organized(&mut conn, "r", &desired).unwrap();

        let rows = load_organized(&conn, "r");
        assert_eq!(rows.len(), 1, "manifest should match disk exactly");
        assert_eq!(rows[0].tech_name, "FS25_New", "archived mod adopted");
        assert!(
            !rows.iter().any(|r| r.tech_name == "FS25_Gone"),
            "vanished mod pruned"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn source_pages_round_trip() {
        let conn = open(&temp_db("sources")).unwrap();
        assert!(load_sources(&conn).is_empty());

        set_source(&conn, "FS25_A", Some("https://example.com/a")).unwrap();
        let rows = load_sources(&conn);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tech_name, "FS25_A");
        assert_eq!(rows[0].url, "https://example.com/a");
        assert_eq!(rows[0].last_checked, None);

        assert!(mark_source_checked(&conn, "FS25_A", 1_700_000_000).unwrap());
        assert_eq!(load_sources(&conn)[0].last_checked, Some(1_700_000_000));

        // Saving the same page again keeps the date; a different page starts over.
        set_source(&conn, "FS25_A", Some("https://example.com/a")).unwrap();
        assert_eq!(load_sources(&conn)[0].last_checked, Some(1_700_000_000));
        set_source(&conn, "FS25_A", Some("https://example.com/b")).unwrap();
        let rows = load_sources(&conn);
        assert_eq!(rows[0].url, "https://example.com/b");
        assert_eq!(rows[0].last_checked, None);

        // Marking a mod with no saved page changes nothing.
        assert!(!mark_source_checked(&conn, "FS25_None", 1).unwrap());

        // None (or empty) removes the row.
        set_source(&conn, "FS25_A", None).unwrap();
        assert!(load_sources(&conn).is_empty());
        set_source(&conn, "FS25_A", Some("https://example.com/a")).unwrap();
        set_source(&conn, "FS25_A", Some("  ")).unwrap();
        assert!(load_sources(&conn).is_empty());
    }

    #[test]
    fn has_repo_reports_a_linked_repo() {
        let conn = open(&temp_db("hasrepo")).unwrap();
        assert!(!has_repo(&conn, "FS25_A"));
        set_repo(&conn, "FS25_A", "owner", "repo").unwrap();
        assert!(has_repo(&conn, "FS25_A"));
        assert!(!has_repo(&conn, "FS25_B"));
    }
}

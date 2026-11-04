//! Catalog links for zips Silo installed under another name before #388 (#402).
//!
//! Since #388 a Browse install records which catalog entry a zip came from when the file isn't
//! named after the entry's techName (`catalogalias`). A zip installed that way earlier has no
//! record, so its drawer said "Not in Silo's catalog", update checks skipped it and Verify
//! integrity said Unverified. This looks such zips up by their bytes and records the same link
//! an install would have.
//!
//! A zip is looked at when its file name is its techName (as the joins require), the catalog
//! has no entry with that techName (`POST /mods/lookup`), and its bytes have no link yet. Silo
//! hashes it and asks the catalog which entries have a build with exactly those bytes
//! (`POST /mods/by-hash`: one request for a run's files, answered from one query on the
//! server, silo-api#39). It records the link when exactly one entry has, that entry's build of
//! those bytes downloads as a file of the zip's own name (a Browse install saves the file under
//! its download's name), and the entry isn't named after the zip. Otherwise it records nothing.
//! The record is an install's (`catalogalias::record`), made under the library lock.
//!
//! When the catalog has no `by-hash` route yet (a SiloAPI from before it), nothing is decided;
//! that's logged once per session, and the files are looked at once it has one.
//!
//! When it runs: it sends some mod names to the catalog, so only with the user's leave to
//! check for updates, and never for a mod left out of update checks: after a library scan when
//! the startup update check is on, and beside an update check of the library. Off the UI
//! thread, in the background, one run at a time (a run asked for while one is going is
//! skipped; a later one takes its files). The window hears which mods were linked
//! (`catalog:linked`), and the drawer and the Updates list ask again.
//!
//! Each version of a file (its path, size and modified time, as the scan cache keys it) is
//! decided once: then it isn't hashed or looked up again until the file changes. A failed or
//! offline lookup, a busy library, or a file that can't be read or changes meanwhile decides
//! nothing, so that file is tried again next time, behind files not tried yet; a hash already
//! taken is kept for it, and so is the catalog's "no entry has this techName" (for a day), so
//! the next runs hash such a file without asking for its name again. A run stops at its `Limits`
//! (names looked up, files hashed, time, checked before every request and file; a record waits
//! for the library lock no longer than the run has left, and only once a run); the rest wait
//! for the next run. A run's notes go to the database in at most three transactions, and a
//! busy database stops the run. Whether a file's bytes already have a link is only decided
//! under the library lock, where an install's own record can't be half done. The answer to the hash lookup is read whole or refused (at most 1 MiB).
//! Nothing here is shown to the user as an error; it's only logged.

use crate::catalogalias::{self, Link};
use crate::db;
use crate::siloapi::{self, HashMatch};
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// The most files one run hashes and looks up.
pub const MAX_HASHED: usize = 25;

/// How much one run may do: files looked up by name (one request), files hashed (or tried),
/// and time (checked before each request and each file, so a run ends within about one
/// request or one file's hashing of it).
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_named: usize,
    pub max_hashed: usize,
    pub time: std::time::Duration,
    /// The longest one database write waits for a busy database (rusqlite's own wait).
    pub db_wait: std::time::Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_named: siloapi::LOOKUP_BATCH,
            max_hashed: MAX_HASHED,
            time: std::time::Duration::from_secs(60),
            db_wait: DB_WAIT,
        }
    }
}

/// A zip from a library scan.
#[derive(Debug, Clone)]
pub struct LibraryZip {
    pub path: PathBuf,
    /// The techName the library knows it by (its file name without `.zip`).
    pub tech_name: String,
    /// The size and modified time the scan saw.
    pub size: u64,
    pub mtime_ms: u64,
}

/// What the backfill asks the catalog.
pub trait Catalog {
    /// Which of `names` are an entry's techName, exactly.
    fn known_tech_names(&self, names: &[String]) -> Result<HashSet<String>, String>;
    /// Every catalog build with one of these whole-zip SHA-256s (lowercase), by hash, every hash
    /// asked present; `Ok(None)` when the catalog can't answer this (no such route).
    fn by_hash(&self, hashes: &[String])
        -> Result<Option<HashMap<String, Vec<HashMatch>>>, String>;
}

/// The catalog at `base`.
pub struct SiloApi {
    pub base: String,
}

impl Catalog for SiloApi {
    fn known_tech_names(&self, names: &[String]) -> Result<HashSet<String>, String> {
        Ok(siloapi::lookup(&self.base, names)?
            .into_iter()
            .filter_map(|r| r.tech_name)
            .collect())
    }

    fn by_hash(
        &self,
        hashes: &[String],
    ) -> Result<Option<HashMap<String, Vec<HashMatch>>>, String> {
        siloapi::by_hash(&self.base, hashes)
    }
}

/// What the backfill made of one zip.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Linked to this catalog entry.
    Linked(String),
    /// The catalog has an entry with its techName.
    InCatalog,
    /// Its bytes already have a link.
    AlreadyLinked,
    /// No entry has a build with its bytes.
    NoMatch,
    /// This many entries have a build with its bytes, so it can't be told which it came from.
    Several(usize),
    /// The one entry with its bytes downloads them under another file name, or is named after
    /// the zip: not what a Browse install of this file was.
    OtherName,
    /// Nothing was decided (why); it's looked at again next time.
    Retry(String),
}

impl Outcome {
    /// Whether this settles the file's current version.
    fn decided(&self) -> bool {
        !matches!(self, Outcome::Retry(_))
    }
}

/// Set once it was logged that the catalog can't look up by hash yet (once a session).
static NO_ROUTE_LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Why files wait when the catalog can't look up by hash yet.
const NO_ROUTE: &str = "Silo's catalog can't look mods up by their files yet";

/// Why files wait when a run has done all it may.
const BUDGET_USED: &str = "this run has done all it may; the next one goes on";

/// Why files wait when the library lock couldn't be had this run.
const LIBRARY_BUSY: &str = "Silo's library was busy";

/// How long the catalog's "no entry has this techName" is trusted for a file, so a file it said
/// that about goes straight to hashing in the next runs instead of being asked by name again. A
/// day: an entry gaining that techName meanwhile is joined by techName within a day after it.
const UNNAMED_FRESH_MS: i64 = 24 * 60 * 60 * 1000;

/// A zip to look at, with what was noted about this version of it on an earlier try.
struct Pending<'a> {
    zip: &'a LibraryZip,
    noted: Option<db::AliasCheck>,
    /// When the catalog last said it has no entry with the zip's techName, if lately.
    unnamed_ms: Option<i64>,
}

impl Pending<'_> {
    fn sha(&self) -> Option<&str> {
        self.noted.as_ref().and_then(|n| n.sha256.as_deref())
    }
}

/// Look up the zips in `zips` that need it (see the module notes) and record the links found,
/// within the default `Limits`. `hash` gives a file's whole-file SHA-256; `lock` takes the
/// library lock for a record, waiting at most the time it's given. Never fails: what couldn't
/// be done is logged and tried again on a later run. Returns what it made of each zip it looked
/// at.
pub fn run<G>(
    conn: &Connection,
    zips: &[LibraryZip],
    catalog: &impl Catalog,
    hash: impl Fn(&Path) -> Result<String, String>,
    lock: impl Fn(std::time::Duration) -> Result<G, String>,
) -> Vec<(PathBuf, Outcome)> {
    run_with(conn, zips, catalog, hash, lock, Limits::default())
}

/// `run` within `limits`. Its notes go to the database in at most three transactions (the name
/// batch with what it decided, the hashed files, the rest of what it decided); when one fails
/// (the database busy past its wait), the run stops and the files it hadn't settled wait.
pub fn run_with<G>(
    conn: &Connection,
    zips: &[LibraryZip],
    catalog: &impl Catalog,
    hash: impl Fn(&Path) -> Result<String, String>,
    lock: impl Fn(std::time::Duration) -> Result<G, String>,
    limits: Limits,
) -> Vec<(PathBuf, Outcome)> {
    let out = run_bounded(conn, zips, catalog, hash, lock, limits);
    // Back to the connection's usual wait for a busy database.
    let _ = conn.busy_timeout(DB_WAIT);
    out
}

/// rusqlite's own wait for a busy database, which `run_with` narrows to the run's time.
const DB_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Let the next database write wait for a busy database no longer than the run has left, nor
/// `most` (and not at all once it has none left: a free database still takes it at once).
fn wait_at_most(conn: &Connection, until: std::time::Instant, most: std::time::Duration) {
    let left = until.saturating_duration_since(std::time::Instant::now());
    let _ = conn.busy_timeout(left.min(most));
}

/// The mods the user left out of update checks, read again before a request goes out, so one
/// left out while the run was going isn't sent (#333).
fn left_out(conn: &Connection) -> Result<HashSet<String>, String> {
    db::skip_update_names(conn).map_err(|e| format!("couldn't read which mods to leave out: {e}"))
}

fn run_bounded<G>(
    conn: &Connection,
    zips: &[LibraryZip],
    catalog: &impl Catalog,
    hash: impl Fn(&Path) -> Result<String, String>,
    lock: impl Fn(std::time::Duration) -> Result<G, String>,
    limits: Limits,
) -> Vec<(PathBuf, Outcome)> {
    let until = std::time::Instant::now() + limits.time;
    let late = || std::time::Instant::now() >= until;
    let mut out = Vec::new();
    // Mods the user left out of update checks (#333) aren't asked about either.
    let skipped = match db::skip_update_names(conn) {
        Ok(s) => s,
        Err(e) => {
            log::warn!(
                "[catalog] link backfill skipped: couldn't read which mods to leave out: {e}"
            );
            return out;
        }
    };
    let now = now_ms();
    // This run's tries are numbered after every earlier one, whatever the clock says (two runs,
    // or two stages of one, can fall in the same millisecond): the name batch at `first_try`,
    // the hashed files after it, so the order of tries is always the order they were made in.
    let first_try = match db::last_alias_attempt(conn) {
        Ok(last) => now.max(last.unwrap_or(i64::MIN).saturating_add(1)),
        Err(e) => {
            log::warn!("[catalog] link backfill skipped: couldn't read what it checked: {e}");
            return out;
        }
    };
    let mut pending = Vec::new();
    for z in zips {
        if catalogalias::zip_tech_name(&z.path).as_deref() != Some(z.tech_name.as_str())
            || skipped.contains(&z.tech_name)
        {
            continue;
        }
        let noted = match db::alias_check(conn, &path_key(z)) {
            Ok(n) => n.filter(|n| n.size == z.size && n.mtime_ms == z.mtime_ms),
            Err(e) => {
                log::warn!("[catalog] link backfill skipped: couldn't read what it checked: {e}");
                return out;
            }
        };
        if noted.as_ref().is_some_and(|n| n.decided) {
            continue;
        }
        let unnamed_ms = noted
            .as_ref()
            .and_then(|n| n.unnamed_ms)
            .filter(|t| now.saturating_sub(*t) < UNNAMED_FRESH_MS);
        pending.push(Pending {
            zip: z,
            noted,
            unnamed_ms,
        });
    }
    if pending.is_empty() {
        return out;
    }
    // Files never tried first, then the ones that have waited longest, so a file that keeps
    // failing never holds up the rest.
    pending.sort_by_key(|p| p.noted.as_ref().map(|n| n.attempted_ms));

    // A zip the catalog knows by its own name is joined by that and needs no link. Files it
    // said lately it doesn't know go straight on; at most one name lookup's worth of the others.
    let mut kept = Vec::with_capacity(pending.len());
    let mut to_name = 0;
    for p in pending {
        if p.unnamed_ms.is_none() {
            if to_name == limits.max_named {
                out.push((p.zip.path.clone(), Outcome::Retry(BUDGET_USED.to_string())));
                continue;
            }
            to_name += 1;
        }
        kept.push(p);
    }
    let mut pending = kept;
    if late() {
        retry_all(pending.iter().map(|p| p.zip), BUDGET_USED, &mut out);
        return out;
    }
    // Read again just before anything is sent: a mod left out meanwhile isn't asked about.
    match left_out(conn) {
        Ok(skip) => pending.retain(|p| !skip.contains(&p.zip.tech_name)),
        Err(e) => {
            log::warn!("[catalog] link backfill stopped: {e}");
            retry_all(pending.iter().map(|p| p.zip), &e, &mut out);
            return out;
        }
    }
    let to_name = pending.iter().filter(|p| p.unnamed_ms.is_none()).count();
    if to_name > 0 {
        let names: Vec<String> = pending
            .iter()
            .filter(|p| p.unnamed_ms.is_none())
            .map(|p| p.zip.tech_name.clone())
            .collect();
        let answer = catalog.known_tech_names(&names);
        // The batch is noted at once, in one transaction: each file as tried (any hash kept), so
        // the next batch starts with files not tried yet; the ones the catalog knows by name as
        // settled; the ones it doesn't with when it said so, for the next runs to hash without
        // asking again.
        let asked_at = now_ms();
        let mut notes = Vec::new();
        let mut known = Vec::new();
        for p in pending.iter_mut().filter(|p| p.unnamed_ms.is_none()) {
            let decided = match &answer {
                Ok(k) if k.contains(&p.zip.tech_name) => true,
                Ok(_) => {
                    p.unnamed_ms = Some(asked_at);
                    false
                }
                Err(_) => false,
            };
            if decided {
                known.push(p.zip);
            }
            notes.push((path_key(p.zip), check_of(p, p.sha(), decided, first_try)));
        }
        wait_at_most(conn, until, limits.db_wait);
        if let Err(e) = db::note_alias_checks(conn, &notes) {
            log::warn!("[catalog] link backfill stopped: couldn't note what it looked up: {e}");
            retry_all(pending.iter().map(|p| p.zip), &e, &mut out);
            return out;
        }
        if let Err(e) = answer {
            log::info!(
                "[catalog] link backfill couldn't look mods up by name, trying next time: {e}"
            );
            retry_all(
                pending
                    .iter()
                    .filter(|p| p.unnamed_ms.is_none())
                    .map(|p| p.zip),
                &e,
                &mut out,
            );
        }
        for z in &known {
            log::debug!("[catalog] {}: not linked (InCatalog)", file_name(z));
            out.push((z.path.clone(), Outcome::InCatalog));
        }
        // On with the files the catalog doesn't know by name (this run's answer, or lately).
        pending.retain(|p| p.unnamed_ms.is_some());
    }

    // Hash (or take the noted hash of) at most `max_hashed` of them; every one tried is noted as
    // tried, in one transaction, so files that keep failing rotate behind the rest.
    let mut asked: Vec<(&LibraryZip, String)> = Vec::new();
    let mut notes = Vec::new();
    let mut tried = 0;
    for p in &pending {
        if tried >= limits.max_hashed || late() {
            out.push((p.zip.path.clone(), Outcome::Retry(BUDGET_USED.to_string())));
            continue;
        }
        tried += 1;
        let (got, sha) = bytes_of(p, &hash);
        notes.push((
            path_key(p.zip),
            check_of(p, sha.as_deref(), false, first_try + 1),
        ));
        match got {
            Ok(sha) => asked.push((p.zip, sha)),
            Err(why) => {
                log::info!(
                    "[catalog] {}: no catalog link yet, trying next time ({why})",
                    file_name(p.zip)
                );
                out.push((p.zip.path.clone(), Outcome::Retry(why)));
            }
        }
    }
    wait_at_most(conn, until, limits.db_wait);
    if let Err(e) = db::note_alias_checks(conn, &notes) {
        log::warn!("[catalog] link backfill stopped: couldn't note the files it hashed: {e}");
        retry_all(asked.iter().map(|(z, _)| *z), &e, &mut out);
        return out;
    }
    if asked.is_empty() {
        return out;
    }
    if late() {
        retry_all(asked.iter().map(|(z, _)| *z), BUDGET_USED, &mut out);
        return out;
    }
    // Read again just before the hashes go out: a mod left out while they were taken isn't
    // looked up (nor linked).
    match left_out(conn) {
        Ok(skip) => asked.retain(|(z, _)| !skip.contains(&z.tech_name)),
        Err(e) => {
            log::warn!("[catalog] link backfill stopped: {e}");
            retry_all(asked.iter().map(|(z, _)| *z), &e, &mut out);
            return out;
        }
    }
    if asked.is_empty() {
        return out;
    }

    let hashes: Vec<String> = asked.iter().map(|(_, sha)| sha.clone()).collect();
    let found = match catalog.by_hash(&hashes) {
        Ok(Some(found)) => found,
        Ok(None) => {
            if !NO_ROUTE_LOGGED.swap(true, std::sync::atomic::Ordering::SeqCst) {
                log::info!(
                    "[catalog] {NO_ROUTE}; older installs get their catalog links once it can"
                );
            }
            retry_all(asked.iter().map(|(z, _)| *z), NO_ROUTE, &mut out);
            return out;
        }
        Err(e) => {
            log::info!("[catalog] link backfill skipped, trying next time: {e}");
            retry_all(asked.iter().map(|(z, _)| *z), &e, &mut out);
            return out;
        }
    };
    // Once the library lock couldn't be had, or a record couldn't be written (a busy
    // database), no further record is tried this run. A record's write waits for the database
    // no longer than the run has left.
    let stop = std::cell::Cell::new(false);
    // Set when the database failed a record: then nothing more is written this run.
    let db_down = std::cell::Cell::new(false);
    let take = || -> Result<G, String> {
        if stop.get() || db_down.get() {
            return Err(LIBRARY_BUSY.to_string());
        }
        let left = until.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return Err(BUDGET_USED.to_string());
        }
        let guard = lock(left).inspect_err(|_| stop.set(true))?;
        wait_at_most(conn, until, limits.db_wait);
        Ok(guard)
    };
    let mut decided = Vec::new();
    let mut outcomes = Vec::new();
    for (z, sha) in asked {
        let outcome = match found.get(&sha) {
            Some(matches) => judge(conn, z, &sha, matches, &take, &db_down),
            None => Outcome::Retry("Silo's catalog left this file out of its answer".to_string()),
        };
        log_outcome(z, &outcome);
        if outcome.decided() {
            let check = db::AliasCheck {
                size: z.size,
                mtime_ms: z.mtime_ms,
                sha256: Some(sha.clone()),
                unnamed_ms: None,
                decided: true,
                attempted_ms: first_try + 1,
            };
            decided.push((path_key(z), check));
        }
        outcomes.push((z.path.clone(), outcome));
    }
    // What it decided, in one transaction, waiting no longer than the run has left. When that
    // fails, those files are looked at again next time (a link recorded meanwhile is then found
    // as already there).
    if db_down.get() {
        log::warn!("[catalog] link backfill stopped writing: the database is busy");
        out.extend(outcomes);
        return out;
    }
    wait_at_most(conn, until, limits.db_wait);
    if let Err(e) = db::note_alias_checks(conn, &decided) {
        log::warn!(
            "[catalog] couldn't note what the link backfill decided, so it will look again: {e}"
        );
    }
    out.extend(outcomes);
    out
}

/// The zip's whole-file SHA-256 for the version the scan saw: the one noted on an earlier try,
/// or hashed now. Also the hash to note for the file: the one taken, or the noted one (none
/// when the file changed since the scan).
fn bytes_of(
    p: &Pending,
    hash: &impl Fn(&Path) -> Result<String, String>,
) -> (Result<String, String>, Option<String>) {
    let z = p.zip;
    let noted = p.sha().map(str::to_string);
    if let Err(why) = unchanged(z) {
        return (Err(why), noted);
    }
    if let Some(sha) = noted {
        return (Ok(sha.clone()), Some(sha));
    }
    let taken = hash(&z.path).map(|h| h.to_ascii_lowercase());
    // Hashed as the scan saw it, not mid-change.
    if let Err(why) = unchanged(z) {
        return (Err(why), None);
    }
    match taken {
        Ok(sha) => (Ok(sha.clone()), Some(sha)),
        Err(e) => (Err(format!("couldn't read it: {e}")), None),
    }
}

/// What the catalog's builds with the bytes `sha` of the zip `z` make of it, linking it when
/// they show where it came from.
fn judge<G>(
    conn: &Connection,
    z: &LibraryZip,
    sha: &str,
    matches: &[HashMatch],
    lock: &impl Fn() -> Result<G, String>,
    stop: &std::cell::Cell<bool>,
) -> Outcome {
    let mut ids: Vec<&str> = matches.iter().map(|m| m.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    let id = match ids.as_slice() {
        [] => return Outcome::NoMatch,
        [one] => *one,
        several => return Outcome::Several(several.len()),
    };
    let file = file_name(z);
    // The entry's build of these bytes that downloads as this file, as a Browse install saved it.
    let Some(build) = matches
        .iter()
        .find(|m| m.file_name.as_deref() == Some(file))
    else {
        return Outcome::OtherName;
    };
    if !catalogalias::needs_alias(file, build.tech_name.as_deref()) {
        return Outcome::OtherName;
    }
    link(conn, z, sha, id, build, lock, stop)
}

/// Record the link an install would have, under the library lock, unless the file changed or
/// its bytes have a link. Whether they have one is only read under the lock: an install writes
/// its record before placing its file and takes it back if placing fails, all under the lock,
/// so a record seen without it may not last.
fn link<G>(
    conn: &Connection,
    z: &LibraryZip,
    sha: &str,
    id: &str,
    build: &HashMatch,
    lock: &impl Fn() -> Result<G, String>,
    stop: &std::cell::Cell<bool>,
) -> Outcome {
    let _write = match lock() {
        Ok(g) => g,
        Err(e) => return Outcome::Retry(e),
    };
    if let Err(why) = unchanged(z) {
        return Outcome::Retry(why);
    }
    match catalogalias::recorded_entry(conn, &z.tech_name, sha) {
        Ok(Some(_)) => return Outcome::AlreadyLinked,
        Ok(None) => {}
        Err(e) => {
            stop.set(true);
            return Outcome::Retry(e);
        }
    }
    let link = Link {
        catalog_id: id.to_string(),
        catalog_tech_name: build.tech_name.clone(),
        download_url: build.download_url.clone(),
    };
    match catalogalias::record(conn, &z.tech_name, sha, z.size, &link) {
        Ok(_) => Outcome::Linked(id.to_string()),
        Err(e) => {
            stop.set(true);
            Outcome::Retry(e)
        }
    }
}

/// Log what was made of `z`.
fn log_outcome(z: &LibraryZip, outcome: &Outcome) {
    let label = file_name(z);
    match outcome {
        Outcome::Linked(id) => log::info!("[catalog] {label} linked to catalog entry {id}"),
        Outcome::Retry(why) => {
            log::info!("[catalog] {label}: no catalog link yet, trying next time ({why})")
        }
        Outcome::Several(n) => {
            log::info!("[catalog] {label}: {n} catalog entries have these bytes; not linked")
        }
        other => log::debug!("[catalog] {label}: not linked ({other:?})"),
    }
}

/// What to note about this version of `p`'s file: `sha` once taken, what the catalog said lately
/// about its name, whether it's settled, and the try's number (`tried`).
fn check_of(p: &Pending, sha: Option<&str>, decided: bool, tried: i64) -> db::AliasCheck {
    db::AliasCheck {
        size: p.zip.size,
        mtime_ms: p.zip.mtime_ms,
        sha256: sha.map(str::to_string),
        unnamed_ms: p.unnamed_ms,
        decided,
        attempted_ms: tried,
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn retry_all<'a>(
    zips: impl Iterator<Item = &'a LibraryZip>,
    why: &str,
    out: &mut Vec<(PathBuf, Outcome)>,
) {
    out.extend(zips.map(|z| (z.path.clone(), Outcome::Retry(why.to_string()))));
}

/// The file still has the size and modified time the scan saw.
fn unchanged(z: &LibraryZip) -> Result<(), String> {
    let meta = std::fs::metadata(&z.path).map_err(|e| format!("couldn't read it: {e}"))?;
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    if meta.is_file() && meta.len() == z.size && mtime_ms == z.mtime_ms {
        Ok(())
    } else {
        Err("it changed since the scan".to_string())
    }
}

/// The key the scan cache uses for the file.
fn path_key(z: &LibraryZip) -> String {
    z.path.to_string_lossy().into_owned()
}

fn file_name(z: &LibraryZip) -> &str {
    z.path.file_name().and_then(|n| n.to_str()).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "silo_aliasbackfill_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn sha(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    const TODO_URL: &str =
        "https://github.com/knoellix/FS25_ToDo/releases/download/v0.1.0.17/FS25_FieldToDoList.zip";

    /// `FS25_ToDo`'s build that downloads as `FS25_FieldToDoList.zip`.
    fn todo_build() -> HashMatch {
        HashMatch {
            id: "cat-todo".into(),
            tech_name: Some("FS25_ToDo".into()),
            version: "v0.1.0.17".into(),
            source: "github".into(),
            file_name: Some("FS25_FieldToDoList.zip".into()),
            download_url: Some(TODO_URL.into()),
        }
    }

    /// A fake catalog: the builds by hash and the techNames it knows. It counts what it's asked
    /// and can be told to fail, or to have no by-hash route.
    #[derive(Default)]
    struct Fake {
        builds: HashMap<String, Vec<HashMatch>>,
        known: HashSet<String>,
        fail_known: bool,
        fail_by_hash: bool,
        no_route: bool,
        /// How long a name lookup takes.
        slow_known: std::time::Duration,
        asked_known: Cell<usize>,
        asked_names: RefCell<Vec<String>>,
        asked_by_hash: RefCell<Vec<Vec<String>>>,
    }

    impl Fake {
        fn with(bytes: &[u8], builds: Vec<HashMatch>) -> Self {
            Fake::default().and(bytes, builds)
        }
        fn and(mut self, bytes: &[u8], builds: Vec<HashMatch>) -> Self {
            self.builds.insert(sha(bytes), builds);
            self
        }
        fn asked(&self) -> usize {
            self.asked_known.get() + self.asked_by_hash.borrow().len()
        }
    }

    impl Catalog for Fake {
        fn known_tech_names(&self, names: &[String]) -> Result<HashSet<String>, String> {
            self.asked_known.set(self.asked_known.get() + 1);
            self.asked_names.borrow_mut().extend(names.iter().cloned());
            std::thread::sleep(self.slow_known);
            if self.fail_known {
                return Err("offline".into());
            }
            Ok(names
                .iter()
                .filter(|n| self.known.contains(*n))
                .cloned()
                .collect())
        }
        fn by_hash(
            &self,
            hashes: &[String],
        ) -> Result<Option<HashMap<String, Vec<HashMatch>>>, String> {
            self.asked_by_hash.borrow_mut().push(hashes.to_vec());
            if self.fail_by_hash {
                return Err("Silo can't reach its catalog right now".into());
            }
            if self.no_route {
                return Ok(None);
            }
            Ok(Some(
                hashes
                    .iter()
                    .map(|h| (h.clone(), self.builds.get(h).cloned().unwrap_or_default()))
                    .collect(),
            ))
        }
    }

    /// The library zip at `path` as a scan sees it now.
    fn scanned(path: &Path) -> LibraryZip {
        let meta = std::fs::metadata(path).unwrap();
        LibraryZip {
            path: path.to_path_buf(),
            tech_name: catalogalias::zip_tech_name(path).unwrap(),
            size: meta.len(),
            mtime_ms: meta
                .modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
        }
    }

    fn write_zip(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    /// Run the backfill with real hashing (counted) and a free library lock.
    fn backfill(
        conn: &Connection,
        zips: &[LibraryZip],
        catalog: &Fake,
        hashed: &Cell<usize>,
    ) -> Vec<(PathBuf, Outcome)> {
        let hash = |p: &Path| {
            hashed.set(hashed.get() + 1);
            crate::provenance::sha256_file(p)
        };
        run(conn, zips, catalog, hash, |_| Ok(()))
    }

    fn setup(tag: &str) -> (PathBuf, Connection) {
        let dir = temp_dir(tag);
        let conn = db::open(&dir.join("silo.db")).unwrap();
        (dir, conn)
    }

    /// Whether this version of the file is settled.
    fn settled(conn: &Connection, z: &LibraryZip) -> bool {
        db::alias_check(conn, &path_key(z))
            .unwrap()
            .is_some_and(|n| n.decided && n.size == z.size && n.mtime_ms == z.mtime_ms)
    }

    #[test]
    fn one_entry_with_the_bytes_links_the_zip_for_the_drawer_update_and_verify_joins() {
        let (dir, conn) = setup("one");
        let file = write_zip(&dir, "FS25_FieldToDoList.zip", b"build one");
        // The same entry may have the bytes under two builds (re-tagged): still one entry.
        let retagged = HashMatch {
            version: "v0.1.0.17b".into(),
            ..todo_build()
        };
        let fake = Fake::with(b"build one", vec![todo_build(), retagged]);
        let hashed = Cell::new(0);
        let out = backfill(&conn, &[scanned(&file)], &fake, &hashed);
        assert_eq!(out, [(file.clone(), Outcome::Linked("cat-todo".into()))]);
        assert_eq!(hashed.get(), 1);
        assert_eq!(*fake.asked_by_hash.borrow(), [vec![sha(b"build one")]]);
        // The drawer, update checks and sharing join through `link_for_file`; Verify integrity
        // through `link_for_hash`. Both find the entry, with the row an install would write.
        let want = Link {
            catalog_id: "cat-todo".into(),
            catalog_tech_name: Some("FS25_ToDo".into()),
            download_url: Some(TODO_URL.into()),
        };
        assert_eq!(
            catalogalias::link_for_file(&conn, "FS25_FieldToDoList", &file).unwrap(),
            Some(want.clone())
        );
        assert_eq!(
            catalogalias::link_for_hash(&conn, "FS25_FieldToDoList", &file, &sha(b"build one"))
                .unwrap(),
            Some(want)
        );
        let rows = db::catalog_aliases(&conn, "FS25_FieldToDoList").unwrap();
        assert_eq!(rows[0].size, 9);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_entry_or_several_with_the_bytes_record_nothing() {
        let (dir, conn) = setup("none_several");
        let none = write_zip(&dir, "FS25_FieldToDoList.zip", b"edited by hand");
        let both = write_zip(&dir, "FS25_StatsExtended_update.zip", b"shared build");
        let fork = HashMatch {
            id: "cat-fork".into(),
            tech_name: Some("FS25_StatsFork".into()),
            file_name: Some("FS25_StatsExtended_update.zip".into()),
            ..todo_build()
        };
        let stats = HashMatch {
            id: "cat-stats".into(),
            tech_name: Some("FS25_statsExtended".into()),
            ..fork.clone()
        };
        let fake = Fake::with(b"shared build", vec![stats, fork]);
        let zips = [scanned(&none), scanned(&both)];
        let out = backfill(&conn, &zips, &fake, &Cell::new(0));
        assert_eq!(out[0].1, Outcome::NoMatch);
        assert_eq!(out[1].1, Outcome::Several(2));
        // Both looked up in one request.
        assert_eq!(fake.asked_by_hash.borrow().len(), 1);
        assert_eq!(fake.asked_by_hash.borrow()[0].len(), 2);
        assert!(!catalogalias::has_any(&conn, "FS25_FieldToDoList").unwrap());
        assert!(!catalogalias::has_any(&conn, "FS25_StatsExtended_update").unwrap());
        assert!(zips.iter().all(|z| settled(&conn, z)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_one_entry_must_have_shipped_the_bytes_under_the_zips_name() {
        let (dir, conn) = setup("other_name");
        let file = write_zip(&dir, "FS25_FieldToDoList.zip", b"build one");
        // The entry's build of these bytes downloads as another file, or as nothing known.
        let elsewhere = HashMatch {
            file_name: Some("FS25_ToDo_v17.zip".into()),
            ..todo_build()
        };
        let unnamed = HashMatch {
            file_name: None,
            ..todo_build()
        };
        for builds in [vec![elsewhere], vec![unnamed]] {
            let fake = Fake::with(b"build one", builds);
            let out = backfill(&conn, &[scanned(&file)], &fake, &Cell::new(0));
            assert_eq!(out[0].1, Outcome::OtherName);
            assert!(!catalogalias::has_any(&conn, "FS25_FieldToDoList").unwrap());
            conn.execute("DELETE FROM alias_check", []).unwrap();
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_named_after_its_entry_or_known_by_its_tech_name_is_skipped() {
        let (dir, conn) = setup("named");
        let file = write_zip(&dir, "FS25_ToDo.zip", b"build one");
        let mut fake = Fake::with(b"build one", vec![todo_build()]);
        fake.known.insert("FS25_ToDo".into());
        let hashed = Cell::new(0);
        let out = backfill(&conn, &[scanned(&file)], &fake, &hashed);
        assert_eq!(out[0].1, Outcome::InCatalog);
        assert_eq!(hashed.get(), 0, "not hashed");
        assert!(fake.asked_by_hash.borrow().is_empty());
        // Named after its entry, though the catalog's exact lookup missed it: the entry's build
        // of its bytes carries the zip's own name, so it needs no alias.
        let file = write_zip(&dir, "fs25_todo.zip", b"build two");
        let own = HashMatch {
            tech_name: Some("fs25_todo".into()),
            file_name: Some("fs25_todo.zip".into()),
            ..todo_build()
        };
        let fake = Fake::with(b"build two", vec![own]);
        let out = backfill(&conn, &[scanned(&file)], &fake, &Cell::new(0));
        assert_eq!(out[0].1, Outcome::OtherName);
        assert!(!catalogalias::has_any(&conn, "fs25_todo").unwrap());
        // A zip the library knows by another name isn't looked at at all.
        let renamed = LibraryZip {
            tech_name: "FS25_Else".into(),
            ..scanned(&file)
        };
        let fake = Fake::default();
        assert!(backfill(&conn, &[renamed], &fake, &Cell::new(0)).is_empty());
        assert_eq!(fake.asked(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unchanged_file_is_not_hashed_or_looked_up_again() {
        let (dir, conn) = setup("unchanged");
        let linked = write_zip(&dir, "FS25_FieldToDoList.zip", b"build one");
        let unmatched = write_zip(&dir, "FS25_Unlisted.zip", b"not listed");
        let known = write_zip(&dir, "FS25_Known.zip", b"known");
        let mut fake = Fake::with(b"build one", vec![todo_build()]);
        fake.known.insert("FS25_Known".into());
        let zips = [scanned(&linked), scanned(&unmatched), scanned(&known)];
        let hashed = Cell::new(0);
        assert_eq!(backfill(&conn, &zips, &fake, &hashed).len(), 3);
        assert_eq!(hashed.get(), 2);
        let asked = fake.asked();
        let again = backfill(&conn, &zips, &fake, &hashed);
        assert!(again.is_empty(), "{again:?}");
        assert_eq!(hashed.get(), 2, "nothing hashed again");
        assert_eq!(fake.asked(), asked, "nothing looked up again");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_changed_file_is_checked_again() {
        let (dir, conn) = setup("changed");
        let file = write_zip(&dir, "FS25_FieldToDoList.zip", b"a build nobody lists");
        let fake = Fake::with(b"build one", vec![todo_build()]);
        let hashed = Cell::new(0);
        let out = backfill(&conn, &[scanned(&file)], &fake, &hashed);
        assert_eq!(out[0].1, Outcome::NoMatch);
        // The user puts the catalog's build back: another size, another time.
        std::fs::write(&file, b"build one").unwrap();
        let out = backfill(&conn, &[scanned(&file)], &fake, &hashed);
        assert_eq!(out[0].1, Outcome::Linked("cat-todo".into()));
        assert_eq!(hashed.get(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_lookup_records_nothing_and_retries_without_hashing_again() {
        let (dir, conn) = setup("offline");
        let file = write_zip(&dir, "FS25_FieldToDoList.zip", b"build one");
        let zips = [scanned(&file)];
        let hashed = Cell::new(0);
        let no_names = Fake {
            fail_known: true,
            ..Fake::with(b"build one", vec![todo_build()])
        };
        let no_hashes = Fake {
            fail_by_hash: true,
            ..Fake::with(b"build one", vec![todo_build()])
        };
        for fake in [&no_names, &no_hashes, &no_hashes] {
            let out = backfill(&conn, &zips, fake, &hashed);
            assert!(matches!(out[0].1, Outcome::Retry(_)), "{out:?}");
            assert!(!catalogalias::has_any(&conn, "FS25_FieldToDoList").unwrap());
            assert!(!settled(&conn, &zips[0]));
        }
        assert_eq!(hashed.get(), 1, "hashed once, then the noted hash is used");
        // Back online: the same file version is linked, still without hashing it again.
        let fake = Fake::with(b"build one", vec![todo_build()]);
        let out = backfill(&conn, &zips, &fake, &hashed);
        assert_eq!(out[0].1, Outcome::Linked("cat-todo".into()));
        assert_eq!(hashed.get(), 1);
        // A new build of the file is hashed afresh, never judged by the old hash.
        std::fs::write(&file, b"build three!").unwrap();
        backfill(&conn, &[scanned(&file)], &no_hashes, &hashed);
        assert_eq!(hashed.get(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_catalog_without_the_route_decides_nothing_until_it_has_it() {
        let (dir, conn) = setup("no_route");
        let file = write_zip(&dir, "FS25_FieldToDoList.zip", b"build one");
        let zips = [scanned(&file)];
        let old = Fake {
            no_route: true,
            ..Fake::with(b"build one", vec![todo_build()])
        };
        let hashed = Cell::new(0);
        for _ in 0..2 {
            let out = backfill(&conn, &zips, &old, &hashed);
            assert_eq!(out, [(file.clone(), Outcome::Retry(NO_ROUTE.to_string()))]);
            assert!(!settled(&conn, &zips[0]));
            assert!(!catalogalias::has_any(&conn, "FS25_FieldToDoList").unwrap());
        }
        assert!(NO_ROUTE_LOGGED.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(hashed.get(), 1);
        // Once the catalog has it, the file is linked.
        let new = Fake::with(b"build one", vec![todo_build()]);
        let out = backfill(&conn, &zips, &new, &hashed);
        assert_eq!(out[0].1, Outcome::Linked("cat-todo".into()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_mod_left_out_of_update_checks_is_never_asked_about() {
        let (dir, conn) = setup("skip");
        let file = write_zip(&dir, "FS25_FieldToDoList.zip", b"build one");
        db::set_curation(
            &conn,
            &db::CurationRow {
                tech_name: "FS25_FieldToDoList".into(),
                favorite: false,
                hidden: false,
                broken: false,
                rating: 0,
                note: None,
                skip_updates: true,
            },
        )
        .unwrap();
        let fake = Fake::with(b"build one", vec![todo_build()]);
        let hashed = Cell::new(0);
        assert!(backfill(&conn, &[scanned(&file)], &fake, &hashed).is_empty());
        assert_eq!((fake.asked(), hashed.get()), (0, 0));
        // When that can't be read, nothing is asked at all.
        conn.execute_batch("DROP TABLE curation").unwrap();
        assert!(backfill(&conn, &[scanned(&file)], &fake, &hashed).is_empty());
        assert_eq!(fake.asked(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bytes_already_linked_keep_their_link() {
        let (dir, conn) = setup("already");
        let file = write_zip(&dir, "FS25_FieldToDoList.zip", b"build one");
        let installed = Link {
            catalog_id: "cat-installed".into(),
            catalog_tech_name: Some("FS25_ToDo".into()),
            download_url: None,
        };
        catalogalias::record(
            &conn,
            "FS25_FieldToDoList",
            &sha(b"build one"),
            9,
            &installed,
        )
        .unwrap();
        let fake = Fake::with(b"build one", vec![todo_build()]);
        let out = backfill(&conn, &[scanned(&file)], &fake, &Cell::new(0));
        assert_eq!(out[0].1, Outcome::AlreadyLinked);
        assert_eq!(
            catalogalias::link_for_file(&conn, "FS25_FieldToDoList", &file).unwrap(),
            Some(installed)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_busy_library_or_a_changed_or_unreadable_file_waits_for_the_next_run() {
        let (dir, conn) = setup("busy");
        let file = write_zip(&dir, "FS25_FieldToDoList.zip", b"build one");
        let zip = scanned(&file);
        let fake = Fake::with(b"build one", vec![todo_build()]);
        let hash = crate::provenance::sha256_file;
        let busy = |_| -> Result<(), String> { Err("Silo is busy".into()) };
        let out = run(&conn, std::slice::from_ref(&zip), &fake, hash, busy);
        assert!(matches!(out[0].1, Outcome::Retry(_)), "{out:?}");
        let stale = LibraryZip {
            size: zip.size + 1,
            ..zip.clone()
        };
        let out = run(&conn, &[stale], &fake, hash, |_| Ok(()));
        assert!(matches!(out[0].1, Outcome::Retry(_)), "{out:?}");
        assert!(!catalogalias::has_any(&conn, "FS25_FieldToDoList").unwrap());
        let out = run(&conn, std::slice::from_ref(&zip), &fake, hash, |_| Ok(()));
        assert_eq!(out[0].1, Outcome::Linked("cat-todo".into()));
        // A file that can't be read (the game has it open, say) waits too.
        let other = dir.join("other");
        std::fs::create_dir_all(&other).unwrap();
        let file = write_zip(&other, "FS25_FieldToDoList.zip", b"build one!");
        let fake = Fake::with(b"build one!", vec![todo_build()]);
        let zip = scanned(&file);
        let unreadable = |_: &Path| -> Result<String, String> { Err("in use".into()) };
        let out = run(&conn, std::slice::from_ref(&zip), &fake, unreadable, |_| {
            Ok(())
        });
        assert!(matches!(out[0].1, Outcome::Retry(_)), "{out:?}");
        assert!(fake.asked_by_hash.borrow().is_empty());
        let out = run(&conn, &[zip], &fake, hash, |_| Ok(()));
        assert_eq!(out[0].1, Outcome::Linked("cat-todo".into()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `n` zips, each the build of its own entry under its own name, and the catalog of them.
    fn many(dir: &Path, n: usize) -> (Vec<LibraryZip>, Fake) {
        let mut fake = Fake::default();
        let mut zips = Vec::new();
        for i in 0..n {
            let name = format!("FS25_File{i:02}.zip");
            let bytes = format!("bytes {i}");
            zips.push(scanned(&write_zip(dir, &name, bytes.as_bytes())));
            fake = fake.and(
                bytes.as_bytes(),
                vec![HashMatch {
                    id: format!("cat-{i}"),
                    tech_name: Some(format!("FS25_Entry{i}")),
                    file_name: Some(name),
                    ..todo_build()
                }],
            );
        }
        (zips, fake)
    }

    #[test]
    fn one_run_hashes_a_bounded_number_and_asks_once() {
        let (dir, conn) = setup("bounded");
        let (zips, fake) = many(&dir, MAX_HASHED + 2);
        let hashed = Cell::new(0);
        let out = backfill(&conn, &zips, &fake, &hashed);
        assert_eq!(hashed.get(), MAX_HASHED);
        assert_eq!(
            fake.asked_by_hash.borrow().len(),
            1,
            "one request for the run"
        );
        assert_eq!(fake.asked_by_hash.borrow()[0].len(), MAX_HASHED);
        let linked = out
            .iter()
            .filter(|(_, o)| matches!(o, Outcome::Linked(_)))
            .count();
        assert_eq!(linked, MAX_HASHED);
        // The rest go on in the next run.
        let out = backfill(&conn, &zips, &fake, &hashed);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|(_, o)| matches!(o, Outcome::Linked(_))));
        // No time: nothing hashed or looked up by hash, nothing settled.
        let (dir2, conn2) = setup("bounded_time");
        let (zips2, fake2) = many(&dir2, 2);
        let none = Limits {
            time: std::time::Duration::ZERO,
            ..Limits::default()
        };
        let hash = crate::provenance::sha256_file;
        let out = run_with(&conn2, &zips2, &fake2, hash, |_| Ok(()), none);
        assert!(
            out.iter().all(|(_, o)| matches!(o, Outcome::Retry(_))),
            "{out:?}"
        );
        assert_eq!(fake2.asked(), 0, "no request at all");
        assert!(!zips2.iter().any(|z| settled(&conn2, z)));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    #[test]
    fn one_run_looks_up_a_bounded_number_of_names_and_the_next_goes_on() {
        let (dir, conn) = setup("named_bound");
        let (zips, mut fake) = many(&dir, 3);
        fake.known.insert("FS25_File00".into());
        let two = Limits {
            max_named: 2,
            ..Limits::default()
        };
        let hash = crate::provenance::sha256_file;
        let out = run_with(&conn, &zips, &fake, hash, |_| Ok(()), two);
        assert_eq!(out.len(), 3);
        assert_eq!(fake.asked_known.get(), 1);
        let last = out.iter().find(|(p, _)| *p == zips[2].path).unwrap();
        assert!(matches!(last.1, Outcome::Retry(_)), "{out:?}");
        assert_eq!(
            out.iter().find(|(p, _)| *p == zips[0].path).unwrap().1,
            Outcome::InCatalog
        );
        // The next run takes the one left.
        let out = run_with(&conn, &zips, &fake, hash, |_| Ok(()), two);
        assert_eq!(
            out,
            [(zips[2].path.clone(), Outcome::Linked("cat-2".into()))]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_name_batch_that_fails_goes_behind_files_not_tried_yet() {
        let (dir, conn) = setup("named_fail");
        let (zips, fake) = many(&dir, 3);
        let two = Limits {
            max_named: 2,
            ..Limits::default()
        };
        let hash = crate::provenance::sha256_file;
        let down = Fake {
            fail_known: true,
            ..Fake::default()
        };
        let out = run_with(&conn, &zips, &down, hash, |_| Ok(()), two);
        assert!(
            out.iter().all(|(_, o)| matches!(o, Outcome::Retry(_))),
            "{out:?}"
        );
        // The next batch starts with the file the failed one didn't hold.
        let out = run_with(&conn, &zips, &fake, hash, |_| Ok(()), two);
        let got = out
            .iter()
            .find(|(p, _)| *p == zips[2].path)
            .map(|(_, o)| o.clone());
        assert_eq!(got, Some(Outcome::Linked("cat-2".into())));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_name_lookup_that_used_up_the_run_leaves_the_next_run_to_hash() {
        let (dir, conn) = setup("slow_names");
        let (zips, mut fake) = many(&dir, 1);
        fake.slow_known = std::time::Duration::from_millis(400);
        let short = Limits {
            time: std::time::Duration::from_millis(200),
            ..Limits::default()
        };
        let hash = crate::provenance::sha256_file;
        let out = run_with(&conn, &zips, &fake, hash, |_| Ok(()), short);
        assert!(matches!(out[0].1, Outcome::Retry(_)), "{out:?}");
        assert!(fake.asked_by_hash.borrow().is_empty());
        // The catalog said it has no entry of that name: the next run hashes without asking.
        fake.slow_known = std::time::Duration::ZERO;
        let out = run_with(&conn, &zips, &fake, hash, |_| Ok(()), short);
        assert_eq!(out[0].1, Outcome::Linked("cat-0".into()));
        assert_eq!(fake.asked_known.get(), 1, "asked by name once");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_library_larger_than_a_run_asks_each_name_once_and_gets_through() {
        let (dir, conn) = setup("big");
        let (zips, fake) = many(&dir, 5);
        let small = Limits {
            max_named: 2,
            max_hashed: 1,
            ..Limits::default()
        };
        let hash = crate::provenance::sha256_file;
        let mut linked = 0;
        for _ in 0..10 {
            let out = run_with(&conn, &zips, &fake, hash, |_| Ok(()), small);
            linked += out
                .iter()
                .filter(|(_, o)| matches!(o, Outcome::Linked(_)))
                .count();
        }
        assert_eq!(linked, 5);
        let mut names = fake.asked_names.borrow().clone();
        assert_eq!(names.len(), 5, "no name asked twice: {names:?}");
        names.dedup();
        assert_eq!(names.len(), 5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_the_catalog_said_about_a_name_is_asked_again_after_a_day() {
        let (dir, conn) = setup("unnamed_stale");
        let (zips, fake) = many(&dir, 1);
        let z = &zips[0];
        let check = |unnamed_ms| db::AliasCheck {
            size: z.size,
            mtime_ms: z.mtime_ms,
            sha256: None,
            unnamed_ms,
            decided: false,
            attempted_ms: 1,
        };
        db::note_alias_check(
            &conn,
            &path_key(z),
            &check(Some(now_ms() - UNNAMED_FRESH_MS - 1)),
        )
        .unwrap();
        run(&conn, &zips, &fake, crate::provenance::sha256_file, |_| {
            Ok(())
        });
        assert_eq!(fake.asked_known.get(), 1, "a day old: asked again");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_busy_library_is_waited_for_once_a_run() {
        let (dir, conn) = setup("busy_once");
        let (zips, fake) = many(&dir, 3);
        let waited = RefCell::new(Vec::new());
        let busy = |left: std::time::Duration| -> Result<(), String> {
            waited.borrow_mut().push(left);
            Err("Silo is busy".into())
        };
        let out = run(&conn, &zips, &fake, crate::provenance::sha256_file, busy);
        assert!(
            out.iter().all(|(_, o)| matches!(o, Outcome::Retry(_))),
            "{out:?}"
        );
        assert_eq!(
            waited.borrow().len(),
            1,
            "no second wait after the first refusal"
        );
        // It waited no longer than the run had left.
        assert!(waited.borrow()[0] <= Limits::default().time);
        assert!(!zips.iter().any(|z| settled(&conn, z)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn files_with_a_noted_hash_rotate_too() {
        let (dir, conn) = setup("rotate_cached");
        let (zips, mut fake) = many(&dir, 4);
        fake.fail_by_hash = true;
        // Every file already hashed, known lately not to be any entry's name, tried in order.
        for (i, z) in zips.iter().enumerate() {
            let check = db::AliasCheck {
                size: z.size,
                mtime_ms: z.mtime_ms,
                sha256: Some(crate::provenance::sha256_file(&z.path).unwrap()),
                unnamed_ms: Some(now_ms()),
                decided: false,
                attempted_ms: i as i64 + 1,
            };
            db::note_alias_check(&conn, &path_key(z), &check).unwrap();
        }
        let two = Limits {
            max_hashed: 2,
            ..Limits::default()
        };
        let no_hashing =
            |_: &Path| -> Result<String, String> { panic!("hashed a file with a noted hash") };
        let batch = |n: usize| -> HashSet<String> {
            fake.asked_by_hash.borrow()[n].iter().cloned().collect()
        };
        let shas: Vec<String> = zips
            .iter()
            .map(|z| crate::provenance::sha256_file(&z.path).unwrap())
            .collect();
        for _ in 0..2 {
            run_with(&conn, &zips, &fake, no_hashing, |_| Ok(()), two);
        }
        assert_eq!(fake.asked_known.get(), 0, "no name asked again");
        // The first batch is the two tried longest ago; the next, the other two.
        assert_eq!(batch(0), HashSet::from([shas[0].clone(), shas[1].clone()]));
        assert_eq!(batch(1), HashSet::from([shas[2].clone(), shas[3].clone()]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `Fake`, with another connection taking the database's write lock while the hash lookup
    /// is out (and keeping it).
    struct LocksDb<'a> {
        fake: &'a Fake,
        other: &'a Connection,
    }

    impl Catalog for LocksDb<'_> {
        fn known_tech_names(&self, names: &[String]) -> Result<HashSet<String>, String> {
            self.fake.known_tech_names(names)
        }
        fn by_hash(
            &self,
            hashes: &[String],
        ) -> Result<Option<HashMap<String, Vec<HashMatch>>>, String> {
            self.other.execute_batch("BEGIN IMMEDIATE").unwrap();
            self.fake.by_hash(hashes)
        }
    }

    #[test]
    fn a_database_busy_while_recording_stops_the_records_after_one_wait() {
        let (dir, conn) = setup("db_busy_records");
        let (zips, fake) = many(&dir, 3);
        let other = db::open(&dir.join("silo.db")).unwrap();
        let catalog = LocksDb {
            fake: &fake,
            other: &other,
        };
        // Room to reach the records on a slow runner (a 300 ms budget could run out while
        // hashing, before any record was tried); the short wait for the database is what
        // the busy record runs into.
        let short = Limits {
            time: std::time::Duration::from_secs(10),
            db_wait: std::time::Duration::from_millis(300),
            ..Limits::default()
        };
        let locks = Cell::new(0);
        let lock = |_| -> Result<(), String> {
            locks.set(locks.get() + 1);
            Ok(())
        };
        let started = std::time::Instant::now();
        let out = run_with(
            &conn,
            &zips,
            &catalog,
            crate::provenance::sha256_file,
            lock,
            short,
        );
        // One short wait, not the whole budget, nor a wait per record.
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert!(
            out.iter().all(|(_, o)| matches!(o, Outcome::Retry(_))),
            "{out:?}"
        );
        assert_eq!(locks.get(), 1, "one record tried, then none");
        other.execute_batch("ROLLBACK").unwrap();
        assert!(!zips.iter().any(|z| settled(&conn, z)));
        // Free again: all three are linked.
        let out = run(&conn, &zips, &fake, crate::provenance::sha256_file, |_| {
            Ok(())
        });
        let linked = out
            .iter()
            .filter(|(_, o)| matches!(o, Outcome::Linked(_)))
            .count();
        assert_eq!(linked, 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn after_the_database_failed_a_record_nothing_more_is_written() {
        let (dir, conn) = setup("db_down");
        let (zips, mut fake) = many(&dir, 2);
        // The second file's bytes are no entry's: a decision to note.
        fake.builds
            .remove(&crate::provenance::sha256_file(&zips[1].path).unwrap());
        let other = db::open(&dir.join("silo.db")).unwrap();
        let catalog = LocksDb {
            fake: &fake,
            other: &other,
        };
        // Time to spare, short database waits: only one of them may happen.
        let limits = Limits {
            time: std::time::Duration::from_secs(10),
            db_wait: std::time::Duration::from_millis(500),
            ..Limits::default()
        };
        // How long one such wait takes here (SQLite sleeps in whole seconds on some builds).
        let one_wait = {
            let probe = db::open(&dir.join("silo.db")).unwrap();
            let blocker = db::open(&dir.join("silo.db")).unwrap();
            blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
            probe.busy_timeout(limits.db_wait).unwrap();
            let t = std::time::Instant::now();
            assert!(probe
                .execute("DELETE FROM alias_check WHERE path = 'none'", [])
                .is_err());
            let took = t.elapsed();
            blocker.execute_batch("ROLLBACK").unwrap();
            took
        };
        let started = std::time::Instant::now();
        let out = run_with(
            &conn,
            &zips,
            &catalog,
            crate::provenance::sha256_file,
            |_| Ok(()),
            limits,
        );
        let took = started.elapsed();
        // One wait; a second would make it at least twice that.
        let most = one_wait + one_wait / 2 + std::time::Duration::from_millis(150);
        assert!(took < most, "{took:?} (one wait: {one_wait:?})");
        let of = |i: usize| {
            out.iter()
                .find(|(p, _)| *p == zips[i].path)
                .unwrap()
                .1
                .clone()
        };
        assert!(matches!(of(0), Outcome::Retry(_)), "{out:?}");
        other.execute_batch("ROLLBACK").unwrap();
        // The decision wasn't written either: the file is looked at again.
        assert!(!settled(&conn, &zips[1]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_mod_left_out_while_the_run_goes_is_not_sent() {
        let (dir, conn) = setup("left_out_late");
        let (zips, fake) = many(&dir, 2);
        let skip = |name: &str| {
            db::set_curation(
                &conn,
                &db::CurationRow {
                    tech_name: name.into(),
                    favorite: false,
                    hidden: false,
                    broken: false,
                    rating: 0,
                    note: None,
                    skip_updates: true,
                },
            )
            .unwrap()
        };
        // The user leaves the first mod out of update checks while the run hashes.
        let hash = |p: &Path| {
            skip("FS25_File00");
            crate::provenance::sha256_file(p)
        };
        let out = run(&conn, &zips, &fake, hash, |_| Ok(()));
        let sent: Vec<String> = fake
            .asked_by_hash
            .borrow()
            .iter()
            .flatten()
            .cloned()
            .collect();
        assert_eq!(
            sent,
            [crate::provenance::sha256_file(&zips[1].path).unwrap()]
        );
        assert!(!catalogalias::has_any(&conn, "FS25_File00").unwrap());
        assert!(out.iter().all(|(p, _)| *p != zips[0].path), "{out:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_name_answer_that_used_up_the_run_still_settles_the_mods_it_knows() {
        let (dir, conn) = setup("late_known");
        let (zips, mut fake) = many(&dir, 2);
        fake.known.insert("FS25_File00".into());
        fake.slow_known = std::time::Duration::from_millis(400);
        let short = Limits {
            time: std::time::Duration::from_millis(200),
            ..Limits::default()
        };
        let out = run_with(
            &conn,
            &zips,
            &fake,
            crate::provenance::sha256_file,
            |_| Ok(()),
            short,
        );
        let of = |i: usize| {
            out.iter()
                .find(|(p, _)| *p == zips[i].path)
                .unwrap()
                .1
                .clone()
        };
        assert_eq!(of(0), Outcome::InCatalog);
        assert!(matches!(of(1), Outcome::Retry(_)));
        assert!(settled(&conn, &zips[0]));
        assert!(
            fake.asked_by_hash.borrow().is_empty(),
            "no request past the time"
        );
        let noted = db::alias_check(&conn, &path_key(&zips[1]))
            .unwrap()
            .unwrap();
        assert!(
            noted.unnamed_ms.is_some(),
            "the next run hashes it without asking"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_link_seen_outside_the_library_lock_is_checked_under_it() {
        let (dir, conn) = setup("rollback");
        let file = write_zip(&dir, "FS25_FieldToDoList.zip", b"build one");
        let zips = [scanned(&file)];
        let fake = Fake::with(b"build one", vec![todo_build()]);
        // An install elsewhere recorded these bytes before placing its file...
        let theirs = Link {
            catalog_id: "cat-other".into(),
            catalog_tech_name: None,
            download_url: None,
        };
        let undo =
            catalogalias::record(&conn, "FS25_FieldToDoList", &sha(b"build one"), 9, &theirs)
                .unwrap();
        // ...and while it holds the lock, nothing is decided.
        let held = |_| -> Result<(), String> { Err("Silo is busy".into()) };
        let out = run(&conn, &zips, &fake, crate::provenance::sha256_file, held);
        assert!(matches!(out[0].1, Outcome::Retry(_)), "{out:?}");
        assert!(!settled(&conn, &zips[0]));
        // Its placing failed and it took the record back before letting go of the lock.
        let undo = RefCell::new(Some(undo));
        let after_rollback = |_| -> Result<(), String> {
            if let Some(u) = undo.borrow_mut().take() {
                u.apply(&conn).unwrap();
            }
            Ok(())
        };
        let out = run(
            &conn,
            &zips,
            &fake,
            crate::provenance::sha256_file,
            after_rollback,
        );
        assert_eq!(out[0].1, Outcome::Linked("cat-todo".into()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_busy_database_stops_the_run_after_one_wait() {
        let (dir, conn) = setup("db_busy");
        let (zips, mut fake) = many(&dir, 3);
        fake.known.insert("FS25_File00".into());
        // Another connection holds the database's write lock. A write waits at most the run's
        // time (here 300 ms), once.
        let other = db::open(&dir.join("silo.db")).unwrap();
        other.execute_batch("BEGIN IMMEDIATE").unwrap();
        let short = Limits {
            time: std::time::Duration::from_millis(300),
            ..Limits::default()
        };
        let started = std::time::Instant::now();
        let out = run_with(
            &conn,
            &zips,
            &fake,
            crate::provenance::sha256_file,
            |_| Ok(()),
            short,
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        assert!(
            out.iter().all(|(_, o)| matches!(o, Outcome::Retry(_))),
            "{out:?}"
        );
        assert!(
            fake.asked_by_hash.borrow().is_empty(),
            "stopped before the hash lookup"
        );
        other.execute_batch("ROLLBACK").unwrap();
        // Free again: the known one is settled and the others linked.
        let out = run(&conn, &zips, &fake, crate::provenance::sha256_file, |_| {
            Ok(())
        });
        assert_eq!(
            out.iter().filter(|(_, o)| *o == Outcome::InCatalog).count(),
            1
        );
        assert_eq!(
            out.iter()
                .filter(|(_, o)| matches!(o, Outcome::Linked(_)))
                .count(),
            2
        );
        assert!(settled(&conn, &zips[0]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tries_are_numbered_in_the_order_made_whatever_the_clock_says() {
        let (dir, conn) = setup("try_order");
        let (zips, fake) = many(&dir, 3);
        // A try noted far ahead of the clock (a clock set back since, say).
        let ahead = now_ms() + 1_000_000;
        let seen = scanned(&write_zip(&dir, "FS25_Elsewhere.zip", b"x"));
        let check = db::AliasCheck {
            size: seen.size,
            mtime_ms: seen.mtime_ms,
            sha256: None,
            unnamed_ms: None,
            decided: false,
            attempted_ms: ahead,
        };
        db::note_alias_check(&conn, &path_key(&seen), &check).unwrap();
        let one = Limits {
            max_hashed: 1,
            ..Limits::default()
        };
        let hash = crate::provenance::sha256_file;
        let down = Fake {
            fail_by_hash: true,
            ..Fake::default()
        };
        run_with(&conn, &zips, &down, hash, |_| Ok(()), one);
        let tried = |i: usize| {
            db::alias_check(&conn, &path_key(&zips[i]))
                .unwrap()
                .unwrap()
                .attempted_ms
        };
        // The name batch after the earlier try, the hashed file after the name batch.
        assert!(
            tried(1) > ahead && tried(2) == tried(1),
            "{} {}",
            tried(1),
            tried(2)
        );
        assert!(tried(0) > tried(1));
        // The next run goes after all of them, and takes a file the last one only asked about.
        run_with(&conn, &zips, &fake, hash, |_| Ok(()), one);
        assert!(
            settled(&conn, &zips[1]),
            "the file left by the last run went first"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn files_that_keep_failing_dont_hold_up_the_rest() {
        let (dir, conn) = setup("starve");
        let (mut zips, fake) = many(&dir, MAX_HASHED + 1);
        // The first ones can't be read, or the scan couldn't read their size or time.
        for z in zips.iter_mut().take(MAX_HASHED).skip(MAX_HASHED / 2) {
            z.size = 0;
            z.mtime_ms = 0;
        }
        let last = zips[MAX_HASHED].path.clone();
        let locked = |p: &Path| -> Result<String, String> {
            if p == last.as_path() {
                crate::provenance::sha256_file(p)
            } else {
                Err("in use".into())
            }
        };
        let out = run(&conn, &zips, &fake, locked, |_| Ok(()));
        assert!(
            out.iter().all(|(_, o)| matches!(o, Outcome::Retry(_))),
            "{out:?}"
        );
        // The next run tries the file it didn't get to first.
        let out = run(&conn, &zips, &fake, locked, |_| Ok(()));
        let got = out.iter().find(|(p, _)| *p == last).map(|(_, o)| o.clone());
        assert_eq!(got, Some(Outcome::Linked(format!("cat-{MAX_HASHED}"))));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

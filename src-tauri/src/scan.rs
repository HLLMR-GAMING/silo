//! Library scan: walk the mod root(s), read each mod's `modDesc.xml`, parse it,
//! and return structured entries. All heavy work runs here on a rayon pool — the
//! UI thread never touches a zip. We read ONLY the `modDesc.xml` entry from each
//! archive (the zip central directory + one small entry), so even 100 MB+ map
//! mods scan cheaply. Content hashing and icon decode are deliberately deferred.

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::db::CacheEntry;
use crate::moddesc;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModEntry {
    /// Tech name = the zip/dir basename (mod identity the game & deps use).
    pub tech_name: String,
    pub path: String,
    /// "zip" | "dir"
    pub kind: String,
    pub size: u64,
    pub mtime_ms: u64,

    pub title: Option<String>,
    pub author: Option<String>,
    pub version: Option<String>,
    pub desc_version: Option<i64>,
    /// Path to the mod's icon inside the archive/dir (for lazy thumbnail loading).
    pub icon_filename: Option<String>,

    pub is_map: bool,
    pub map_title: Option<String>,
    /// Best-effort category folder bucket (see `category` module).
    pub category: String,
    /// Optional second-level bucket (e.g. Tractors › Medium).
    pub subcategory: Option<String>,

    pub dependencies: Vec<String>,
    pub script_count: usize,
    pub registration_count: usize,
    pub unique_type: Option<String>,
    pub store_item_count: u32,
    pub mp_supported: bool,

    /// True when the mod lives in `mods/archive/<Category>/` (Silo-managed).
    pub organized: bool,
    /// True when the mod is present in the flat root (i.e. the game loads it):
    /// vanilla mods, and organized mods currently projected as links.
    pub active: bool,

    /// The game IGNORES mods whose name starts with a digit — a silent footgun.
    pub ignored_digit_prefix: bool,
    /// Populated when the mod couldn't be read/parsed (still listed, flagged).
    pub error: Option<String>,

    /// Whether the mod exposes user-configurable settings, detected by looking inside the
    /// archive/dir (not just the runtime `modSettings/` folder). See `detect_settings`.
    /// `serde(default)` so scan-cache rows written before this field deserialize cleanly.
    #[serde(default)]
    pub has_settings: bool,
    /// How the settings were detected — governs whether Silo can edit them right now.
    #[serde(default)]
    pub settings_source: SettingsSource,
    /// The [`PARSE_REV`] that produced this entry. A cached row from an older parser is
    /// parsed again even when the file is unchanged; rows from before this field read as 0.
    #[serde(default)]
    pub parse_rev: u32,
}

/// Bumped when a parser fix changes what an unchanged mod file reads as, so cached rows
/// from the old parser are refreshed once instead of staying wrong until the file
/// changes. 1: element text is no longer cut at an `&amp;` (#266).
pub const PARSE_REV: u32 = 1;

/// How a mod's settings were detected, which decides whether Silo can edit them now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum SettingsSource {
    /// No settings detected.
    #[default]
    None,
    /// Ships a settings XML in the archive (Tier A) — defaults are readable statically.
    Shipped,
    /// Lua persists settings under `modSettings/` at runtime (Tier B) — values appear
    /// only after the game has run the mod once.
    Runtime,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanResult {
    pub mods: Vec<ModEntry>,
    pub roots: Vec<String>,
    pub took_ms: u128,
    pub total: usize,
    /// Interrupted mod updates finished or rolled back before the scan, for the user.
    pub notices: Vec<String>,
    /// What that recovery couldn't fix, for the user.
    pub problems: Vec<String>,
}

/// Result of a cached scan: the full list plus which paths were freshly parsed
/// (cache misses), with the key to cache each under, so the caller can persist just those.
pub struct ScanOutput {
    pub result: ScanResult,
    pub fresh_paths: HashMap<String, CacheKey>,
    /// The archived copy behind each mods-folder entry that stands in for it (see
    /// [`projection_of`]), keyed by the entry's path. Its row in `result` describes the
    /// file the game loads; the library's manifest still names the archived copy.
    pub stand_ins: HashMap<String, ArchivedCopy>,
}

/// An archived mod hidden behind a different file at its name in the mods folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivedCopy {
    pub path: String,
    pub kind: String,
}

struct Candidate {
    path: PathBuf,
    tech_name: String,
    kind: &'static str,
    /// Which scanned layout (mods folder + its archives) the entry was found in.
    layout: usize,
}

/// Build a Candidate from a directory entry (a `.zip` file or an unpacked mod dir),
/// or None if it isn't a mod.
fn candidate_from(path: PathBuf, name: &str, ft: std::fs::FileType) -> Option<Candidate> {
    // A symlink's own `file_type` is neither dir nor file, so resolve the target — otherwise
    // symlinked dev mods (a common workflow, e.g. `mods/FS25_Foo -> <dev repo>`) and Silo's
    // own dir-mod junction projections would be silently skipped. `fs::metadata` follows links.
    let (is_dir, is_file) = if ft.is_symlink() {
        match fs::metadata(&path) {
            Ok(m) => (m.is_dir(), m.is_file()),
            Err(_) => (false, false), // dangling link — ignore
        }
    } else {
        (ft.is_dir(), ft.is_file())
    };

    if is_dir {
        if path.join("modDesc.xml").is_file() {
            return Some(Candidate {
                tech_name: name.to_string(),
                path,
                kind: "dir",
                layout: 0,
            });
        }
    } else if is_file && name.to_lowercase().ends_with(".zip") {
        let tech_name = name[..name.len() - 4].to_string();
        return Some(Candidate {
            tech_name,
            path,
            kind: "zip",
            layout: 0,
        });
    }
    None
}

/// True for the scratch entries Silo leaves mid-operation (an archive move's in-flight copy or
/// the old copy it is deleting, relink, organize's `.{name}.silo-tmp`, or a mod update's
/// download and set-aside old version). They can hold a full copy of a mod and must never be
/// scanned as one.
pub(crate) fn is_silo_temp(name: &str) -> bool {
    let l = name.to_lowercase();
    l.ends_with(".silo-move-tmp")
        || l.ends_with(".silo-move-old")
        || l.ends_with(".silo-relink-tmp")
        || l.ends_with(crate::update::PART_SUFFIX)
        || l.ends_with(crate::update::OLD_SUFFIX)
        || (l.starts_with('.') && l.ends_with(".silo-tmp"))
        || l.starts_with(crate::organize::HARDLINK_PROBE)
        || l.starts_with(crate::organize::SYMLINK_PROBE)
}

/// Mods in the flat root (what the game reads) — excluding Silo's `archive/` and
/// any `backups/` folder.
fn collect_root_candidates(root: &Path) -> Vec<Candidate> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(root) else {
        return out;
    };
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let lname = name.to_lowercase();
        if lname == "archive" || lname == "backups" || is_silo_temp(&name) {
            continue;
        }
        if let Ok(ft) = entry.file_type() {
            if let Some(c) = candidate_from(entry.path(), &name, ft) {
                out.push(c);
            }
        }
    }
    out
}

/// The file names of the mods in a mods folder itself, as a scan lists them: zips and unpacked
/// folder mods, without `archive/`, `backups/` and Silo's scratch entries.
pub fn root_mod_names(root: &Path) -> Vec<String> {
    collect_root_candidates(root)
        .into_iter()
        .filter_map(|c| c.path.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect()
}

/// Mods parked in an archive folder's `<Category>/` subfolders (Silo-managed, one level of folders).
fn collect_archive_candidates(archive: &Path) -> Vec<Candidate> {
    let mut out = Vec::new();
    let Ok(cats) = fs::read_dir(archive) else {
        return out;
    };
    for cat in cats.flatten() {
        if !cat.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let Ok(mods) = fs::read_dir(cat.path()) else {
            continue;
        };
        for entry in mods.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if is_silo_temp(&name) {
                continue;
            }
            if let Ok(ft) = entry.file_type() {
                if let Some(c) = candidate_from(entry.path(), &name, ft) {
                    out.push(c);
                }
            }
        }
    }
    out
}

/// Read a mod's `modDesc.xml` text, whether it's a `.zip` or an unpacked dir.
pub fn read_moddesc_xml(mod_path: &Path, kind: &str) -> Result<String, String> {
    match kind {
        "zip" => read_moddesc_from_zip(mod_path),
        _ => read_moddesc_from_dir(mod_path),
    }
}

/// Largest we'll read for a single member. modDesc.xml is normally a few KB; a script or
/// icon a few MB. These caps stop a hostile/broken archive from decompressing a tiny
/// entry into gigabytes of memory (a zip bomb) — the game trusts the mod folder, but Silo
/// parses arbitrary downloads, so it must not.
pub(crate) const MODDESC_LIMIT: u64 = 16 * 1024 * 1024;

/// Read a (possibly-decompressing) reader with a hard byte cap. Errors — never
/// truncates — if the content exceeds `limit`, so a zip bomb can't exhaust memory.
pub(crate) fn read_capped<R: Read>(r: R, limit: u64, what: &str) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    // Read one past the limit so we can tell "exactly at the cap" from "over it".
    r.take(limit + 1)
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    if buf.len() as u64 > limit {
        return Err(format!(
            "{what} is larger than the {limit}-byte safety limit"
        ));
    }
    Ok(buf)
}

fn read_moddesc_from_zip(path: &Path) -> Result<String, String> {
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    // modDesc.xml lives at the archive root per FS conventions.
    let f = archive
        .by_name("modDesc.xml")
        .map_err(|_| "modDesc.xml not found in archive".to_string())?;
    let bytes = read_capped(f, MODDESC_LIMIT, "modDesc.xml")?;
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

fn read_moddesc_from_dir(path: &Path) -> Result<String, String> {
    let p = path.join("modDesc.xml");
    if fs::metadata(&p).map(|m| m.len()).unwrap_or(0) > MODDESC_LIMIT {
        return Err("modDesc.xml is larger than the safety limit".to_string());
    }
    fs::read_to_string(p).map_err(|e| e.to_string())
}

fn build_entry(c: &Candidate) -> ModEntry {
    let meta = fs::metadata(&c.path).ok();
    let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    let mtime_ms = meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let ignored_digit_prefix = c
        .tech_name
        .chars()
        .next()
        .map(|ch| ch.is_ascii_digit())
        .unwrap_or(false);

    let mut entry = ModEntry {
        tech_name: c.tech_name.clone(),
        path: c.path.to_string_lossy().into_owned(),
        kind: c.kind.to_string(),
        size,
        mtime_ms,
        title: None,
        author: None,
        version: None,
        desc_version: None,
        icon_filename: None,
        is_map: false,
        map_title: None,
        category: "Other".to_string(),
        subcategory: None,
        dependencies: Vec::new(),
        script_count: 0,
        registration_count: 0,
        unique_type: None,
        store_item_count: 0,
        mp_supported: false,
        organized: false,
        active: false,
        ignored_digit_prefix,
        error: None,
        has_settings: false,
        settings_source: SettingsSource::None,
        parse_rev: PARSE_REV,
    };

    let xml = match c.kind {
        "zip" => read_moddesc_from_zip(&c.path),
        _ => read_moddesc_from_dir(&c.path),
    };

    match xml {
        Ok(xml) => {
            let md = moddesc::parse(&xml);
            // Compute category while `md` is still whole (before we move fields out).
            // Read the authoritative FS store category when the mod has store items.
            let store_cat = if md.store_item_files.is_empty() {
                None
            } else {
                crate::store::first_store_category(&c.path, c.kind, &md.store_item_files)
            };
            let title = md.title.clone().or_else(|| Some(c.tech_name.clone()));
            let (category, subcategory) = crate::category::categorize(
                &md,
                store_cat.as_deref(),
                &c.tech_name,
                title.as_deref(),
            );
            entry.category = category;
            entry.subcategory = subcategory;
            entry.title = title;
            entry.author = md.author;
            entry.version = md.version;
            entry.desc_version = md.desc_version;
            entry.icon_filename = md.icon_filename;
            entry.is_map = md.is_map;
            entry.map_title = md.map_title;
            entry.dependencies = md.dependencies;
            entry.script_count = md.scripts.len();
            entry.registration_count = md.registrations.len();
            entry.unique_type = md.unique_type;
            entry.store_item_count = md.store_item_count;
            entry.mp_supported = md.mp_supported;
        }
        Err(e) => {
            entry.title = Some(c.tech_name.clone());
            entry.error = Some(e);
        }
    }

    let (has_settings, settings_source) = detect_settings(&c.path, c.kind);
    entry.has_settings = has_settings;
    entry.settings_source = settings_source;

    entry
}

/// Budget for the Tier-B Lua grep so a huge script mod can't stall the scan. The whole
/// scan of one mod stays cheap: Tier A is a name-list check (no decompression), Tier B
/// decompresses Lua only until a hit or this budget is spent.
const LUA_SCAN_BUDGET: u64 = 8 * 1024 * 1024;
const LUA_MEMBER_CAP: u64 = 4 * 1024 * 1024;
/// Most Lua files the Tier-B grep opens in one mod. The biggest real script mods ship a few
/// hundred (Courseplay about 270); past this the rest go unread, as after the byte budget.
const LUA_MEMBER_LIMIT: usize = 1000;

/// Read at most `cap` bytes of one Lua file for the Tier-B grep. Returns the bytes it consumed,
/// which the caller charges to the budget whether or not the file fit (an oversized or broken
/// file still cost the decompression), and the contents when the whole file fit.
fn read_lua<R: Read>(r: R, cap: u64) -> (u64, Option<Vec<u8>>) {
    let mut buf = Vec::new();
    // On an error `read_to_end` keeps what it read, so `buf.len()` is still what it consumed.
    let ok = r.take(cap + 1).read_to_end(&mut buf).is_ok();
    let used = buf.len() as u64;
    (used, (ok && used <= cap).then_some(buf))
}

/// Detect whether a mod exposes user-configurable settings by looking inside it.
///
/// Two tiers (see the settings-detection research): **Shipped** (Tier A) — the archive
/// carries a `*settings*.xml` whose defaults we can read now — beats **Runtime** (Tier B) —
/// Lua that persists to a `modSettings/` path, whose values only exist after a run.
/// `modDesc.xml` declares nothing useful here, so we never consult it. Best-effort: any
/// read error just yields "no settings" rather than failing the scan.
pub fn detect_settings(path: &Path, kind: &str) -> (bool, SettingsSource) {
    match kind {
        "zip" => detect_settings_zip(path),
        _ => detect_settings_dir(path),
    }
}

/// Tier A: an XML whose basename says "settings" — but not a translation/l10n file, which
/// often ship a `settings` string in a different sense.
fn is_settings_xml(name_lower: &str) -> bool {
    if !name_lower.ends_with(".xml") {
        return false;
    }
    if name_lower.contains("translation") || name_lower.contains("l10n") {
        return false;
    }
    let base = name_lower.rsplit(['/', '\\']).next().unwrap_or(name_lower);
    base.contains("settings")
}

/// Tier B: Lua that references a `modSettings/` path (also the seen-in-the-wild
/// `modsSettings` typo, and the `g_*ModSettingsDirectory` globals — all lowercase to
/// `modsettings`). This is the strongest static predictor of runtime-persisted settings.
fn lua_has_settings_signature(bytes: &[u8]) -> bool {
    let lower = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    lower.contains("modsettings") || lower.contains("modssettings")
}

fn detect_settings_zip(path: &Path) -> (bool, SettingsSource) {
    let Ok(file) = fs::File::open(path) else {
        return (false, SettingsSource::None);
    };
    let Ok(mut archive) = zip::ZipArchive::new(file) else {
        return (false, SettingsSource::None);
    };
    // Cheap first pass over just the name list (no decompression): Tier A wins outright,
    // and we note the first Lua members for the bounded Tier-B grep.
    let mut lua: Vec<String> = Vec::new();
    for name in archive.file_names().flatten() {
        let lower = name.to_ascii_lowercase();
        if is_settings_xml(&lower) {
            return (true, SettingsSource::Shipped);
        }
        if lower.ends_with(".lua") && lua.len() < LUA_MEMBER_LIMIT {
            lua.push(name.into_owned());
        }
    }
    let mut budget = LUA_SCAN_BUDGET;
    for name in lua {
        if budget == 0 {
            break;
        }
        let Ok(f) = archive.by_name(&name) else {
            continue;
        };
        let (used, bytes) = read_lua(f, budget.min(LUA_MEMBER_CAP));
        budget = budget.saturating_sub(used);
        let Some(bytes) = bytes else {
            continue; // over the per-member cap — skip it, keep scanning the rest
        };
        if lua_has_settings_signature(&bytes) {
            return (true, SettingsSource::Runtime);
        }
    }
    (false, SettingsSource::None)
}

fn detect_settings_dir(path: &Path) -> (bool, SettingsSource) {
    let mut lua: Vec<PathBuf> = Vec::new();
    for entry in walkdir::WalkDir::new(path)
        .max_depth(8)
        .into_iter()
        .flatten()
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let lower = entry.path().to_string_lossy().to_ascii_lowercase();
        if is_settings_xml(&lower) {
            return (true, SettingsSource::Shipped);
        }
        if lower.ends_with(".lua") && lua.len() < LUA_MEMBER_LIMIT {
            lua.push(entry.path().to_path_buf());
        }
    }
    let mut budget = LUA_SCAN_BUDGET;
    for p in lua {
        if budget == 0 {
            break;
        }
        let Ok(file) = fs::File::open(&p) else {
            continue;
        };
        let (used, bytes) = read_lua(file, budget.min(LUA_MEMBER_CAP));
        budget = budget.saturating_sub(used);
        let Some(bytes) = bytes else {
            continue;
        };
        if lua_has_settings_signature(&bytes) {
            return (true, SettingsSource::Runtime);
        }
    }
    (false, SettingsSource::None)
}

/// What a scan-cache row is keyed on besides its path: when it changes, the mod is parsed again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheKey {
    pub mtime_ms: u64,
    pub size: u64,
}

/// A path's `(mtime_ms, size)`, following links; zeros when it can't be read.
fn stamp(path: &Path) -> (u64, u64) {
    let Ok(meta) = fs::metadata(path) else {
        return (0, 0);
    };
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    (mtime_ms, meta.len())
}

/// The cache key for a candidate. A zip's is its own mtime and size. A folder's own mtime and
/// size don't change when a file inside it is rewritten in place (its size is 0 on Windows), so
/// a folder mod's key also folds in `modDesc.xml`'s, which is where everything the scan reads
/// lives (#353). The fold (`a ^ b.rotate_left(32)`) changes whenever either side alone does.
fn cache_key(c: &Candidate) -> CacheKey {
    let (mut mtime_ms, mut size) = stamp(&c.path);
    if c.kind == "dir" {
        let (desc_mtime, desc_size) = stamp(&c.path.join("modDesc.xml"));
        mtime_ms ^= desc_mtime.rotate_left(32);
        size ^= desc_size.rotate_left(32);
    }
    CacheKey { mtime_ms, size }
}

/// Resolve one candidate: reuse the cached entry when its [`cache_key`] is unchanged (no
/// archive open), otherwise parse fresh. Returns the entry and, when it was freshly parsed, the
/// key to cache it under.
fn resolve_entry(
    c: &Candidate,
    cache: &HashMap<String, CacheEntry>,
) -> (ModEntry, Option<CacheKey>) {
    let key = cache_key(c);
    if let Some(ce) = cache.get(c.path.to_string_lossy().as_ref()) {
        if ce.mtime_ms == key.mtime_ms && ce.size == key.size {
            // A schema change that fails deserialization simply falls through to a
            // fresh parse — we never trust a partial decode.
            // A row from an older parser is re-read too (see `PARSE_REV`).
            if let Ok(entry) = serde_json::from_str::<ModEntry>(&ce.json) {
                if entry.parse_rev >= PARSE_REV {
                    return (entry, None);
                }
            }
        }
    }
    (build_entry(c), Some(key))
}

/// What Silo recorded for the zips it projected into one mods folder: file name to the
/// `(size, mtime_ms)` the entry had when Silo wrote or last confirmed it (`db::load_projections`).
pub type ProjectionRecords = HashMap<String, (u64, i64)>;

/// How sure the scan is that a mods-folder entry is Silo's projection of an archived copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Ownership {
    /// Something else at that name: a different kind, link target, size or recorded stamp.
    Foreign,
    /// An unrecorded plain file of the same size, trusted without a hash (see below).
    Trusted,
    /// Shown by structure or by Silo's own record.
    Proven,
}

/// Cheap check that mods-folder entry `link` is Silo's projection of archived `src`: the
/// structural and recorded-stamp tests of `organize::is_silo_projection` /
/// `organize::matches_provenance`, without the content hash, which a scan never pays for. A
/// link must resolve to `src` and a real folder must carry Silo's marker. A plain file is ours
/// when it is `src` itself (a hardlink); otherwise it must be `src`'s size and, when Silo
/// recorded the entry (`record`), still carry that size and mtime. A file swapped in by the user
/// fails one of those; only an unrecorded same-size file passes, on trust, and the foreign-file
/// check after each scan hashes it (and records it when it is ours).
fn projection_of(link: &Candidate, src: &Candidate, record: Option<(u64, i64)>) -> Ownership {
    let proven = |yes: bool| {
        if yes {
            Ownership::Proven
        } else {
            Ownership::Foreign
        }
    };
    if link.kind != src.kind {
        return Ownership::Foreign;
    }
    // Gone since it was listed: nothing to show instead, so keep the archived entry.
    let Ok(meta) = fs::symlink_metadata(&link.path) else {
        return Ownership::Trusted;
    };
    if meta.file_type().is_symlink() {
        // Silo's symlink or junction resolves to the archived copy.
        return proven(matches!(
            (fs::canonicalize(&link.path), fs::canonicalize(&src.path)),
            (Ok(a), Ok(b)) if a == b
        ));
    }
    if meta.is_dir() {
        return proven(link.path.join(crate::organize::SILO_MARKER).is_file());
    }
    if let (Ok(a), Ok(b)) = (
        same_file::Handle::from_path(&link.path),
        same_file::Handle::from_path(&src.path),
    ) {
        if a == b {
            return Ownership::Proven;
        }
    }
    let same_size = fs::metadata(&src.path)
        .map(|s| s.len() == meta.len())
        .unwrap_or(true);
    match record {
        _ if !same_size => Ownership::Foreign,
        Some(r) => proven(r == file_stamp(&meta)),
        None => Ownership::Trusted,
    }
}

/// A plain file's `(size, mtime_ms)` the way `organize::file_stats` records it.
fn file_stamp(meta: &fs::Metadata) -> (u64, i64) {
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    (meta.len(), mtime)
}

/// Core scan over mods folders with explicit archive folders. Archives are read in order and a
/// techName already seen in an earlier archive is skipped (during a paused archive move the new
/// archive is listed first). No Tauri dependency — testable / CLI-reusable.
pub fn scan_layouts<F>(
    layouts: Vec<(PathBuf, Vec<PathBuf>)>,
    cache: &HashMap<String, CacheEntry>,
    progress: F,
) -> ScanOutput
where
    F: Fn(usize, usize) + Sync + Send,
{
    scan_layouts_with(layouts, &[], cache, progress)
}

/// [`scan_layouts`] with each layout's [`ProjectionRecords`] (by layout index; a layout without
/// them trusts any same-size file at an archived mod's name, see [`projection_of`]).
pub fn scan_layouts_with<F>(
    layouts: Vec<(PathBuf, Vec<PathBuf>)>,
    records: &[ProjectionRecords],
    cache: &HashMap<String, CacheEntry>,
    progress: F,
) -> ScanOutput
where
    F: Fn(usize, usize) + Sync + Send,
{
    let started = std::time::Instant::now();
    let mut root_cands: Vec<Candidate> = Vec::new();
    let mut archive_cands: Vec<Candidate> = Vec::new();
    // Later archives' copies of names already seen: not listed, but a mods-folder link may
    // still point at one (an archive move between placing a mod's new copy and re-pointing it).
    let mut shadowed: Vec<Candidate> = Vec::new();
    for (layout, (root, archives)) in layouts.iter().enumerate() {
        let tag = |mut c: Candidate| {
            c.layout = layout;
            c
        };
        // Dedupe is per layout: two mods folders may legitimately each hold the same techName.
        let mut seen: HashSet<String> = HashSet::new();
        root_cands.extend(collect_root_candidates(root).into_iter().map(tag));
        for a in archives {
            // Skip only names seen in an EARLIER archive; a single archive's own contents are
            // kept as-is so a one-archive scan behaves exactly as before.
            let cands = collect_archive_candidates(a);
            let names: Vec<String> = cands.iter().map(|c| c.tech_name.clone()).collect();
            let (later, first): (Vec<_>, Vec<_>) = cands
                .into_iter()
                .map(tag)
                .partition(|c| seen.contains(&c.tech_name));
            archive_cands.extend(first);
            shadowed.extend(later);
            seen.extend(names);
        }
    }
    let roots: Vec<PathBuf> = layouts.into_iter().map(|(r, _)| r).collect();
    let jobs = plan_jobs(&root_cands, &archive_cands, &shadowed, records);
    finish_scan(roots, jobs, cache, progress, started)
}

/// Scan with each root's default archive (`<root>/archive`).
pub fn scan_cached<F>(
    roots: Vec<PathBuf>,
    cache: &HashMap<String, CacheEntry>,
    progress: F,
) -> ScanOutput
where
    F: Fn(usize, usize) + Sync + Send,
{
    let layouts = roots
        .into_iter()
        .map(|r| {
            let a = r.join("archive");
            (r, vec![a])
        })
        .collect();
    scan_layouts(layouts, cache, progress)
}

/// Resolve each candidate from `cache` when possible and parse the rest in parallel.
/// Reports progress via a callback (invoked from many rayon threads, hence Sync).
fn finish_scan<F>(
    roots: Vec<PathBuf>,
    jobs: Vec<Job>,
    cache: &HashMap<String, CacheEntry>,
    progress: F,
    started: std::time::Instant,
) -> ScanOutput
where
    F: Fn(usize, usize) + Sync + Send,
{
    let total = jobs.len();
    progress(0, total);

    let done = AtomicUsize::new(0);
    let step = (total / 100).max(10);

    let pairs: Vec<(ModEntry, Option<CacheKey>)> = jobs
        .par_iter()
        .map(|job| {
            let (mut entry, fresh) = resolve_entry(job.read, cache);
            entry.organized = job.organized;
            entry.active = job.active;
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            if n % step == 0 || n == total {
                progress(n, total);
            }
            (entry, fresh)
        })
        .collect();

    let stand_ins = jobs
        .iter()
        .filter_map(|job| {
            let behind = job.behind?;
            Some((
                job.read.path.to_string_lossy().into_owned(),
                ArchivedCopy {
                    path: behind.path.to_string_lossy().into_owned(),
                    kind: behind.kind.to_string(),
                },
            ))
        })
        .collect();

    let mut fresh_paths = HashMap::new();
    let mut mods = Vec::with_capacity(pairs.len());
    for (entry, fresh) in pairs {
        if let Some(key) = fresh {
            fresh_paths.insert(entry.path.clone(), key);
        }
        mods.push(entry);
    }

    mods.sort_by(|a, b| {
        let ta = a.title.as_deref().unwrap_or(&a.tech_name).to_lowercase();
        let tb = b.title.as_deref().unwrap_or(&b.tech_name).to_lowercase();
        ta.cmp(&tb)
    });

    progress(total, total);

    ScanOutput {
        result: ScanResult {
            mods,
            roots: roots
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
            took_ms: started.elapsed().as_millis(),
            total,
            notices: Vec::new(),
            problems: Vec::new(),
        },
        fresh_paths,
        stand_ins,
    }
}

/// One library row to produce: the entry to read and how it sits in the library.
struct Job<'a> {
    /// The file or folder whose `modDesc.xml` describes the row.
    read: &'a Candidate,
    organized: bool,
    active: bool,
    /// For a mods-folder entry standing in for an archived mod, that archived copy.
    behind: Option<&'a Candidate>,
}

/// Where a mod sits: its layout and its exact file name (`FS25_A.zip`, or `FS25_A` for a
/// folder). A mods-folder entry is Silo's projection of an archived mod only at the archived
/// copy's own file name, which is how `organize` links, parks and checks them.
type Slot<'a> = (usize, &'a std::ffi::OsStr);

fn slot(c: &Candidate) -> Slot<'_> {
    (c.layout, c.path.file_name().unwrap_or_default())
}

/// Decide the rows of one scan. Every archived mod gets a row, and so does every mods-folder
/// entry that isn't at an archived mod's file name (vanilla, or the other kind of the same
/// techName). An entry at an archived mod's file name marks that mod active. When it is Silo's
/// projection (cheaply checked by [`projection_of`]) of any archived copy at that name,
/// the archived row stands, parsed from the archive as before; when it is something else (the
/// user swapped in another build or their own folder), the first archived copy's row describes
/// the file the game actually loads (#353), and the foreign-file banner offers adopt / restore.
fn plan_jobs<'a>(
    root_cands: &'a [Candidate],
    archive_cands: &'a [Candidate],
    shadowed: &[Candidate],
    records: &[ProjectionRecords],
) -> Vec<Job<'a>> {
    // Every archived copy per slot: a second category of one archive can hold the same name, and
    // a later archive's copy (`shadowed`) can still be a link's target mid archive move.
    let mut copies: HashMap<Slot, Vec<&Candidate>> = HashMap::new();
    for c in archive_cands.iter().chain(shadowed) {
        copies.entry(slot(c)).or_default().push(c);
    }

    // The archived mods that have a row, by slot, and the zips among them by techName in archive
    // order (see `archived_slots`).
    let mut visible: HashMap<Slot, &str> = HashMap::new();
    for c in archive_cands {
        visible.entry(slot(c)).or_insert(c.tech_name.as_str());
    }
    let mut zips: HashMap<(usize, &str), Vec<Slot>> = HashMap::new();
    for c in archive_cands.iter().filter(|c| c.kind == "zip") {
        let spellings = zips.entry((c.layout, c.tech_name.as_str())).or_default();
        if !spellings.contains(&slot(c)) {
            spellings.push(slot(c));
        }
    }
    let listed: HashSet<Slot> = root_cands.iter().map(slot).collect();

    // Mods-folder entries at archived mods' names: checked in parallel (a few stats each). The
    // entry sits at the slot it is most surely Silo's projection of (the first on a tie), so a
    // proven link to one copy beats a size match with another.
    let verdicts: Vec<Option<(Slot, bool)>> = root_cands
        .par_iter()
        .map(|r| {
            let slots = archived_slots(r, &visible, &zips, &listed);
            let ownership = |at: &Slot| {
                let record = records
                    .get(at.0)
                    .and_then(|m| m.get(at.1.to_string_lossy().as_ref()))
                    .copied();
                copies
                    .get(at)
                    .and_then(|c| c.iter().map(|a| projection_of(r, a, record)).max())
                    .unwrap_or(Ownership::Foreign)
            };
            let mut best: Option<(Slot, Ownership)> = None;
            for at in slots {
                let o = ownership(&at);
                if best.map_or(true, |(_, b)| o > b) {
                    best = Some((at, o));
                }
            }
            best.map(|(at, o)| (at, o != Ownership::Foreign))
        })
        .collect();

    // A slot is one file name in one folder, so it holds at most one mods-folder entry.
    let mut in_root: HashSet<Slot> = HashSet::new();
    let mut foreign: HashMap<Slot, &Candidate> = HashMap::new();
    let mut loose = Vec::new();
    for (r, verdict) in root_cands.iter().zip(verdicts) {
        match verdict {
            None => loose.push(Job {
                read: r,
                organized: false,
                active: true, // a vanilla mod sits in the mods folder, so it loads
                behind: None,
            }),
            Some((at, ours)) => {
                in_root.insert(at);
                if !ours {
                    foreign.insert(at, r);
                }
            }
        }
    }

    let mut jobs = Vec::with_capacity(archive_cands.len() + loose.len());
    let mut stood_in: HashSet<Slot> = HashSet::new();
    for a in archive_cands {
        let key = slot(a);
        // Only the first archived copy at that name gives up its row to the stand-in.
        let stand_in = foreign.get(&key).filter(|_| stood_in.insert(key));
        jobs.push(match stand_in {
            Some(r) => Job {
                read: r,
                organized: true,
                active: true,
                behind: Some(a),
            },
            // Organized mods are "active" only when also present in the mods folder.
            None => Job {
                read: a,
                organized: true,
                active: in_root.contains(&key),
                behind: None,
            },
        });
    }
    jobs.extend(loose);
    jobs
}

/// The slots of archived mods with a row that mods-folder entry `r` may sit at. Its own file
/// name first. Failing that, for a zip, the archived spellings of the same techName whose
/// extension is spelled differently (`FS25_A.ZIP` for an archived `FS25_A.zip`), as the old
/// techName match allowed: only those no entry is listed under and that the folder resolves to
/// this very entry, i.e. on a case-insensitive volume, where `organize` opening the archived
/// name there reaches it. The techName itself is never case-folded (it is the mod's identity).
/// An entry matching only a later archive's copy has no row to stand in for, so it gets none.
fn archived_slots<'a>(
    r: &'a Candidate,
    visible: &HashMap<Slot<'a>, &'a str>,
    zips: &HashMap<(usize, &'a str), Vec<Slot<'a>>>,
    listed: &HashSet<Slot<'a>>,
) -> Vec<Slot<'a>> {
    let own = slot(r);
    // At the archived name only with the archived techName: a folder named `FS25_A.zip` holds
    // the name of an archived `FS25_A.zip` but is another mod (techName `FS25_A.zip`).
    if let Some(tech_name) = visible.get(&own) {
        return if *tech_name == r.tech_name {
            vec![own]
        } else {
            Vec::new()
        };
    }
    if r.kind != "zip" {
        return Vec::new();
    }
    let Some(spellings) = zips.get(&(r.layout, r.tech_name.as_str())) else {
        return Vec::new();
    };
    let stamp = |p: &Path| {
        fs::symlink_metadata(p)
            .ok()
            .map(|m| (m.file_type().is_dir(), m.len(), m.modified().ok()))
    };
    let mine = stamp(&r.path);
    spellings
        .iter()
        .filter(|at| !listed.contains(at))
        .filter(|at| mine.is_some() && stamp(&r.path.with_file_name(at.1)) == mine)
        .copied()
        .collect()
}

/// Convenience wrapper with no cache — used by the example harness and tests.
pub fn scan_with<F>(roots: Vec<PathBuf>, progress: F) -> ScanResult
where
    F: Fn(usize, usize) + Sync + Send,
{
    scan_cached(roots, &HashMap::new(), progress).result
}

#[cfg(test)]
mod cap_tests {
    use super::read_capped;

    #[test]
    fn capped_read_errors_past_limit_never_truncates() {
        assert_eq!(read_capped(&b"hello"[..], 100, "x").unwrap(), b"hello");
        assert!(read_capped(&b"hello"[..], 5, "x").is_ok()); // exactly at cap
        assert!(read_capped(&vec![0u8; 200][..], 100, "big").is_err()); // over cap
    }
}

#[cfg(test)]
mod symlink_tests {
    use super::collect_root_candidates;

    #[cfg(windows)]
    fn symlink_dir(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_dir(src, dst)
    }
    #[cfg(unix)]
    fn symlink_dir(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(src, dst)
    }

    #[test]
    fn follows_a_symlinked_dir_mod() {
        let base = std::env::temp_dir().join(format!("silo_symlink_scan_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("root");
        let target = base.join("devmod");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("modDesc.xml"), b"<modDesc/>").unwrap();

        let link = root.join("FS25_DevLink");
        if symlink_dir(&target, &link).is_err() {
            // Creating a symlink needs privilege on Windows (Developer Mode/admin) — if it's
            // unavailable in this environment, skip rather than fail.
            let _ = std::fs::remove_dir_all(&base);
            return;
        }

        let cands = collect_root_candidates(&root);
        assert!(
            cands
                .iter()
                .any(|c| c.tech_name == "FS25_DevLink" && c.kind == "dir"),
            "a symlinked dir mod should be scanned, got: {:?}",
            cands.iter().map(|c| &c.tech_name).collect::<Vec<_>>()
        );

        let _ = std::fs::remove_dir_all(&base);
    }
}

#[cfg(test)]
mod settings_detect_tests {
    use super::{
        detect_settings, is_settings_xml, lua_has_settings_signature, read_lua, SettingsSource,
        LUA_MEMBER_CAP, LUA_MEMBER_LIMIT,
    };

    #[test]
    fn is_settings_xml_matches_value_and_gui_files_only() {
        assert!(is_settings_xml("shared/defaultusersettings.xml"));
        assert!(is_settings_xml("config/realisticeconomysettings.xml"));
        assert!(is_settings_xml("gui/settingspage.xml"));
        // not settings files
        assert!(!is_settings_xml("moddesc.xml"));
        assert!(!is_settings_xml("data/vehicle.xml"));
        assert!(!is_settings_xml("shared/defaultusersettings.lua"));
        // translation/l10n files that merely contain "settings" are excluded
        assert!(!is_settings_xml("translations/translation_settings_en.xml"));
        assert!(!is_settings_xml("l10n/settings_de.xml"));
    }

    #[test]
    fn lua_signature_catches_modsettings_paths_not_savegame_hooks() {
        assert!(lua_has_settings_signature(
            b"local p = getUserProfileAppPath() .. \"modSettings/Foo.xml\""
        ));
        assert!(lua_has_settings_signature(b"g_currentModSettingsDirectory"));
        assert!(lua_has_settings_signature(b"\"modsSettings/typo.xml\"")); // known typo
                                                                           // the standard savegame persistence hook is NOT a settings signal
        assert!(!lua_has_settings_signature(
            b"function foo:saveToXMLFile(xml) end"
        ));
    }

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("silo_settings_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn dir_shipped_beats_runtime() {
        let d = tmp("shipped");
        std::fs::write(d.join("modDesc.xml"), b"<modDesc/>").unwrap();
        std::fs::create_dir_all(d.join("scripts")).unwrap();
        std::fs::write(
            d.join("scripts/main.lua"),
            b"getUserProfileAppPath()..\"modSettings/X.xml\"",
        )
        .unwrap();
        std::fs::write(d.join("mysettings.xml"), b"<settings/>").unwrap();
        assert_eq!(detect_settings(&d, "dir"), (true, SettingsSource::Shipped));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn dir_runtime_when_only_lua_signature() {
        let d = tmp("runtime");
        std::fs::write(d.join("modDesc.xml"), b"<modDesc/>").unwrap();
        std::fs::create_dir_all(d.join("scripts")).unwrap();
        std::fs::write(d.join("scripts/s.lua"), b"local d = g_modSettingsDirectory").unwrap();
        assert_eq!(detect_settings(&d, "dir"), (true, SettingsSource::Runtime));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn read_lua_reports_what_it_consumed() {
        assert_eq!(read_lua(&b"abc"[..], 3), (3, Some(b"abc".to_vec())));
        // Over the cap: the bytes read still count, the contents don't come back.
        assert_eq!(read_lua(&b"abcdef"[..], 3), (4, None));
    }

    /// A zip of Lua members, in order, deflated.
    fn lua_zip(path: &std::path::Path, members: &[(String, Vec<u8>)]) {
        use std::io::Write;
        let mut w = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (name, bytes) in members {
            w.start_file(name.as_str(), zip::write::SimpleFileOptions::default())
                .unwrap();
            w.write_all(bytes).unwrap();
        }
        w.finish().unwrap();
    }

    const SIGNATURE: &[u8] = b"local d = g_modSettingsDirectory";

    /// Oversized members cost their decompressed bytes too (#351): two zero-filled ones just
    /// over the per-member cap spend the whole budget, so a later member goes unread.
    #[test]
    fn oversized_lua_members_spend_the_budget() {
        let d = tmp("oversized");
        let big = vec![0u8; LUA_MEMBER_CAP as usize + 1];
        let sig = ("z.lua".to_string(), SIGNATURE.to_vec());
        let zip = d.join("m.zip");
        lua_zip(&zip, std::slice::from_ref(&sig));
        assert_eq!(
            detect_settings(&zip, "zip"),
            (true, SettingsSource::Runtime)
        );
        let a = ("a.lua".to_string(), big.clone());
        let b = ("b.lua".to_string(), big);
        lua_zip(&zip, &[a, b, sig]);
        assert_eq!(detect_settings(&zip, "zip"), (false, SettingsSource::None));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Only the first [`LUA_MEMBER_LIMIT`] Lua members are opened (#351).
    #[test]
    fn lua_members_past_the_limit_go_unread() {
        let d = tmp("member_limit");
        let zip = d.join("m.zip");
        let with_sig_at = |at: usize| {
            let mut m: Vec<(String, Vec<u8>)> =
                (0..at).map(|i| (format!("s/{i}.lua"), vec![])).collect();
            m.push(("s/last.lua".to_string(), SIGNATURE.to_vec()));
            m
        };
        lua_zip(&zip, &with_sig_at(LUA_MEMBER_LIMIT - 1));
        assert_eq!(
            detect_settings(&zip, "zip"),
            (true, SettingsSource::Runtime)
        );
        lua_zip(&zip, &with_sig_at(LUA_MEMBER_LIMIT));
        assert_eq!(detect_settings(&zip, "zip"), (false, SettingsSource::None));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn dir_none_when_no_signal() {
        let d = tmp("none");
        std::fs::write(d.join("modDesc.xml"), b"<modDesc/>").unwrap();
        std::fs::write(d.join("v.lua"), b"function v:saveToXMLFile() end").unwrap();
        assert_eq!(detect_settings(&d, "dir"), (false, SettingsSource::None));
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    fn moddesc_zip(path: &std::path::Path, title: &str) {
        let f = std::fs::File::create(path).unwrap();
        let mut w = zip::ZipWriter::new(f);
        w.start_file("modDesc.xml", zip::write::SimpleFileOptions::default())
            .unwrap();
        use std::io::Write;
        write!(
            w,
            "<modDesc descVersion=\"92\"><title><en>{title}</en></title><version>1.0.0.0</version></modDesc>"
        )
        .unwrap();
        w.finish().unwrap();
    }

    /// Fresh, uniquely named temp dir for one test.
    fn fresh(tag: &str) -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!("silo_layout_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    fn scan(layouts: Vec<(PathBuf, Vec<PathBuf>)>) -> Vec<ModEntry> {
        scan_layouts(layouts, &HashMap::new(), |_, _| {})
            .result
            .mods
    }

    /// A cache row written by the pre-#266 parser (no `parseRev`, title cut at the `&`)
    /// is parsed again even though the file is unchanged; a current row is reused as is.
    #[test]
    fn rows_from_an_older_parser_are_parsed_again() {
        let base = fresh("parse_rev");
        let mods = base.join("mods");
        std::fs::create_dir_all(&mods).unwrap();
        let zip = mods.join("FS25_ElmAsh.zip");
        moddesc_zip(&zip, "Elm &amp; Ash");
        let c = Candidate {
            path: zip.clone(),
            tech_name: "FS25_ElmAsh".into(),
            kind: "zip",
            layout: 0,
        };
        let (fresh_entry, parsed) = resolve_entry(&c, &HashMap::new());
        let cache_key = parsed.expect("parsed fresh");
        assert_eq!(fresh_entry.title.as_deref(), Some("Elm & Ash"));

        let mut old = serde_json::to_value(&fresh_entry).unwrap();
        old["title"] = "Elm".into();
        old.as_object_mut().unwrap().remove("parseRev");
        let row = |json: String| CacheEntry {
            mtime_ms: cache_key.mtime_ms,
            size: cache_key.size,
            json,
        };
        let key = zip.to_string_lossy().into_owned();

        let stale = HashMap::from([(key.clone(), row(old.to_string()))]);
        let (entry, parsed) = resolve_entry(&c, &stale);
        assert!(parsed.is_some(), "an old-parser row must be re-read");
        assert_eq!(entry.title.as_deref(), Some("Elm & Ash"));

        let current = HashMap::from([(key, row(serde_json::to_string(&fresh_entry).unwrap()))]);
        let (_, parsed) = resolve_entry(&c, &current);
        assert!(parsed.is_none(), "a current row is reused");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn reads_two_archives_without_duplicating_a_mod() {
        let base = fresh("two_archives");
        let mods = base.join("mods");
        let new_arch = base.join("new");
        let old_arch = base.join("old");
        for d in [&mods, &new_arch.join("Tools"), &old_arch.join("Tools")] {
            std::fs::create_dir_all(d).unwrap();
        }
        moddesc_zip(&new_arch.join("Tools/FS25_Moved.zip"), "Moved");
        moddesc_zip(&old_arch.join("Tools/FS25_Moved.zip"), "Moved");
        moddesc_zip(&old_arch.join("Tools/FS25_NotYet.zip"), "Not yet");

        let found = scan(vec![(
            mods.clone(),
            vec![new_arch.clone(), old_arch.clone()],
        )]);
        let names: Vec<_> = found.iter().map(|m| m.tech_name.clone()).collect();
        assert_eq!(names.iter().filter(|n| *n == "FS25_Moved").count(), 1);
        assert!(names.contains(&"FS25_NotYet".to_string()));
        let moved = found.iter().find(|m| m.tech_name == "FS25_Moved").unwrap();
        assert!(
            Path::new(&moved.path).starts_with(&new_arch),
            "the new archive wins"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn first_listed_archive_wins_in_either_order() {
        let base = fresh("order");
        let mods = base.join("mods");
        let a = base.join("a");
        let b = base.join("b");
        for d in [&mods, &a.join("Tools"), &b.join("Tools")] {
            std::fs::create_dir_all(d).unwrap();
        }
        moddesc_zip(&a.join("Tools/FS25_Moved.zip"), "Moved");
        moddesc_zip(&b.join("Tools/FS25_Moved.zip"), "Moved");

        // Reversed order: [old, new] → the first-listed (old) wins.
        let found = scan(vec![(mods.clone(), vec![b.clone(), a.clone()])]);
        assert_eq!(found.len(), 1);
        assert!(Path::new(&found[0].path).starts_with(&b));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn same_tech_name_in_two_categories_of_one_archive_is_kept() {
        let base = fresh("one_archive_dupe");
        let mods = base.join("mods");
        let arch = base.join("arch");
        for d in [&mods, &arch.join("Tools"), &arch.join("Misc")] {
            std::fs::create_dir_all(d).unwrap();
        }
        moddesc_zip(&arch.join("Tools/FS25_Dup.zip"), "Dup");
        moddesc_zip(&arch.join("Misc/FS25_Dup.zip"), "Dup");

        // One archive keeps behaving exactly like the old `<root>/archive` scan.
        let found = scan(vec![(mods.clone(), vec![arch.clone()])]);
        assert_eq!(
            found.iter().filter(|m| m.tech_name == "FS25_Dup").count(),
            2
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn archive_dedupe_is_per_layout() {
        let base = fresh("per_layout");
        let (mods1, mods2) = (base.join("mods1"), base.join("mods2"));
        let (arch1, arch2) = (base.join("arch1"), base.join("arch2"));
        for d in [&mods1, &mods2, &arch1.join("Tools"), &arch2.join("Tools")] {
            std::fs::create_dir_all(d).unwrap();
        }
        moddesc_zip(&arch1.join("Tools/FS25_Same.zip"), "Same");
        moddesc_zip(&arch2.join("Tools/FS25_Same.zip"), "Same");

        let found = scan(vec![
            (mods1.clone(), vec![arch1.clone()]),
            (mods2.clone(), vec![arch2.clone()]),
        ]);
        let same: Vec<_> = found
            .iter()
            .filter(|m| m.tech_name == "FS25_Same")
            .collect();
        assert_eq!(same.len(), 2, "each layout keeps its own copy");
        assert!(same.iter().any(|m| Path::new(&m.path).starts_with(&arch1)));
        assert!(same.iter().any(|m| Path::new(&m.path).starts_with(&arch2)));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn silo_temp_entries_are_not_mods() {
        let base = fresh("temp_entries");
        let mods = base.join("mods");
        let arch = base.join("arch");
        let cat = arch.join("Tools");
        std::fs::create_dir_all(&mods).unwrap();
        std::fs::create_dir_all(&cat).unwrap();
        let desc = "<modDesc descVersion=\"92\"><version>1.0.0.0</version></modDesc>";
        for dir in [
            cat.join("FS25_X.silo-move-tmp"),
            cat.join("FS25_V.silo-move-old"),
            cat.join("FS25_Y.silo-relink-tmp"),
            cat.join(".FS25_Z.silo-tmp"),
            mods.join("FS25_X.silo-move-tmp"),
            mods.join(".FS25_Z.silo-tmp"),
        ] {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("modDesc.xml"), desc).unwrap();
        }
        // Zip flavours of the same temp names are ignored too.
        moddesc_zip(&cat.join("FS25_W.zip.silo-move-tmp"), "W");
        moddesc_zip(&cat.join("FS25_U.zip.silo-move-old"), "U");
        // A real mod alongside them is still found.
        moddesc_zip(&cat.join("FS25_Real.zip"), "Real");

        let found = scan(vec![(mods.clone(), vec![arch.clone()])]);
        let names: Vec<_> = found.iter().map(|m| m.tech_name.as_str()).collect();
        assert_eq!(names, vec!["FS25_Real"], "got: {names:?}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn is_silo_temp_matches_move_relink_and_dot_tmp_names() {
        assert!(is_silo_temp("FS25_X.silo-move-tmp"));
        assert!(is_silo_temp("FS25_X.zip.silo-move-old"));
        assert!(is_silo_temp("FS25_X.SILO-MOVE-OLD"));
        assert!(is_silo_temp("FS25_X.SILO-RELINK-TMP"));
        assert!(is_silo_temp(".FS25_X.silo-tmp"));
        assert!(is_silo_temp("FS25_X.zip.silo-update.part"));
        assert!(is_silo_temp("FS25_X.zip.SILO-UPDATE-OLD"));
        // Link probes (and their links), including the fixed-name cloud check's.
        assert!(is_silo_temp(".silo-hardlink-probe-12-3"));
        assert!(is_silo_temp(".silo-hardlink-probe-12-3.link"));
        assert!(is_silo_temp(".silo-symlink-probe-12-3.link"));
        assert!(is_silo_temp(".silo-hardlink-probe"));
        assert!(!is_silo_temp("FS25_X.silo-tmp")); // only the dot-prefixed form is organize's
        assert!(!is_silo_temp("FS25_X.zip"));
        assert!(!is_silo_temp("FS25_X"));
    }

    /// A folder mod with a modDesc titled `title`.
    fn moddesc_dir(dir: &std::path::Path, title: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("modDesc.xml"),
            format!("<modDesc descVersion=\"92\"><title><en>{title}</en></title></modDesc>"),
        )
        .unwrap();
    }

    /// #353: a folder mod's cache key follows its modDesc.xml, so an in-place edit (which
    /// leaves the folder's own mtime and size alone) is parsed again.
    #[test]
    fn an_in_place_moddesc_edit_refreshes_a_folder_mod() {
        let base = fresh("dir_cache");
        let dir = base.join("FS25_Dev");
        moddesc_dir(&dir, "Before");
        let c = Candidate {
            path: dir.clone(),
            tech_name: "FS25_Dev".into(),
            kind: "dir",
            layout: 0,
        };
        let (entry, key) = resolve_entry(&c, &HashMap::new());
        let key = key.expect("parsed fresh");
        let cache = HashMap::from([(
            dir.to_string_lossy().into_owned(),
            CacheEntry {
                mtime_ms: key.mtime_ms,
                size: key.size,
                json: serde_json::to_string(&entry).unwrap(),
            },
        )]);
        assert!(resolve_entry(&c, &cache).1.is_none(), "unchanged: reused");

        // Rewrite modDesc.xml in place (a later mtime); the folder's own mtime doesn't move.
        let folder_mtime = fs::metadata(&dir).unwrap().modified().unwrap();
        let desc = dir.join("modDesc.xml");
        let later =
            fs::metadata(&desc).unwrap().modified().unwrap() + std::time::Duration::from_secs(5);
        let mut f = fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&desc)
            .unwrap();
        std::io::Write::write_all(
            &mut f,
            b"<modDesc descVersion=\"92\"><title><en>After!</en></title></modDesc>",
        )
        .unwrap();
        f.set_modified(later).unwrap();
        drop(f);
        assert_eq!(
            fs::metadata(&dir).unwrap().modified().unwrap(),
            folder_mtime
        );

        let (entry, key) = resolve_entry(&c, &cache);
        assert!(key.is_some(), "the edit is noticed");
        assert_eq!(entry.title.as_deref(), Some("After!"));

        // A zip's key is still just its own mtime and size.
        let zip = base.join("FS25_Z.zip");
        moddesc_zip(&zip, "Z");
        let (m, s) = stamp(&zip);
        let z = Candidate {
            path: zip,
            tech_name: "FS25_Z".into(),
            kind: "zip",
            layout: 0,
        };
        assert_eq!(
            cache_key(&z),
            CacheKey {
                mtime_ms: m,
                size: s
            }
        );
        let _ = fs::remove_dir_all(&base);
    }

    /// One mods folder with its archive, scanned with the stand-in map.
    fn scan_one(mods: &Path, arch: &Path) -> ScanOutput {
        scan_layouts(
            vec![(mods.to_path_buf(), vec![arch.to_path_buf()])],
            &HashMap::new(),
            |_, _| {},
        )
    }

    /// Silo's projections (a hardlink, a byte-for-byte copy, a marked folder copy) still fold
    /// into the archived entry exactly as before (#353).
    #[test]
    fn projections_fold_into_the_archived_entry() {
        let base = fresh("projections");
        let mods = base.join("mods");
        let arch = base.join("arch");
        let cat = arch.join("Tools");
        std::fs::create_dir_all(&mods).unwrap();
        std::fs::create_dir_all(&cat).unwrap();
        moddesc_zip(&cat.join("FS25_Linked.zip"), "Linked");
        moddesc_zip(&cat.join("FS25_Copied.zip"), "Copied");
        moddesc_dir(&cat.join("FS25_Folder"), "Folder");
        std::fs::hard_link(cat.join("FS25_Linked.zip"), mods.join("FS25_Linked.zip")).unwrap();
        std::fs::copy(cat.join("FS25_Copied.zip"), mods.join("FS25_Copied.zip")).unwrap();
        let copy = mods.join("FS25_Folder");
        moddesc_dir(&copy, "Folder");
        std::fs::write(copy.join(crate::organize::SILO_MARKER), b"silo\n").unwrap();

        let out = scan_one(&mods, &arch);
        assert!(out.stand_ins.is_empty());
        assert_eq!(out.result.mods.len(), 3);
        for m in &out.result.mods {
            assert!(m.organized && m.active, "{}", m.tech_name);
            assert!(Path::new(&m.path).starts_with(&arch), "{}", m.path);
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A different file at an archived mod's name (another build, the user's own folder, a
    /// folder where a zip is archived) is what the game loads, so its row describes that file;
    /// the archived copy behind it is reported for the manifest (#353).
    #[test]
    fn a_swapped_in_file_stands_in_for_the_archived_mod() {
        let base = fresh("stand_in");
        let mods = base.join("mods");
        let arch = base.join("arch");
        let cat = arch.join("Tools");
        std::fs::create_dir_all(&mods).unwrap();
        std::fs::create_dir_all(&cat).unwrap();
        moddesc_zip(&cat.join("FS25_Zip.zip"), "Archived v1");
        moddesc_zip(&mods.join("FS25_Zip.zip"), "Swapped in v2");
        moddesc_dir(&cat.join("FS25_Dir"), "Archived dir");
        moddesc_dir(&mods.join("FS25_Dir"), "Own dir"); // no Silo marker

        let out = scan_one(&mods, &arch);
        assert_eq!(out.result.mods.len(), 2, "one row per mod");
        for (name, title, kind) in [
            ("FS25_Zip", "Swapped in v2", "zip"),
            ("FS25_Dir", "Own dir", "dir"),
        ] {
            let m = out
                .result
                .mods
                .iter()
                .find(|m| m.tech_name == name)
                .unwrap();
            assert_eq!(m.title.as_deref(), Some(title));
            assert_eq!(m.kind, kind);
            assert!(Path::new(&m.path).starts_with(&mods), "{}", m.path);
            assert!(m.organized && m.active);
            let behind = &out.stand_ins[&m.path];
            assert!(Path::new(&behind.path).starts_with(&cat), "{}", behind.path);
            assert_eq!(behind.kind, kind);
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A same-size file at an archived zip's name: Silo's recorded stamp decides (#353). One that
    /// still matches the record is Silo's copy; one that doesn't is the user's build; with no
    /// record, the same size is trusted (the foreign-file check hashes it after the scan).
    #[test]
    fn a_same_size_swap_is_caught_by_the_projection_record() {
        let base = fresh("same_size");
        let mods = base.join("mods");
        let arch = base.join("arch");
        let cat = arch.join("Tools");
        std::fs::create_dir_all(&mods).unwrap();
        std::fs::create_dir_all(&cat).unwrap();
        // Same length, different version: "1.0.0.0" vs "2.0.0.0".
        moddesc_zip(&cat.join("FS25_A.zip"), "Version 1");
        moddesc_zip(&mods.join("FS25_A.zip"), "Version 2");
        let flat = mods.join("FS25_A.zip");
        assert_eq!(
            fs::metadata(&flat).unwrap().len(),
            fs::metadata(cat.join("FS25_A.zip")).unwrap().len()
        );
        let stamp = file_stamp(&fs::metadata(&flat).unwrap());
        let scan_rec = |rec: Option<(u64, i64)>| {
            let records: ProjectionRecords = rec
                .map(|r| HashMap::from([("FS25_A.zip".to_string(), r)]))
                .unwrap_or_default();
            scan_layouts_with(
                vec![(mods.clone(), vec![arch.clone()])],
                &[records],
                &HashMap::new(),
                |_, _| {},
            )
        };

        let unrecorded = scan_rec(None);
        assert!(unrecorded.stand_ins.is_empty(), "trusted without a record");
        let matching = scan_rec(Some(stamp));
        assert!(matching.stand_ins.is_empty(), "Silo's own recorded copy");
        assert_eq!(matching.result.mods[0].title.as_deref(), Some("Version 1"));

        let swapped = scan_rec(Some((stamp.0, stamp.1 - 60_000)));
        assert_eq!(swapped.stand_ins.len(), 1);
        assert_eq!(swapped.result.mods.len(), 1);
        assert_eq!(swapped.result.mods[0].title.as_deref(), Some("Version 2"));
        assert!(swapped.result.mods[0].active);
        let _ = fs::remove_dir_all(&base);
    }

    /// The other kind of an archived mod's techName (an unpacked folder beside an archived zip)
    /// is not at that mod's file name, so it is its own loose row, also beside a real projection;
    /// the archived mod is active only through its own file name, as `organize` sees it.
    #[test]
    fn the_other_kind_of_an_archived_name_is_its_own_row() {
        let base = fresh("other_kind");
        let mods = base.join("mods");
        let arch = base.join("arch");
        let cat = arch.join("Tools");
        std::fs::create_dir_all(&mods).unwrap();
        std::fs::create_dir_all(&cat).unwrap();
        moddesc_zip(&cat.join("FS25_Kind.zip"), "Archived zip");
        moddesc_dir(&mods.join("FS25_Kind"), "Unpacked");
        moddesc_zip(&cat.join("FS25_Both.zip"), "Both zip");
        std::fs::hard_link(cat.join("FS25_Both.zip"), mods.join("FS25_Both.zip")).unwrap();
        moddesc_dir(&mods.join("FS25_Both"), "Both dir");

        let out = scan_one(&mods, &arch);
        assert!(out.stand_ins.is_empty());
        assert_eq!(out.result.mods.len(), 4);
        let row = |title: &str| {
            out.result
                .mods
                .iter()
                .find(|m| m.title.as_deref() == Some(title))
                .unwrap()
                .clone()
        };
        let parked = row("Archived zip");
        assert!(parked.organized && !parked.active);
        let both = row("Both zip");
        assert!(both.organized && both.active);
        for loose in [row("Unpacked"), row("Both dir")] {
            assert!(!loose.organized && loose.active);
            assert!(Path::new(&loose.path).starts_with(&mods));
        }
        let _ = fs::remove_dir_all(&base);
    }

    /// One archive holding a name in two categories: a hardlink to either copy is a projection,
    /// so neither row is replaced.
    #[test]
    fn a_link_to_a_second_category_copy_is_a_projection() {
        let base = fresh("two_cats");
        let mods = base.join("mods");
        let arch = base.join("arch");
        for d in [&mods, &arch.join("Tools"), &arch.join("Misc")] {
            std::fs::create_dir_all(d).unwrap();
        }
        moddesc_zip(&arch.join("Tools/FS25_Dup.zip"), "Dup in tools");
        moddesc_zip(
            &arch.join("Misc/FS25_Dup.zip"),
            "Dup in misc, a longer title",
        );
        for cat in ["Tools", "Misc"] {
            let flat = mods.join("FS25_Dup.zip");
            let _ = std::fs::remove_file(&flat);
            std::fs::hard_link(arch.join(cat).join("FS25_Dup.zip"), &flat).unwrap();
            let out = scan_one(&mods, &arch);
            assert!(out.stand_ins.is_empty(), "linked to {cat}");
            assert_eq!(out.result.mods.len(), 2);
            assert!(out
                .result
                .mods
                .iter()
                .all(|m| m.organized && Path::new(&m.path).starts_with(&arch)));
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A zip spelled `.ZIP` at an archived `.zip`'s name. Where the volume ignores case, the
    /// folder resolves the archived spelling to it, so it is at that mod's slot (here Silo's own
    /// hardlink, so the archived row stands). Where case matters it is a different file name,
    /// so it is its own loose row and the archived mod is parked.
    #[test]
    fn a_differently_spelled_extension_follows_the_volume() {
        let base = fresh("ext_case");
        let mods = base.join("mods");
        let arch = base.join("arch");
        std::fs::create_dir_all(&mods).unwrap();
        std::fs::create_dir_all(arch.join("Tools")).unwrap();
        moddesc_zip(&arch.join("Tools/FS25_A.zip"), "Archived");
        std::fs::hard_link(arch.join("Tools/FS25_A.zip"), mods.join("FS25_A.ZIP")).unwrap();
        let ignores_case = mods.join("FS25_A.zip").exists();

        let out = scan_one(&mods, &arch);
        assert!(out.stand_ins.is_empty());
        let rows: Vec<(bool, bool)> = out
            .result
            .mods
            .iter()
            .map(|m| (m.organized, m.active))
            .collect();
        if ignores_case {
            assert_eq!(rows, vec![(true, true)]);
        } else {
            assert_eq!(rows.len(), 2);
            assert!(rows.contains(&(true, false)) && rows.contains(&(false, true)));
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Mid archive move the older archive can hold a folder where the newer one holds a zip of
    /// the same techName. A mods-folder entry matching only that hidden copy has no row to stand
    /// in for, so it is listed as its own row, never dropped.
    #[test]
    fn an_entry_matching_only_a_hidden_archive_copy_is_listed() {
        let base = fresh("shadow_only");
        let mods = base.join("mods");
        let (new_arch, old_arch) = (base.join("new"), base.join("old"));
        std::fs::create_dir_all(&mods).unwrap();
        std::fs::create_dir_all(new_arch.join("Tools")).unwrap();
        moddesc_zip(&new_arch.join("Tools/FS25_A.zip"), "New zip");
        moddesc_dir(&old_arch.join("Tools/FS25_A"), "Old folder");
        moddesc_dir(&mods.join("FS25_A"), "In the mods folder");
        std::fs::write(
            mods.join("FS25_A").join(crate::organize::SILO_MARKER),
            b"silo\n",
        )
        .unwrap();

        let out = scan_layouts(
            vec![(mods.clone(), vec![new_arch, old_arch])],
            &HashMap::new(),
            |_, _| {},
        );
        assert!(out.stand_ins.is_empty());
        let shown = out
            .result
            .mods
            .iter()
            .find(|m| m.title.as_deref() == Some("In the mods folder"))
            .expect("listed");
        assert!(shown.active && !shown.organized);
        assert_eq!(out.result.mods.len(), 2);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Two categories hold the same techName spelled `.zip` and `.ZIP`, and the mods folder holds
    /// a hardlink to the second, spelled `.Zip`. Where the volume ignores case it is a projection
    /// of the copy it links, whatever the order the slots are tried in; where case matters it is
    /// its own loose row. Either way nothing stands in.
    #[test]
    fn mixed_extension_spellings_match_the_copy_actually_linked() {
        let base = fresh("ext_mixed");
        let mods = base.join("mods");
        let arch = base.join("arch");
        for d in [&mods, &arch.join("Tools"), &arch.join("Misc")] {
            std::fs::create_dir_all(d).unwrap();
        }
        // The same size, so only the link's identity tells them apart.
        moddesc_zip(&arch.join("Tools/FS25_A.zip"), "Version 1");
        moddesc_zip(&arch.join("Misc/FS25_A.ZIP"), "Version 2");
        assert_eq!(
            fs::metadata(arch.join("Tools/FS25_A.zip")).unwrap().len(),
            fs::metadata(arch.join("Misc/FS25_A.ZIP")).unwrap().len()
        );
        std::fs::hard_link(arch.join("Misc/FS25_A.ZIP"), mods.join("FS25_A.Zip")).unwrap();
        let ignores_case = mods.join("FS25_A.zip").exists();

        for _ in 0..4 {
            let out = scan_one(&mods, &arch);
            assert!(out.stand_ins.is_empty());
            let loose = out.result.mods.iter().filter(|m| !m.organized).count();
            assert_eq!(loose, usize::from(!ignores_case));
            let two = out
                .result
                .mods
                .iter()
                .find(|m| m.organized && m.title.as_deref() == Some("Version 2"))
                .unwrap();
            assert_eq!(two.active, ignores_case);
            let one = out
                .result
                .mods
                .iter()
                .find(|m| m.organized && m.title.as_deref() == Some("Version 1"))
                .unwrap();
            assert!(!one.active, "only the linked copy is active");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A folder named like an archived zip (`FS25_A.zip/modDesc.xml`) holds that file name but
    /// is another mod (techName `FS25_A.zip`): its own row, never a stand-in that would rename
    /// the archived mod in the manifest.
    #[test]
    fn a_folder_named_like_an_archived_zip_is_its_own_mod() {
        let base = fresh("dir_named_zip");
        let mods = base.join("mods");
        let arch = base.join("arch");
        std::fs::create_dir_all(&mods).unwrap();
        std::fs::create_dir_all(arch.join("Tools")).unwrap();
        moddesc_zip(&arch.join("Tools/FS25_A.zip"), "Archived");
        moddesc_dir(&mods.join("FS25_A.zip"), "Folder");

        let out = scan_one(&mods, &arch);
        assert!(out.stand_ins.is_empty());
        let folder = out
            .result
            .mods
            .iter()
            .find(|m| m.title.as_deref() == Some("Folder"))
            .unwrap();
        assert!(!folder.organized && folder.active);
        assert_eq!(folder.tech_name, "FS25_A.zip");
        let archived = out.result.mods.iter().find(|m| m.organized).unwrap();
        assert_eq!(archived.tech_name, "FS25_A");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    fn symlink(src: &Path, dst: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(src, dst)
    }
    #[cfg(windows)]
    fn symlink(src: &Path, dst: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_dir(src, dst)
    }

    /// A link at an archived mod's name is Silo's projection only when it resolves to the
    /// archived copy; a dev link pointing elsewhere stands in for it (#353).
    #[test]
    fn a_link_is_a_projection_only_when_it_points_at_the_archive() {
        let base = fresh("links");
        let mods = base.join("mods");
        let arch = base.join("arch");
        let cat = arch.join("Tools");
        std::fs::create_dir_all(&mods).unwrap();
        std::fs::create_dir_all(&cat).unwrap();
        moddesc_dir(&cat.join("FS25_Ours"), "Ours");
        moddesc_dir(&cat.join("FS25_Dev"), "Archived dev");
        let dev = base.join("dev");
        moddesc_dir(&dev, "Working copy");
        if symlink(&cat.join("FS25_Ours"), &mods.join("FS25_Ours")).is_err()
            || symlink(&dev, &mods.join("FS25_Dev")).is_err()
        {
            // Symlinks need privilege on Windows (Developer Mode/admin): skip without it.
            let _ = std::fs::remove_dir_all(&base);
            return;
        }
        let out = scan_one(&mods, &arch);
        let row = |n: &str| {
            out.result
                .mods
                .iter()
                .find(|m| m.tech_name == n)
                .unwrap()
                .clone()
        };
        assert!(Path::new(&row("FS25_Ours").path).starts_with(&arch));
        assert_eq!(row("FS25_Dev").title.as_deref(), Some("Working copy"));
        assert_eq!(out.stand_ins.len(), 1);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Mid archive move, a mod can sit in both archives while its mods-folder link still points
    /// at the old copy: that is still Silo's projection, not a stand-in.
    #[test]
    fn a_link_into_the_old_archive_mid_move_is_still_a_projection() {
        let base = fresh("links_mid_move");
        let mods = base.join("mods");
        let (new_arch, old_arch) = (base.join("new"), base.join("old"));
        std::fs::create_dir_all(&mods).unwrap();
        moddesc_dir(&new_arch.join("Tools/FS25_A"), "A");
        moddesc_dir(&old_arch.join("Tools/FS25_A"), "A");
        if symlink(&old_arch.join("Tools/FS25_A"), &mods.join("FS25_A")).is_err() {
            let _ = std::fs::remove_dir_all(&base);
            return; // no symlink privilege on this Windows box
        }
        let out = scan_layouts(
            vec![(mods.clone(), vec![new_arch.clone(), old_arch])],
            &HashMap::new(),
            |_, _| {},
        );
        assert!(out.stand_ins.is_empty());
        assert_eq!(out.result.mods.len(), 1);
        assert!(Path::new(&out.result.mods[0].path).starts_with(&new_arch));
        assert!(out.result.mods[0].active);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A mods-folder entry is matched only against its own layout's archive: a name archived in
    /// another layout is that folder's own loose mod, not hidden behind the other archive.
    #[test]
    fn a_name_archived_in_another_layout_is_a_loose_mod() {
        let base = fresh("cross_layout");
        let (mods1, mods2) = (base.join("mods1"), base.join("mods2"));
        let (arch1, arch2) = (base.join("arch1"), base.join("arch2"));
        for d in [&mods1, &mods2, &arch1.join("Tools"), &arch2] {
            std::fs::create_dir_all(d).unwrap();
        }
        moddesc_zip(&arch1.join("Tools/FS25_A.zip"), "Archived");
        moddesc_zip(&mods2.join("FS25_A.zip"), "Loose in two");

        let found = scan(vec![(mods1, vec![arch1]), (mods2.clone(), vec![arch2])]);
        assert_eq!(found.len(), 2);
        let archived = found.iter().find(|m| m.organized).unwrap();
        assert!(
            !archived.active,
            "nothing at its name in its own mods folder"
        );
        let loose = found.iter().find(|m| !m.organized).unwrap();
        assert!(loose.active && Path::new(&loose.path).starts_with(&mods2));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// #419 repro: an organized mod whose mods-folder entry FS25 can't open (a link to a folder
    /// that's gone, a plain file where a folder mod should be, a folder where a zip should be)
    /// is not active in the scan: it shows parked, which is what FS25 loads.
    #[test]
    fn an_entry_fs25_cant_open_leaves_the_archived_mod_parked() {
        let base = fresh("broken_entry");
        let mods = base.join("mods");
        let arch = base.join("arch");
        let cat = arch.join("Tools");
        std::fs::create_dir_all(&mods).unwrap();
        std::fs::create_dir_all(&cat).unwrap();
        moddesc_dir(&cat.join("FS25_Dangling"), "Dangling");
        let gone = base.join("gone");
        std::fs::create_dir_all(&gone).unwrap();
        crate::organize::link_dir_native(&gone, &mods.join("FS25_Dangling")).unwrap();
        std::fs::remove_dir(&gone).unwrap();
        moddesc_dir(&cat.join("FS25_PlainFile"), "PlainFile");
        std::fs::write(mods.join("FS25_PlainFile"), b"x").unwrap();
        moddesc_zip(&cat.join("FS25_EmptyDir.zip"), "EmptyDir");
        std::fs::create_dir_all(mods.join("FS25_EmptyDir.zip")).unwrap();

        let out = scan_one(&mods, &arch);
        assert_eq!(out.result.mods.len(), 3, "{:?}", out.result.mods);
        for m in &out.result.mods {
            assert!(m.organized, "{}", m.tech_name);
            assert!(!m.active, "{} shows active", m.tech_name);
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}

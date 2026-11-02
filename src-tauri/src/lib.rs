//! Silo core — Tauri command surface. All heavy logic lives in sibling modules so
//! it stays unit-testable (and reusable by a future CLI) without a running app.

pub mod aliasbackfill;
pub mod bindings;
pub mod bisect;
pub mod bridge;
pub mod catalogalias;
pub mod category;
pub mod collection;
pub mod conflicts;
pub mod db;
mod diagnostics;
pub mod error;
pub mod filltypes;
pub mod firstrun;
pub mod fsgame;
mod fslog;
pub mod gamelaunch;
pub mod gameprofile;
pub mod gh_session;
pub mod github;
pub mod heighttypes;
pub mod icons;
pub mod logscan;
pub mod moddesc;
pub mod modsroot;
pub mod mpsync;
pub mod net;
pub mod notices;
pub mod organize;
pub mod paths;
mod picked;
pub mod profile;
pub mod provenance;
pub mod relocate;
pub mod savegame;
pub mod scan;
mod scratch;
pub mod secrets;
pub mod settings_form;
pub mod siloapi;
pub mod source;
pub mod store;
pub mod update;
pub mod xmlconfig;
pub mod xmltext;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use tauri::{Emitter, Manager};

#[derive(Clone, serde::Serialize)]
struct Progress {
    done: usize,
    total: usize,
}

/// Silo's own GitHub OAuth App client id, baked in so end users get one-click
/// "Connect GitHub" (device flow) with no manual setup. Register ONE OAuth App
/// under the HLLMR account (github.com/settings/applications/new), enable
/// "Device Flow", and paste its Client ID (Iv1.…/Ov23…) here. Client ids are
/// public — safe to embed; device flow needs no secret. Empty = fall back to a
/// user-provided id from Settings.
const SILO_GH_CLIENT_ID: &str = "Ov23lizY2TSJF2P5CRyx";

// The client id is a compile-time toggle: empty = fall back to a user-provided id. For a
// build with it baked in, `is_empty()` is const-false — that's intentional, not a bug.
#[allow(clippy::const_is_empty)]
fn effective_client_id(conn: &rusqlite::Connection) -> Option<String> {
    if !SILO_GH_CLIENT_ID.is_empty() {
        Some(SILO_GH_CLIENT_ID.to_string())
    } else {
        db::get_app_setting(conn, "gh_client_id")
    }
}

fn db_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    profile::app_data_dir(app)
        .map(|d| d.join("silo.db"))
        .map_err(|e| e.to_string())
}

/// Run a command's disk, database, keychain or process work on the blocking pool. A sync
/// command runs on the main thread, so anything slower than a few microseconds there freezes
/// the window. Every command that isn't pure goes through this.
async fn off_main<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|e| e.to_string())?
}

/// Scan the given roots (or the auto-detected default when omitted/empty) — only those that
/// are trusted mods folders, so nothing at all while the folder is blocked.
/// Runs on a blocking thread so the async runtime / UI never stalls.
#[tauri::command]
async fn scan_mods(
    app: tauri::AppHandle,
    roots: Option<Vec<String>>,
    link_catalog: Option<bool>,
) -> Result<scan::ScanResult, String> {
    let supplied: Vec<PathBuf> = roots
        .unwrap_or_default()
        .into_iter()
        .map(PathBuf::from)
        .collect();

    tauri::async_runtime::spawn_blocking(move || {
        // A mod update Silo was in the middle of when it stopped is finished or rolled back
        // first, so the scan sees the library as it should be.
        let recovered = recover_interrupted_updates();
        // Taken after this scan's own recovery, so only writes from here on count against it.
        let start = quiet_epoch(&LIBRARY_WRITE, &LIBRARY_EPOCH);
        // Read after `start`: a change of archive (repoint, move) before this point is seen
        // here, and one after it moves the epoch on, so the reconcile below is refused.
        let active = modsroot::active();
        let trusted = trusted_roots(active.clone(), modsroot::resolved(), false);
        let plan = scan_plan(supplied, &trusted, active.as_ref());
        let mut conn = db::open(&db_path(&app)?)?;
        let emitter = app.clone();
        let mut result = scan_library(
            &mut conn,
            &plan,
            active.as_ref(),
            &LIBRARY_WRITE,
            &LIBRARY_EPOCH,
            start,
            move |done, total| {
                let _ = emitter.emit("scan:progress", Progress { done, total });
            },
        );
        result.notices = recovered.notices;
        result.problems = recovered.problems;
        // Only with the user's leave to send mod names to the catalog (the startup update
        // check): the backfill asks about some of them (#402).
        if link_catalog == Some(true) {
            spawn_alias_backfill(app, backfill_zips(&result.mods));
        }
        Ok::<_, String>(result)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// How long a catalog-link backfill waits for the library lock before leaving the record for
/// the next scan.
const ALIAS_BACKFILL_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// Held while a catalog-link backfill runs, so two never run at once.
static ALIAS_BACKFILL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The library zips in `mods` (from a scan) as the catalog-link backfill takes them.
fn backfill_zips(mods: &[scan::ModEntry]) -> Vec<aliasbackfill::LibraryZip> {
    mods.iter()
        .filter(|m| m.kind == "zip")
        .map(|m| aliasbackfill::LibraryZip {
            path: PathBuf::from(&m.path),
            tech_name: m.tech_name.clone(),
            size: m.size,
            mtime_ms: m.mtime_ms,
        })
        .collect()
}

/// Link the zips in `zips` that Silo installed under another name before catalog links were
/// recorded at install (#402; see `aliasbackfill`). Leaves the files to a run already going (it
/// or a later one takes them). Never fails: problems are logged. Tells the window which mods were
/// linked (`catalog:linked`, their techNames: a link is for the name and bytes, so every copy of
/// those bytes is linked), so a drawer showing one and the Updates list ask again.
fn backfill_catalog_links(app: &tauri::AppHandle, zips: &[aliasbackfill::LibraryZip]) {
    if zips.is_empty() {
        return;
    }
    let _running = match ALIAS_BACKFILL.try_lock() {
        Ok(g) => g,
        Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return,
    };
    let ran = (|| -> Result<Vec<String>, String> {
        let conn = db::open(&db_path(app)?)?;
        // A database too busy to tidy up is too busy for the run's notes too.
        db::prune_alias_checks(&conn)
            .map_err(|e| format!("couldn't tidy its notes, the database is busy: {e}"))?;
        let catalog = aliasbackfill::SiloApi {
            base: siloapi_base(app)?,
        };
        // A record waits for the library lock at most this long, and never past the run's time.
        let out = aliasbackfill::run(&conn, zips, &catalog, provenance::sha256_file, |left| {
            catalog_link_write_within(left.min(ALIAS_BACKFILL_WAIT))
        });
        let linked = linked_tech_names(zips, &out);
        if !out.is_empty() {
            log::info!(
                "[catalog] link backfill looked at {} mods, linked {}",
                out.len(),
                linked.len()
            );
        }
        Ok(linked)
    })();
    match ran {
        Ok(linked) if !linked.is_empty() => {
            let _ = app.emit("catalog:linked", linked);
        }
        Ok(_) => {}
        Err(e) => log::warn!("[catalog] link backfill skipped: {e}"),
    }
}

/// The techNames of the zips a backfill run linked, each once (`catalog:linked`'s payload).
fn linked_tech_names(
    zips: &[aliasbackfill::LibraryZip],
    out: &[(PathBuf, aliasbackfill::Outcome)],
) -> Vec<String> {
    let linked: HashSet<&PathBuf> = out
        .iter()
        .filter(|(_, o)| matches!(o, aliasbackfill::Outcome::Linked(_)))
        .map(|(p, _)| p)
        .collect();
    if linked.is_empty() {
        return Vec::new();
    }
    let mut names: Vec<String> = zips
        .iter()
        .filter(|z| linked.contains(&z.path))
        .map(|z| z.tech_name.clone())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Run the catalog-link backfill on the blocking pool without holding up the caller (a scan or
/// an update check): its links arrive through `catalog:linked`.
fn spawn_alias_backfill(app: tauri::AppHandle, zips: Vec<aliasbackfill::LibraryZip>) {
    if zips.is_empty() {
        return;
    }
    tauri::async_runtime::spawn_blocking(move || backfill_catalog_links(&app, &zips));
}

/// What a scan reads, and the one folder whose `organized` manifest it may rewrite.
#[derive(Debug, Default, PartialEq, Eq)]
struct ScanPlan {
    roots: Vec<PathBuf>,
    reconcile_key: Option<String>,
}

/// The supplied roots that are trusted mods folders (all of `trusted` when none are supplied;
/// `trusted` is empty while the folder is blocked), and the first one's key only when it is the
/// active layout's mods folder — a scan never rewrites the manifest of any other folder.
fn scan_plan(
    supplied: Vec<PathBuf>,
    trusted: &[PathBuf],
    active: Option<&modsroot::Active>,
) -> ScanPlan {
    let roots: Vec<PathBuf> = if supplied.is_empty() {
        trusted.to_vec()
    } else {
        supplied
            .into_iter()
            .filter_map(|s| {
                trusted
                    .iter()
                    .find(|t| modsroot::same_folder(t, &s))
                    .cloned()
            })
            .collect()
    };
    let reconcile_key = roots
        .first()
        .filter(|r| active.is_some_and(|a| modsroot::same_folder(&a.layout.mods, r)))
        .map(|r| modsroot::canonical_key(r));
    ScanPlan {
        roots,
        reconcile_key,
    }
}

/// Scan `plan.roots` with their archives, persist the parse cache, and — only for
/// `plan.reconcile_key`, on a quiet library (`start`, `lock`, `epoch`) — rewrite that folder's
/// `organized` manifest from what's in its archive.
#[allow(clippy::too_many_arguments)]
fn scan_library(
    conn: &mut rusqlite::Connection,
    plan: &ScanPlan,
    active: Option<&modsroot::Active>,
    lock: &'static std::sync::Mutex<()>,
    epoch: &std::sync::atomic::AtomicU64,
    start: Option<u64>,
    progress: impl Fn(usize, usize) + Sync + Send,
) -> scan::ScanResult {
    // Nothing to read (a blocked folder): leave the cache and every manifest as they are —
    // pruning against an empty scan would forget every cached mod.
    if plan.roots.is_empty() {
        return scan::ScanResult::default();
    }
    {
        let roots = &plan.roots;
        let scan_key = plan.reconcile_key.clone();
        // Warm cache: parsed entries persist between launches, keyed by
        // path+mtime+size, so unchanged mods skip archive parsing entirely.
        let cache = db::load_cache(conn);

        // Each mods folder with its archive(s), and whether the scanned folder's manifest may
        // be rewritten from what's found (see `scan_archives`).
        let mut may_reconcile = true;
        let layouts: Vec<(PathBuf, Vec<PathBuf>)> = roots
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let (archives, ok) = scan_archives(conn, r, active);
                if i == 0 {
                    may_reconcile = ok;
                }
                (r.clone(), archives)
            })
            .collect();

        // What Silo recorded for the zips it projected, so a file swapped in at one of their
        // names is told apart without hashing (#353).
        let records: Vec<scan::ProjectionRecords> = roots
            .iter()
            .map(|r| db::load_projections(conn, &modsroot::canonical_key(r)))
            .collect();
        let out = scan::scan_layouts_with(layouts, &records, &cache, progress);

        // Persist freshly-parsed rows; prune mods that vanished from disk.
        let fresh_rows: Vec<(String, u64, u64, String)> = out
            .result
            .mods
            .iter()
            .filter_map(|m| {
                let key = out.fresh_paths.get(&m.path)?;
                serde_json::to_string(m)
                    .ok()
                    .map(|json| (m.path.clone(), key.mtime_ms, key.size, json))
            })
            .collect();
        // A failed cache write only costs a re-parse next scan; logged, not surfaced.
        if let Err(e) = db::upsert_many(conn, &fresh_rows) {
            log::warn!(
                "scan: couldn't save {} parsed mods to the cache: {e}",
                fresh_rows.len()
            );
        }

        // An archived copy hidden behind a stand-in is still on disk: keep its cached row.
        let present: HashSet<String> = out
            .result
            .mods
            .iter()
            .map(|m| m.path.clone())
            .chain(out.stand_ins.values().map(|a| a.path.clone()))
            .collect();
        if let Err(e) = db::prune_missing(conn, &present) {
            log::warn!("scan: couldn't drop vanished mods from the cache: {e}");
        }

        // Keep the `organized` manifest in step with what's physically in `archive/`, so an
        // archived mod is always activatable even if the DB drifted from disk (e.g. a fresh
        // DB behind a populated archive). The archive layout is `archive/<Category>/<file>`,
        // so the category is the file's parent directory name.
        let organized_rows: Vec<db::OrganizedRow> = out
            .result
            .mods
            .iter()
            .filter(|m| m.organized)
            .filter_map(|m| {
                // A row standing in for an archived mod describes the mods-folder file; the
                // manifest names the archived copy behind it (#353).
                let (path, kind) = match out.stand_ins.get(&m.path) {
                    Some(a) => (&a.path, &a.kind),
                    None => (&m.path, &m.kind),
                };
                let p = std::path::Path::new(path);
                let file_name = p.file_name()?.to_string_lossy().into_owned();
                let category = p.parent()?.file_name()?.to_string_lossy().into_owned();
                Some(db::OrganizedRow {
                    tech_name: m.tech_name.clone(),
                    file_name,
                    kind: kind.clone(),
                    category,
                    subcategory: None,
                    active: m.active,
                })
            })
            .collect();
        // Only on a quiet library: a scan's snapshot of the archive may predate, or overlap, an
        // organize, activate, install or move, and rewriting the manifest from it would undo it.
        if let Some(key) = scan_key.as_deref().filter(|_| may_reconcile) {
            let quiet = reconcile_if_quiet(lock, epoch, start, conn, key, &organized_rows);
            if !quiet {
                log::info!("scan: library changed while scanning; manifest left for the next scan");
            }
        }
        out.result
    }
}

/// The archive(s) a scan of mods folder `root` reads, and whether that scan may rewrite the
/// folder's `organized` manifest from what it finds. During an unfinished move both archives
/// are read (the current direction's destination first, so it wins for mods already moved) and
/// the manifest is left alone — the files are in flux. Otherwise the folder's own archive: the
/// published one for the active folder (`active`), the stored choice (or the default) for any
/// other. A custom archive that isn't there (its drive unplugged), or any archive that exists
/// but can't be read, scans as empty — so the manifest is left alone then too: rewriting it
/// would forget the whole library.
fn scan_archives(
    conn: &rusqlite::Connection,
    root: &std::path::Path,
    active: Option<&modsroot::Active>,
) -> (Vec<PathBuf>, bool) {
    if let Some(m) = relocate::load_marker(conn, &modsroot::canonical_key(root)) {
        let archives = match m.direction {
            relocate::Direction::Forward => vec![m.to, m.from],
            relocate::Direction::Back => vec![m.from, m.to],
        };
        return (archives, false);
    }
    let layout = match active {
        Some(a) if modsroot::same_folder(&a.layout.mods, root) => a.layout.clone(),
        _ => resolve_active(conn, root).layout,
    };
    let ok = !layout.archive_missing() && relocate::filed_mods(&layout.archive).is_ok();
    (vec![layout.archive], ok)
}

/// Return a mod's icon as a cached PNG `data:` URL (or null if unavailable).
/// Decodes off-thread; cached to the app cache dir on first use.
#[tauri::command]
async fn get_mod_icon(
    app: tauri::AppHandle,
    path: String,
    kind: String,
    icon_filename: Option<String>,
) -> Option<String> {
    let icon = icon_filename?;
    let cache_dir = profile::app_cache_dir(&app).ok()?.join("icons-v2");
    tauri::async_runtime::spawn_blocking(move || {
        icons::cached_data_url(&cache_dir, std::path::Path::new(&path), &kind, &icon)
    })
    .await
    .ok()
    .flatten()
}

// ── Curation (favorite / hidden / broken) ──
#[tauri::command]
async fn get_curation(app: tauri::AppHandle) -> Result<Vec<db::CurationRow>, String> {
    let db = db_path(&app)?;
    off_main(move || Ok(db::load_curation(&db::open(&db)?))).await
}

#[tauri::command]
async fn set_curation(app: tauri::AppHandle, row: db::CurationRow) -> Result<(), String> {
    let db = db_path(&app)?;
    off_main(move || db::set_curation(&db::open(&db)?, &row)).await
}

// ── GitHub update checking ──
#[tauri::command]
async fn get_mod_repos(app: tauri::AppHandle) -> Result<Vec<db::RepoRow>, String> {
    let db = db_path(&app)?;
    off_main(move || Ok(db::load_repos(&db::open(&db)?))).await
}

#[tauri::command]
async fn set_mod_repo(
    app: tauri::AppHandle,
    tech_name: String,
    owner: String,
    repo: String,
) -> Result<(), String> {
    // Empty owner or repo means "unlink" (db::set_repo deletes the row); anything else must
    // be a real GitHub repository name before it's stored and later used in API calls.
    if !owner.trim().is_empty() && !repo.trim().is_empty() {
        github::check_repo(owner.trim(), repo.trim())?;
    }
    let db = db_path(&app)?;
    off_main(move || db::set_repo(&db::open(&db)?, &tech_name, &owner, &repo)).await
}

// ── Source pages: where each mod came from (#196) ──
// Silo only stores these addresses. No command here (or anywhere) requests them; the
// frontend hands them to the user's browser via `openExternal`.
#[tauri::command]
async fn get_mod_sources(app: tauri::AppHandle) -> Result<Vec<db::SourceRow>, String> {
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || -> Result<Vec<db::SourceRow>, String> {
        let conn = db::open(&db)?;
        Ok(db::load_sources(&conn))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Save (or remove, when `url` is empty or null) a mod's source page. Returns the GitHub repo
/// link it also made, when the page is a GitHub repository and the mod had none.
#[tauri::command]
async fn set_mod_source(
    app: tauri::AppHandle,
    tech_name: String,
    url: Option<String>,
) -> Result<Option<db::RepoRow>, String> {
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || -> Result<Option<db::RepoRow>, String> {
        let conn = db::open(&db)?;
        source::save(&conn, &tech_name, url.as_deref())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Record that the user checked a mod's source page just now. Returns the time (unix secs).
#[tauri::command]
async fn mark_source_checked(app: tauri::AppHandle, tech_name: String) -> Result<i64, String> {
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || -> Result<i64, String> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let conn = db::open(&db)?;
        if db::mark_source_checked(&conn, &tech_name, now)? {
            Ok(now)
        } else {
            Err("Save the page this mod came from first.".to_string())
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Scan a mod's modDesc.xml for a github.com/owner/repo reference.
#[tauri::command]
async fn guess_repo(path: String, kind: String) -> Result<Option<db::RepoRow>, String> {
    off_main(move || {
        let Ok(xml) = scan::read_moddesc_xml(std::path::Path::new(&path), &kind) else {
            return Ok(None);
        };
        Ok(
            github::find_repo_in_text(&xml).map(|(owner, repo)| db::RepoRow {
                tech_name: String::new(),
                owner,
                repo,
            }),
        )
    })
    .await
}

#[tauri::command]
async fn check_mod_update(
    app: tauri::AppHandle,
    owner: String,
    repo: String,
    current: String,
) -> Result<github::UpdateInfo, String> {
    github::check_repo(&owner, &repo)?;
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || -> Result<github::UpdateInfo, String> {
        let conn = db::open(&db)?;
        let token = gh_bearer(&conn);
        github::check(&owner, &repo, &current, token.as_ref())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct GhStatus {
    client_id: Option<String>,
    user: Option<String>,
    /// True when a client id is baked into the app (no user setup needed).
    builtin: bool,
    /// True when the stored token can act (star/watch): a `public_repo` OAuth grant
    /// or a PAT. Read-only (`read:user`) connections report false.
    can_write: bool,
    /// True when the stored token can create/read the user's gists (the `gist` OAuth
    /// grant) — needed to share a Collection. Independent of `can_write`.
    can_gist: bool,
}

/// Does a stored scope string grant `want`? Handles GitHub's two formats (the device
/// response is space-separated, `X-OAuth-Scopes` is comma-separated). The `"*"` sentinel
/// means "scopes unknown — assume yes", used for fine-grained PATs that report none.
fn scope_grants(scopes: &str, want: &str) -> bool {
    scopes == "*" || scopes.split([',', ' ']).map(str::trim).any(|s| s == want)
}

#[tauri::command]
#[allow(clippy::const_is_empty)]
async fn gh_status(app: tauri::AppHandle) -> Result<GhStatus, String> {
    let db = db_path(&app)?;
    off_main(move || {
        let conn = db::open(&db)?;
        // Signed out: no read removes what a Disconnect left behind, so do it here. A Disconnect
        // that couldn't clean up storage at all (the marker is still there) is retried whole.
        if GH_SESSION.signed_out() {
            let retried = GH_SESSION.retry_sign_out(|| gh_clear_account(&conn));
            match retried {
                Some(Err(e)) => log::warn!("GitHub: retrying the unfinished Disconnect: {e}"),
                Some(Ok(())) => {}
                None => secrets::sweep(&conn, "gh_token"),
            }
        }
        // Capability comes from the scopes GitHub actually granted. Older connections predate
        // scope capture and only carry the coarse booleans — honour those until the next
        // reconnect writes `gh_scopes`.
        let (can_write, can_gist) = match db::get_app_setting(&conn, "gh_scopes") {
            Some(s) => (
                scope_grants(&s, "public_repo") || scope_grants(&s, "repo"),
                scope_grants(&s, "gist"),
            ),
            None => (
                db::get_app_setting(&conn, "gh_write").as_deref() == Some("1"),
                db::get_app_setting(&conn, "gh_gist").as_deref() == Some("1"),
            ),
        };
        Ok(GhStatus {
            client_id: effective_client_id(&conn),
            // Disconnected in this run (even if clearing the stored copy failed): not connected.
            user: db::get_app_setting(&conn, "gh_user").filter(|_| !GH_SESSION.signed_out()),
            builtin: !SILO_GH_CLIENT_ID.is_empty(),
            can_write,
            can_gist,
        })
    })
    .await
}

#[tauri::command]
async fn gh_set_client_id(app: tauri::AppHandle, client_id: String) -> Result<(), String> {
    let db = db_path(&app)?;
    off_main(move || {
        let conn = db::open(&db)?;
        let v = client_id.trim();
        db::set_app_setting(
            &conn,
            "gh_client_id",
            if v.is_empty() { None } else { Some(v) },
        )
    })
    .await
}

#[tauri::command]
async fn gh_device_start(
    app: tauri::AppHandle,
    write: Option<bool>,
    gist: Option<bool>,
    attempt: u64,
) -> Result<github::DeviceCode, String> {
    let db = db_path(&app)?;
    // Ask only for what the user is enabling — a plain sign-in for update-checks stays
    // read-only. `public_repo` is added for star/watch actions, `gist` for Collection
    // sharing. GitHub accumulates scopes across authorizations, so the caller requests
    // the union of what it already holds plus the new capability.
    let scope = build_gh_scope(write.unwrap_or(false), gist.unwrap_or(false));
    // `attempt` is from `gh_sign_in_begin`: if anything newer (a sign-in, a cancel, a
    // Disconnect) came since, the flow is refused below.
    tauri::async_runtime::spawn_blocking(move || -> Result<github::DeviceCode, String> {
        let conn = db::open(&db)?;
        let cid = effective_client_id(&conn)
            .ok_or_else(|| "No GitHub OAuth App Client ID configured".to_string())?;
        let dc = github::device_start(&cid, &scope)?;
        // Refused when a newer attempt, a cancel or a Disconnect came meanwhile.
        GH_SESSION.remember_flow(&dc.device_code, attempt, dc.expires_in)?;
        Ok(dc)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Build the OAuth scope string for a device-flow request. `read:user` is always
/// present (identity + higher rate limit); `public_repo`/`gist` are added per capability.
fn build_gh_scope(write: bool, gist: bool) -> String {
    let mut scopes = vec!["read:user"];
    if write {
        scopes.push("public_repo");
    }
    if gist {
        scopes.push("gist");
    }
    scopes.join(" ")
}

/// The GitHub session: which token may be sent, and which sign-in may store one (see
/// `gh_session`).
static GH_SESSION: gh_session::Session = gh_session::Session::new();

/// The stored GitHub token, tied to the current session: each request with it is admitted only
/// while that session lasts. None when not connected (or Disconnected in this run).
fn gh_bearer(conn: &rusqlite::Connection) -> Option<gh_session::Bearer<'static>> {
    GH_SESSION.bearer(|| secrets::get(conn, "gh_token"))
}

/// `gh_bearer`, for an action that needs the account.
fn gh_bearer_required(conn: &rusqlite::Connection) -> Result<gh_session::Bearer<'static>, String> {
    gh_bearer(conn).ok_or_else(|| "Connect your GitHub account first.".to_string())
}

/// Store a sign-in's account: token, login and the scopes GitHub granted. A failure says whether
/// the token may have changed anyway (then the session signs out rather than act half-switched).
fn store_gh_account(
    conn: &rusqlite::Connection,
    token: &str,
    user: &str,
    scopes: &str,
) -> Result<(), gh_session::StoreFailed> {
    secrets::store(conn, "gh_token", token).map_err(|e| gh_session::StoreFailed {
        token_changed: e.changed,
        message: if e.changed {
            format!("{} {GH_HALF_SAVED}", e.message)
        } else {
            e.message
        },
    })?;
    let half_saved = |e: String| gh_session::StoreFailed {
        message: format!("{e} {GH_HALF_SAVED}"),
        token_changed: true,
    };
    db::set_app_setting(conn, "gh_user", Some(user)).map_err(half_saved)?;
    db::set_app_setting(conn, "gh_scopes", Some(scopes)).map_err(half_saved)
}

/// Added when a sign-in's token was saved but the rest wasn't.
const GH_HALF_SAVED: &str =
    "Silo disconnected GitHub so it doesn't act as the wrong account. Connect again.";

/// A device flow GitHub approved but whose account Silo couldn't confirm: nothing is stored.
fn sign_in_unconfirmed(why: &str) -> github::PollResult {
    github::PollResult {
        status: "error".into(),
        token: None,
        scope: None,
        error: Some(format!("couldn't confirm the account: {why}")),
    }
}

/// A poll's answer for a device flow that's over without storing anything: `status` is
/// "cancelled" (a newer sign-in, a cancel or a Disconnect) or "expired".
fn ended_poll(status: &str) -> github::PollResult {
    github::PollResult {
        status: status.into(),
        token: None,
        scope: None,
        error: None,
    }
}

#[tauri::command]
async fn gh_device_poll(
    app: tauri::AppHandle,
    device_code: String,
) -> Result<github::PollResult, String> {
    // The attempt the flow started under: a newer sign-in, a cancel or a Disconnect since then
    // wins, however late this runs.
    let attempt = match GH_SESSION.flow(&device_code) {
        gh_session::FlowState::Live(a) => a,
        gh_session::FlowState::Expired => return Ok(ended_poll("expired")),
        gh_session::FlowState::Gone => return Ok(ended_poll("cancelled")),
    };
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || -> Result<github::PollResult, String> {
        let conn = db::open(&db)?;
        let cid =
            effective_client_id(&conn).ok_or_else(|| "No client id configured".to_string())?;
        // The exchange yields a token, so it too goes out only while this sign-in is the newest.
        let res = {
            let Ok(_pass) = GH_SESSION.admit_attempt(attempt) else {
                return Ok(ended_poll("cancelled"));
            };
            github::device_poll(&cid, &device_code)?
        };
        if res.status == "ok" {
            if let Some(tok) = &res.token {
                // No account name, no connection: committing a token with an empty login would
                // leave it in use with the screen showing "not connected" (and no Disconnect).
                let user = match github::whoami(&GH_SESSION.sign_in_bearer(attempt, tok)) {
                    Ok(u) if !u.is_empty() => u,
                    Ok(_) => return Ok(sign_in_unconfirmed("GitHub didn't name the account")),
                    Err(e) => return Ok(sign_in_unconfirmed(&e)),
                };
                // Store the scopes GitHub actually granted — capability is derived from these,
                // not from what we asked for. Accumulates across re-auth.
                let scopes = res.scope.as_deref().unwrap_or("");
                let stored =
                    GH_SESSION.commit(attempt, || store_gh_account(&conn, tok, &user, scopes));
                match stored {
                    Some(r) => r?,
                    // Replaced, cancelled or Disconnected while this flow was out: dropped.
                    None => return Ok(ended_poll("cancelled")),
                }
            }
        }
        // Never expose the raw token to the frontend.
        Ok(github::PollResult { token: None, ..res })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Start a sign-in attempt (a device flow or a PAT save), before its own command: every older
/// attempt can no longer store a token. The frontend calls this in order with Disconnect, so
/// the two are ordered as the user clicked them however the commands themselves are scheduled.
#[tauri::command]
async fn gh_sign_in_begin() -> Result<u64, String> {
    off_main(|| Ok(GH_SESSION.begin())).await
}

/// Cancel a device flow (the user clicked Cancel, or a start answered after it was cancelled):
/// its polls can't store a token afterwards. True when it was the newest sign-in and is now
/// cancelled; false when it was already over (it may have just stored its token), so the
/// caller re-reads the connection state.
#[tauri::command]
async fn gh_device_cancel(device_code: String) -> Result<bool, String> {
    // Off the main thread: a Disconnect or a sign-in storing its token holds the session lock
    // while it talks to the credential store.
    off_main(move || Ok(GH_SESSION.cancel_flow(&device_code))).await
}

/// PAT fallback for users who'd rather mint a token than OAuth. A classic PAT reports its
/// scopes (`X-OAuth-Scopes`), so we store exactly what it can do. A fine-grained PAT uses
/// a different permission model and reports no scopes — we can't introspect it, so we
/// record the `"*"` sentinel ("assume capable", since the user minted it deliberately) and
/// let a 403 surface a reconnect hint if a specific permission is missing.
#[tauri::command]
async fn gh_set_pat(app: tauri::AppHandle, pat: String, attempt: u64) -> Result<String, String> {
    // `attempt` is from `gh_sign_in_begin`, like a device flow's: a newer sign-in or a
    // Disconnect while GitHub checks the token wins.
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        let pat = github::clean_pat(&pat)?;
        let (user, scopes) = github::whoami_scoped(&GH_SESSION.sign_in_bearer(attempt, &pat))?;
        if user.is_empty() {
            return Err("GitHub didn't accept that token. Check it and paste it again.".into());
        }
        let conn = db::open(&db)?;
        let stored = if scopes.trim().is_empty() {
            "*".to_string()
        } else {
            scopes
        };
        GH_SESSION
            .commit(attempt, || store_gh_account(&conn, &pat, &user, &stored))
            .ok_or_else(|| gh_session::SUPERSEDED.to_string())??;
        Ok(user)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn gh_repo_stats(
    app: tauri::AppHandle,
    owner: String,
    repo: String,
) -> Result<github::RepoStats, String> {
    github::check_repo(&owner, &repo)?;
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || -> Result<github::RepoStats, String> {
        let conn = db::open(&db)?;
        let token = gh_bearer(&conn);
        github::repo_stats(&owner, &repo, token.as_ref())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn gh_star(
    app: tauri::AppHandle,
    owner: String,
    repo: String,
    on: bool,
) -> Result<bool, String> {
    github::check_repo(&owner, &repo)?;
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || -> Result<bool, String> {
        let conn = db::open(&db)?;
        let token = gh_bearer_required(&conn)?;
        github::set_star(&owner, &repo, &token, on)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn gh_watch(
    app: tauri::AppHandle,
    owner: String,
    repo: String,
    on: bool,
) -> Result<bool, String> {
    github::check_repo(&owner, &repo)?;
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || -> Result<bool, String> {
        let conn = db::open(&db)?;
        let token = gh_bearer_required(&conn)?;
        github::set_watch(&owner, &repo, &token, on)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// What `download_update` tells the frontend: where the previous version is kept, and anything
/// that went wrong after the new version was in place (the update itself stands).
#[derive(Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateOutcome {
    backup: Option<String>,
    problems: Vec<String>,
}

const UPDATE_GAME_RUNNING: &str = "Close Farming Simulator 25 first, then update.";

/// FS25 writes gameSettings.xml (and mods their settings) back when the game exits, so a change
/// made while it runs is silently undone.
const CONFIG_GAME_RUNNING: &str = "Close Farming Simulator 25 first. The game saves its own \
     settings when it exits, so a change made now would be lost.";

fn config_write_refusal(game_running: bool) -> Result<(), String> {
    if game_running {
        Err(CONFIG_GAME_RUNNING.into())
    } else {
        Ok(())
    }
}

/// "Use this version" and "Keep Silo's copy" move a mod's file in and out of the mods folder,
/// which fails part-way while the game holds it open.
const SWAP_GAME_RUNNING: &str =
    "Close Farming Simulator 25 first. Silo won't swap a mod's files while the game is using them.";

fn swap_refusal(game_running: bool) -> Result<(), String> {
    if game_running {
        Err(SWAP_GAME_RUNNING.into())
    } else {
        Ok(())
    }
}

/// FS25's `modsDirectoryOverride` decides the active mods folder when the user hasn't chosen
/// one in Silo, so only `point_game_at` (Settings) may change it, never a config edit.
const OVERRIDE_IS_SILOS: &str = "Change the mods folder in Settings.";

/// Pure: refuse an edit of gameSettings.xml that changes the mods-folder override (its folder
/// or whether it's on, or adds or removes it). Every other edit is allowed.
fn override_edit_refusal(old: &str, new: &str) -> Result<(), String> {
    if modsroot::read_override(old) == modsroot::read_override(new) {
        Ok(())
    } else {
        Err(OVERRIDE_IS_SILOS.into())
    }
}

/// When a config write targets FS25's gameSettings.xml, refuse it if it would change the
/// mods-folder override. `old` is the current text when the caller already read it; an
/// unreadable file counts as one without the override.
fn ensure_override_kept(
    user: &std::path::Path,
    target: &std::path::Path,
    old: Option<&str>,
    new: &str,
) -> Result<(), String> {
    if !modsroot::same_folder(target, &user.join("gameSettings.xml")) {
        return Ok(());
    }
    match old {
        Some(old) => override_edit_refusal(old, new),
        None => {
            let old = paths::guarded_xml_read(&[user.to_path_buf()], target).unwrap_or_default();
            override_edit_refusal(&old, new)
        }
    }
}

const UPDATE_NOT_A_LIBRARY_MOD: &str = "Silo only updates mods in your library.";

/// `download_update` may replace only a mod zip directly in the mods folder or filed in an
/// archive category folder; never a backup or any other file under those folders. The folder
/// is resolved once to where it really is, so a category that is a link (or junction) into
/// another folder, e.g. `mods/backups`, is judged by where it leads.
fn update_target_problem(dest: &std::path::Path, layout: &organize::Layout) -> Option<String> {
    let refused = || Some(UPDATE_NOT_A_LIBRARY_MOD.to_string());
    let is_zip = dest
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("zip"));
    let Some(parent) = dest.parent().and_then(|p| std::fs::canonicalize(p).ok()) else {
        return refused();
    };
    let in_mods = modsroot::same_folder(&parent, &layout.mods);
    let in_category = parent
        .parent()
        .is_some_and(|a| modsroot::same_folder(a, &layout.archive));
    if is_zip && (in_mods || in_category) {
        None
    } else {
        refused()
    }
}

/// What `download_update` checks again once it holds the library lock, since things may have
/// changed while it downloaded; returns the layout as it is now. An archive move started (and
/// paused) meanwhile comes first: the swap must not replace a file the move already copied.
fn update_recheck(dest: &std::path::Path) -> Result<organize::Layout, String> {
    ensure_no_move()?;
    ensure_library_write(dest)?;
    let layout = primary_layout(None)?;
    if let Some(p) = update_target_problem(dest, &layout) {
        return Err(p);
    }
    if gamelaunch::fs25_running() {
        return Err(UPDATE_GAME_RUNNING.into());
    }
    Ok(layout)
}

/// Update an installed mod from a GitHub release asset (`update` module): download beside it
/// without the library lock, then swap it in under the lock, keeping the previous version in
/// `backups/`. Refused while FS25 runs (its files may be open).
#[tauri::command]
async fn download_update(
    app: tauri::AppHandle,
    path: String,
    asset_url: String,
) -> Result<UpdateOutcome, error::AppError> {
    ensure_no_move()?;
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || -> Result<UpdateOutcome, String> {
        if gamelaunch::fs25_running() {
            return Err(UPDATE_GAME_RUNNING.into());
        }
        let dest = PathBuf::from(&path);
        // Defence against a compromised webview naming an arbitrary write target: the path
        // must contain no `..` AND its parent must resolve inside a real mods root (the flat
        // root or its archive, wherever that lives). An absolute path outside them — which
        // `no_traversal` alone would pass — is rejected here.
        paths::no_traversal(&dest)?;
        ensure_library_write(&dest)?;
        // Only a mod zip in the library, never a backup or another file under its folders.
        let layout = primary_layout(None)?;
        if let Some(p) = update_target_problem(&dest, &layout) {
            return Err(p);
        }
        // The catalog link of the file there now (#386), checked against its hash.
        // A release file named differently from the installed one is a different mod to FS25,
        // so it's never written over this one under this name (#391).
        update::check_asset_name(&asset_url, &dest)?;
        let old_link = current_link(&db, &dest)?;
        // Public release assets don't need auth, and a download must never carry the user's
        // GitHub credential — keep the token for explicit API actions only.
        let part = update::fetch(&asset_url, &dest, Some(&db))?;
        // The new file keeps the link when it's another release of that entry; settled here,
        // outside the lock, and recorded before the swap so the updated file always has it.
        let mut link_problem = None;
        let carry = match old_link {
            Some(link) => match carried_link(&app, link, &asset_url) {
                Some(link) => Some(alias_record(link, part.path())?),
                None => {
                    link_problem = Some(LINK_NOT_KEPT.to_string());
                    None
                }
            },
            None => None,
        };
        // A finished download waits out a busy spell instead of being thrown away.
        let _write = library_write_within(PLACE_WAIT)?;
        let layout = update_recheck(&dest)?;
        // A relink temp the swap makes is recorded, so one a crash leaves can be swept (#394).
        let journal_conn = db::open(&db)?;
        let journal = scratch::DbJournal(&journal_conn);
        let mut policy = organize::LinkPolicy::new(&layout).with_journal(&journal);
        // Finish an earlier, interrupted update first (the user asked: every step is retried),
        // so it doesn't block this one; if it can't be finished, say exactly why.
        let mut dirs = update::layout_dirs(&layout);
        if let Some(parent) = dest.parent() {
            if !dirs.iter().any(|d| d == parent) {
                dirs.push(parent.to_path_buf());
            }
        }
        let settled = update::settle(&dirs, &layout.mods, &mut policy, update::Attempt::UserAsked);
        log_recovery(&settled);
        let old = update::old_path(&dest);
        if settled.leftovers.contains(&old) {
            return Err(update::leftover_message(&settled, &[old]));
        }
        github::verify_update_identity(part.path(), &dest)?;
        // Recovery may have put a linked file back at `dest`. No network under the lock: only
        // the same-release rule can keep that link.
        let carry = match (carry, &link_problem) {
            (None, None) => match current_link(&db, &dest)? {
                Some(link) if catalogalias::another_release_of(&link, &asset_url) => {
                    let link = catalogalias::Link {
                        download_url: Some(asset_url.clone()),
                        ..link
                    };
                    Some(alias_record(link, part.path())?)
                }
                Some(_) => {
                    link_problem = Some(LINK_NOT_KEPT.to_string());
                    None
                }
                None => None,
            },
            (carry, _) => carry,
        };
        // As for an install: a new record is written before the swap, but replacing another
        // entry's record for these bytes waits until the swap has succeeded.
        let mut record_after = None;
        let undo = match &carry {
            Some((link, sha, size)) => {
                let held = catalogalias::zip_tech_name(&dest)
                    .map(|t| catalogalias::recorded_entry(&db::open(&db)?, &t, sha))
                    .transpose()?
                    .flatten();
                if held.is_some_and(|id| id != link.catalog_id) {
                    record_after = Some((link, sha, *size));
                    None
                } else {
                    Some(remember_catalog_alias(&db, &dest, sha, *size, link)?)
                }
            }
            None => None,
        };
        let mut done = match update::swap(&dest, part.path(), &layout.mods, &mut policy) {
            Ok(done) => done,
            Err(e) => {
                // A swap can fail after the new file is in place (recovery then finishes it):
                // the record is undone only when `dest` provably doesn't hold the new bytes.
                if let Some((_, sha, _)) = &carry {
                    if new_bytes_absent(&dest, sha) {
                        undo_catalog_alias(&db, undo);
                    }
                }
                return Err(e);
            }
        };
        if let Some((link, sha, size)) = record_after {
            if let Err(e) = remember_catalog_alias(&db, &dest, sha, size, link) {
                log::warn!("[catalog] {e}");
                link_problem = Some(LINK_NOT_KEPT.to_string());
            }
        }
        done.problems.extend(link_problem);
        if let Some(flat) = &done.relinked {
            if let (Ok(conn), Some(name)) = (db::open(&db), flat.file_name()) {
                let key = modsroot::canonical_key(&layout.mods);
                organize::note_zip_projection(&conn, &key, &name.to_string_lossy(), flat);
            }
        }
        let label = file_label(&dest);
        match &done.backup {
            Some(b) => log::info!("updated {label}; previous version kept at {}", b.display()),
            None => log::info!("updated {label}; previous version not moved to backups"),
        }
        for p in &done.problems {
            log::warn!("{p}");
        }
        Ok(UpdateOutcome {
            backup: done.backup.map(|b| b.to_string_lossy().into_owned()),
            problems: done.problems,
        })
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(error::AppError::from)
}

/// Whether `dest` provably doesn't hold the bytes hashing to `sha`: no file there, or other
/// bytes. A file that can't be read proves nothing, so its record is kept (and logged).
fn new_bytes_absent(dest: &std::path::Path, sha: &str) -> bool {
    new_bytes_absent_with(dest, sha, provenance::sha256_file)
}

/// `new_bytes_absent`, with the hashing passed in (tests make it fail).
fn new_bytes_absent_with(
    dest: &std::path::Path,
    sha: &str,
    hash: impl Fn(&std::path::Path) -> Result<String, String>,
) -> bool {
    if let Err(e) = dest.symlink_metadata() {
        if e.kind() == std::io::ErrorKind::NotFound {
            return true;
        }
    }
    match hash(dest) {
        Ok(h) => !h.eq_ignore_ascii_case(sha),
        Err(e) => {
            log::warn!(
                "[catalog] couldn't read {} after a failed update, so its link is kept: {e}",
                file_label(dest)
            );
            false
        }
    }
}

/// The alias to record for the downloaded file `part` linked by `link`: its hash and size.
fn alias_record(
    link: catalogalias::Link,
    part: &std::path::Path,
) -> Result<(catalogalias::Link, String, u64), String> {
    let sha = provenance::sha256_file(part)?;
    let size = std::fs::metadata(part).map_err(|e| e.to_string())?.len();
    Ok((link, sha, size))
}

/// The catalog link of the file at `dest` now, when it has one (#386). An error when it can't
/// be checked.
fn current_link(
    db: &std::path::Path,
    dest: &std::path::Path,
) -> Result<Option<catalogalias::Link>, String> {
    let Some(tech_name) = catalogalias::zip_tech_name(dest) else {
        return Ok(None);
    };
    catalogalias::link_for_file(&db::open(db)?, &tech_name, dest)
}

/// What an update says when the file it replaced was linked to its catalog entry (#386) and
/// the new one can't be shown to come from that entry.
const LINK_NOT_KEPT: &str = "Silo couldn't confirm this update came from the mod's catalog \
     entry, so it may show as not in Silo's catalog. To link it again, remove it and install it \
     from Browse.";

/// The link for the file an update from `asset_url` writes over a file linked by `link`: kept
/// when the update is a release asset with the same file name from the same GitHub repository
/// the linked file came from, or a download the catalog entry lists right now. None when
/// neither holds, or when the catalog can't be asked for the second. (`download_update` has
/// already refused a release under another file name.)
fn carried_link(
    app: &tauri::AppHandle,
    link: catalogalias::Link,
    asset_url: &str,
) -> Option<catalogalias::Link> {
    let with_url = |link: catalogalias::Link| catalogalias::Link {
        download_url: Some(asset_url.to_string()),
        ..link
    };
    if catalogalias::another_release_of(&link, asset_url) {
        return Some(with_url(link));
    }
    let base = siloapi_base(app).ok()?;
    match siloapi::detail(&base, &link.catalog_id) {
        Ok(detail) if is_catalog_download(&detail, asset_url) => {
            Some(with_url(catalogalias::Link {
                catalog_tech_name: detail.tech_name.or(link.catalog_tech_name),
                ..link
            }))
        }
        Ok(_) => None,
        Err(e) => {
            log::warn!("[update] couldn't check the catalog entry: {e}");
            None
        }
    }
}

/// Whether `url` is a direct download the catalog entry `detail` lists.
fn is_catalog_download(detail: &siloapi::BrowseMod, url: &str) -> bool {
    detail
        .sources
        .iter()
        .any(|s| s.installable && s.download_url.as_deref() == Some(url))
}

fn log_recovery(r: &update::Recovery) {
    for n in &r.notices {
        log::info!("{n}");
    }
    for p in &r.problems {
        log::warn!("{p}");
    }
}

/// Finish or roll back mod updates a crash interrupted (`update::settle`), in the active mods
/// folder and each archive category folder, and return what happened for the user. Only when
/// the library lock is free right now (a scan never waits for it), no archive move is
/// unfinished (its files are in flux), and Silo isn't installing its own update.
fn recover_interrupted_updates() -> update::Recovery {
    if move_in_progress() || self_updating() {
        return update::Recovery::default();
    }
    let Some(active) = modsroot::active() else {
        return update::Recovery::default();
    };
    if active.move_to.is_some() {
        return update::Recovery::default();
    }
    // Counts as a write: it can change files, so a scan already under way (another window
    // focus) mustn't rewrite the manifest from what it saw before. The scan calling this takes
    // its own starting epoch afterwards.
    let Some(_write) = take_for_write(&LIBRARY_WRITE, &LIBRARY_EPOCH) else {
        return update::Recovery::default();
    };
    // Link probes a crash left behind (a probe running right now is younger than this).
    for dir in [&active.layout.mods, &active.layout.archive] {
        organize::log_stale_link_probes(dir, std::time::Duration::from_secs(600));
    }
    let r = settle_layout(&active.layout);
    log_recovery(&r);
    r
}

/// Silo's database, once the app has started: where background recovery records its scratch.
static APP_DB: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Background recovery over `layout` (costly steps at most once per session per file), with
/// the scratch journal in Silo's database when it can be opened.
fn settle_layout(layout: &organize::Layout) -> update::Recovery {
    let conn = APP_DB.get().and_then(|p| match db::open(p) {
        Ok(c) => Some(c),
        Err(e) => {
            log::warn!("couldn't open the scratch journal: {e}");
            None
        }
    });
    let journal = conn.as_ref().map(scratch::DbJournal);
    settle_layout_with(layout, journal.as_ref().map(|j| j as &dyn scratch::Journal))
}

/// [`settle_layout`] with its journal given (tests). With one, relink temps an earlier
/// recovery, update or relink left in the mods folder (a crash) are swept first (only recorded
/// ones, each proven: `scratch::sweep_where`), and the relinks this recovery makes record theirs
/// (#400). Without one nothing it makes is recorded, so nothing it leaves is ever removed.
fn settle_layout_with(
    layout: &organize::Layout,
    journal: Option<&dyn scratch::Journal>,
) -> update::Recovery {
    let mut policy = organize::LinkPolicy::new(layout);
    if let Some(j) = journal {
        scratch::sweep_where(j, &|e| {
            e.role == scratch::Role::RelinkTmp && scratch::inside(&e.path, &layout.mods)
        });
        policy = policy.with_journal(j);
    }
    update::settle(
        &update::layout_dirs(layout),
        &layout.mods,
        &mut policy,
        update::Attempt::Background,
    )
}

/// Resolve update leftovers in the active layout before a library write (call with the library
/// lock held). Refuses while a mod's previous version can't be put back, since the library is
/// short that mod until it is.
fn settle_updates_locked() -> Result<(), String> {
    let Some(active) = modsroot::active() else {
        return Ok(());
    };
    if active.move_to.is_some() {
        return Ok(());
    }
    let r = settle_layout(&active.layout);
    log_recovery(&r);
    if r.stranded.is_empty() {
        Ok(())
    } else {
        Err(update::leftover_message(&r, &r.stranded))
    }
}

// ── SiloAPI (mod browser / discovery) ──

/// The catalog address: the built-in one, or, in a debug build only, one a developer stored with
/// `siloapi_set_base`. Release builds ignore any stored value.
fn effective_siloapi_base(stored: Option<String>, debug: bool) -> String {
    stored
        .filter(|s| debug && !s.trim().is_empty())
        .unwrap_or_else(|| siloapi::DEFAULT_BASE.to_string())
}

fn siloapi_base(app: &tauri::AppHandle) -> Result<String, String> {
    let conn = db::open(&db_path(app)?)?;
    Ok(effective_siloapi_base(
        db::get_app_setting(&conn, "siloapi_base"),
        cfg!(debug_assertions),
    ))
}

#[tauri::command]
async fn siloapi_status(app: tauri::AppHandle) -> Result<String, String> {
    off_main(move || siloapi_base(&app)).await
}

/// Developer-only: point the catalog at another address (e.g. a local SiloAPI). Debug builds
/// only; the catalog decides where Browse and Collections installs download from.
#[cfg(debug_assertions)]
#[tauri::command]
async fn siloapi_set_base(app: tauri::AppHandle, base: String) -> Result<(), String> {
    let db = db_path(&app)?;
    off_main(move || {
        let conn = db::open(&db)?;
        let v = base.trim().trim_end_matches('/');
        // SSRF guard: a custom base drives every catalog request, so validate it before storing.
        // Allow localhost so a developer can point at their own endpoint; block private IPs.
        if !v.is_empty() {
            net::validate_outbound_url(v, true)?;
        }
        db::set_app_setting(
            &conn,
            "siloapi_base",
            if v.is_empty() { None } else { Some(v) },
        )
    })
    .await
}

/// Release builds always use the built-in catalog; same signature so `generate_handler!` stays
/// one list (the argument names are the IPC keys, so they keep their names).
#[cfg(not(debug_assertions))]
#[tauri::command]
async fn siloapi_set_base(app: tauri::AppHandle, base: String) -> Result<(), String> {
    let _ = (app, base);
    Err("Not available in this build.".into())
}

// Mirrors siloapi::browse's filter set; flat args keep the invoke() call shape simple.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
async fn browse_mods(
    app: tauri::AppHandle,
    query: Option<String>,
    category: Option<String>,
    sort: Option<String>,
    tags: Option<Vec<String>>,
    available_by: Option<u32>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<siloapi::BrowsePage, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let base = siloapi_base(&app)?;
        siloapi::browse(
            &base,
            query.as_deref(),
            category.as_deref(),
            sort.as_deref(),
            &tags.unwrap_or_default(),
            available_by,
            limit.unwrap_or(40),
            offset.unwrap_or(0),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The available Browse facets (brand/theme/region/realism/era + counts), for the filter chips.
#[tauri::command]
async fn siloapi_facets(app: tauri::AppHandle) -> Result<siloapi::Facets, String> {
    off_main(move || siloapi::facets(&siloapi_base(&app)?)).await
}

/// One mod's full catalog record (metadata + every source it was seen on).
#[tauri::command]
async fn siloapi_mod_detail(
    app: tauri::AppHandle,
    id: String,
) -> Result<siloapi::BrowseMod, String> {
    off_main(move || siloapi::detail(&siloapi_base(&app)?, &id)).await
}

/// Resolve a *library* mod (by tech name) to its full catalog record, so the library detail
/// drawer can show the same info Browse does — summary, sources, latest version. Returns None
/// when the mod isn't catalogued (dev/obscure mods), in which case the drawer just shows less.
/// `path` is the mod's file: a zip Silo installed from a catalog entry under another name is
/// that entry's (#386).
#[tauri::command]
async fn catalog_detail_by_tech(
    app: tauri::AppHandle,
    tech_name: String,
    path: Option<String>,
) -> Result<Option<siloapi::BrowseMod>, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<Option<siloapi::BrowseMod>, String> {
        let base = siloapi_base(&app)?;
        let conn = alias_db(&app)?;
        if let Some(link) = file_catalog_link(&conn, &tech_name, path.as_deref())? {
            return siloapi::detail(&base, &link.catalog_id).map(Some);
        }
        let hits = siloapi::lookup(&base, std::slice::from_ref(&tech_name))?;
        match hits.into_iter().find(|h| !h.id.is_empty()) {
            Some(h) => siloapi::detail(&base, &h.id).map(Some),
            None => Ok(None),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Fetch a catalog mod thumbnail hosted on GitHub as a `data:` URL (the CSP keeps the webview
/// off arbitrary image hosts). Cached on disk by URL so each image is fetched at most once.
/// ModHub thumbnails are never fetched from GIANTS' CDN here: they come from SiloAPI's cache.
#[tauri::command]
async fn catalog_image(app: tauri::AppHandle, url: String) -> Result<String, String> {
    // The profile's cache when one is active, the same folder the startup cleanup reads.
    let cache_dir = profile::app_cache_dir(&app)
        .ok()
        .map(|d| d.join(CATALOG_IMAGES_DIR));
    tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        if let Some(dir) = &cache_dir {
            let key = format!("{:x}", md5::compute(url.as_bytes()));
            let file = dir.join(&key);
            if let Some(cached) = read_cached_image(&file) {
                return Ok(cached);
            }
            let data = siloapi::fetch_image(&url)?;
            cache_image(&file, &data);
            Ok(data)
        } else {
            siloapi::fetch_image(&url)
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Where catalog thumbnails are cached, under the app cache dir. `-v2`: only entries that decoded
/// before they were written (`cacheable_image`) live here; the unchecked `catalog_images` of
/// earlier versions is removed at startup.
const CATALOG_IMAGES_DIR: &str = "catalog_images_v2";

/// Smallest decoded thumbnail the cache keeps (or accepts back): anything shorter is an empty or
/// failed fetch, not an image.
const MIN_CACHED_IMAGE_BYTES: usize = 16;

/// Pure: the image bytes of a thumbnail `data:` URL (`data:image/...;base64,...`), when it is one
/// and isn't trivially short.
fn thumbnail_bytes(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    let rest = s.strip_prefix("data:image/")?;
    let (_, payload) = rest.split_once(";base64,")?;
    base64::engine::general_purpose::STANDARD
        .decode(payload)
        .ok()
        .filter(|b| b.len() >= MIN_CACHED_IMAGE_BYTES)
}

/// Pure: the format of image bytes that end the way a whole file of it ends (PNG's `IEND`
/// chunk, JPEG's end-of-image marker) and whose header reads (a cut right after an embedded EXIF
/// thumbnail's own end marker leaves a header that doesn't). `None` for anything else (GIF, SVG,
/// WebP, ...): shown, but never cached, since a cut-off copy couldn't be told apart.
fn whole_image_format(b: &[u8]) -> Option<image::ImageFormat> {
    const PNG_END: [u8; 8] = [0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82];
    let format = if b.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        b.ends_with(&PNG_END).then_some(image::ImageFormat::Png)
    } else if b.starts_with(&[0xFF, 0xD8]) {
        b.ends_with(&[0xFF, 0xD9])
            .then_some(image::ImageFormat::Jpeg)
    } else {
        None
    }?;
    image::ImageReader::with_format(std::io::Cursor::new(b), format)
        .into_dimensions()
        .ok()
        .map(|_| format)
}

/// Pure: whether `s` is a usable cached thumbnail: its bytes are a whole PNG or JPEG
/// (`whole_image_format`). Cheap enough for every read: a cut-off write is all an entry written
/// by this cache can suffer, since it decoded before it was kept (`cacheable_image`).
fn cached_image_ok(s: &str) -> bool {
    thumbnail_bytes(s).is_some_and(|b| whole_image_format(&b).is_some())
}

/// Pure: whether a fetched thumbnail is worth caching: whole (`whole_image_format`) and it
/// decodes, so a damaged image the server sent is shown but never kept.
fn cacheable_image(s: &str) -> bool {
    thumbnail_bytes(s).is_some_and(|b| {
        whole_image_format(&b).is_some_and(|f| image::load_from_memory_with_format(&b, f).is_ok())
    })
}

/// The cached thumbnail at `file`, or `None` when there is none or it isn't usable
/// (`cached_image_ok`). An unusable one is left for the fetch that follows to replace (deleting
/// it here could delete a good copy another request just put in its place).
fn read_cached_image(file: &std::path::Path) -> Option<String> {
    let cached = std::fs::read_to_string(file).ok()?;
    if cached_image_ok(&cached) {
        return Some(cached);
    }
    log::info!("refetching an unusable cached thumbnail {}", file.display());
    None
}

/// Tells concurrent thumbnail cache writes in this process apart.
static IMAGE_CACHE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Cache thumbnail `data` at `file`, best effort: only an image worth keeping (`cacheable_image`),
/// written to a temp file of its own beside `file` and renamed into place (replacing an unusable
/// entry), so a crash or a concurrent fetch of the same image never leaves a cut-off entry.
fn cache_image(file: &std::path::Path, data: &str) {
    if !cacheable_image(data) {
        return;
    }
    let (Some(dir), Some(name)) = (file.parent(), file.file_name()) else {
        return;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let seq = IMAGE_CACHE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!(
        "{}.{}-{seq}.tmp",
        name.to_string_lossy(),
        std::process::id()
    ));
    let placed = std::fs::write(&tmp, data).and_then(|()| std::fs::rename(&tmp, file));
    if placed.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Catalog categories with counts, for the Browse filter.
#[tauri::command]
async fn siloapi_categories(app: tauri::AppHandle) -> Result<Vec<siloapi::CategoryCount>, String> {
    off_main(move || siloapi::categories(&siloapi_base(&app)?)).await
}

#[tauri::command]
async fn siloapi_stats(app: tauri::AppHandle) -> Result<siloapi::Stats, String> {
    off_main(move || siloapi::stats(&siloapi_base(&app)?)).await
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct InstallProgress {
    id: String,
    done: u64,
    total: Option<u64>,
}

/// Download a browsed mod's .zip into the library root and return the filename.
/// Streams the download, emitting `install:progress` events per mod id so the UI can
/// show a bar. The frontend rescans (and auto-files) afterwards.
///
/// The download runs before the library lock is taken (so a slow mod never blocks other
/// changes); the lock is held only to place the finished file.
#[tauri::command]
async fn install_remote_mod(
    app: tauri::AppHandle,
    id: String,
    source: Option<String>,
    root: Option<String>,
) -> Result<String, error::AppError> {
    ensure_no_move()?;
    let emitter = app.clone();
    tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        let base = siloapi_base(&app)?;
        let root = primary_root(root)?;
        let resolved = siloapi::resolve_download(&base, &id, source.as_deref())?;
        // The filename comes from the catalog — validate it's a plain basename so a
        // hostile response can't redirect the write outside the mods folder.
        paths::safe_file_name(&resolved.filename)?;
        // The user picked this exact mod, so a file named differently from the catalog's
        // techName still installs, under its own name (never renamed; the game names the mod
        // after the zip). Only noted in the log. A shared set refuses it instead.
        if let Some(note) = browse_name_note(&resolved.filename, resolved.tech_name.as_deref()) {
            log::info!("{note}");
        }
        // Such a file is joined to its catalog entry through a recorded alias instead (#386).
        let alias = catalogalias::needs_alias(&resolved.filename, resolved.tech_name.as_deref());
        let dest = root.join(&resolved.filename);
        if path_taken(&dest) {
            return Err(already_in_library(&dest));
        }
        let db = db_path(&app)?;
        let part = claim_part(&dest, Some(&db))?;
        siloapi::download_to_part(
            &resolved.url,
            &part,
            resolved.expected_sha256.as_deref(),
            |done, total| {
                let _ = emitter.emit(
                    "install:progress",
                    InstallProgress {
                        id: id.clone(),
                        done,
                        total,
                    },
                );
            },
        )?;
        // What this install records about the file's catalog entry (#386): an alias for a
        // differently named file, or, for a file named after its entry, dropping an alias an
        // earlier install left for the same name and bytes. Recorded before the file is placed,
        // so a placed file never has a missing or stale link; put back as it was if the file
        // then isn't placed. A download the catalog has a hash for was already checked against it.
        let stem = catalogalias::zip_tech_name(&dest);
        let hash_part = || match &resolved.expected_sha256 {
            Some(h) => Ok(h.clone()),
            None => provenance::sha256_file(part.path()),
        };
        // The alias's hash and size are taken before the lock.
        let alias = if alias {
            let size = std::fs::metadata(part.path())
                .map_err(|e| e.to_string())?
                .len();
            let link = catalogalias::Link {
                catalog_id: id.clone(),
                catalog_tech_name: resolved.tech_name.clone(),
                download_url: Some(resolved.url.clone()),
            };
            Some((link, hash_part()?, size))
        } else {
            None
        };
        // A finished download waits out a busy spell instead of being thrown away.
        let _write = library_write_within(PLACE_WAIT)?;
        install_recheck(&root, &dest)?;
        // A new record is written before the file is placed (a placed file never lacks it).
        // A change to a record another entry already holds for these bytes, or dropping one,
        // waits until the file is placed: an install that never places its file must not move
        // copies already in other folders to another entry, even if Silo stops in between.
        let mut after = None;
        let undo = match (&alias, stem.as_deref()) {
            (Some((link, sha, size)), Some(stem)) => {
                let held = catalogalias::recorded_entry(&db::open(&db)?, stem, sha)?;
                if held.is_some_and(|id| id != link.catalog_id) {
                    after = Some(AfterPlace::Record(link.clone(), sha.clone(), *size));
                    None
                } else {
                    Some(remember_catalog_alias(&db, &dest, sha, *size, link)?)
                }
            }
            // Asked under the lock, so an install placing the same name meanwhile is seen. The
            // hash is only taken (under the lock) in that rare case.
            (None, Some(stem)) if catalogalias::has_any(&db::open(&db)?, stem)? => {
                after = Some(AfterPlace::Forget(stem.to_string(), hash_part()?));
                None
            }
            _ => None,
        };
        if let Err(e) = place_part(part.path(), &dest) {
            undo_catalog_alias(&db, undo);
            return Err(e);
        }
        // The file is in place; a failure here only leaves the earlier record (logged).
        let done = match after {
            Some(AfterPlace::Record(link, sha, size)) => {
                remember_catalog_alias(&db, &dest, &sha, size, &link).map(drop)
            }
            Some(AfterPlace::Forget(stem, sha)) => forget_catalog_alias(&db, &stem, &sha).map(drop),
            None => Ok(()),
        };
        if let Err(e) = done {
            log::warn!("[catalog] {e}");
        }
        Ok(resolved.filename)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(error::AppError::from)
}

/// The log line for a Browse install whose file isn't named after the catalog's techName, or
/// `None` when it is (or the catalog has no techName). The install goes ahead either way.
fn browse_name_note(filename: &str, tech_name: Option<&str>) -> Option<String> {
    let tech = tech_name.filter(|t| !t.trim().is_empty())?;
    siloapi::check_download_name(filename, tech).err()?;
    Some(format!(
        "[install] catalog techName {tech} downloads as {filename}; installed under that name"
    ))
}

/// Record that the zip Silo is about to place at `dest` (whole-file SHA-256 `sha`, `size`
/// bytes) came from the catalog entry `link` (#386). A failure stops the install or update
/// before the file is placed, so a placed file never lacks its link. The `Undo` puts the record
/// back if the file then isn't placed.
fn remember_catalog_alias(
    db: &std::path::Path,
    dest: &std::path::Path,
    sha: &str,
    size: u64,
    link: &catalogalias::Link,
) -> Result<catalogalias::Undo, String> {
    let label = file_label(dest);
    let saved = catalogalias::zip_tech_name(dest)
        .ok_or_else(|| format!("{label} isn't a zip"))
        .and_then(|tech| catalogalias::record(&db::open(db)?, &tech, sha, size, link));
    match saved {
        Ok(undo) => {
            let entry = link
                .catalog_tech_name
                .as_deref()
                .unwrap_or(&link.catalog_id);
            log::info!("[catalog] {label} linked to catalog entry {entry}");
            Ok(undo)
        }
        Err(e) => Err(format!(
            "Silo couldn't note which catalog entry {label} came from, so it didn't add it. \
             Try again. ({e})"
        )),
    }
}

/// A change to the catalog alias that `install_remote_mod` makes only once the file is placed.
enum AfterPlace {
    /// Record this link for (hash, size), replacing another entry's record for those bytes.
    Record(catalogalias::Link, String, u64),
    /// Drop the alias for (techName, hash).
    Forget(String, String),
}

/// Drop the alias for the zip `tech_name` with whole-file SHA-256 `sha` (#386): the file is
/// being installed from an entry with its own name. A failure stops the install.
fn forget_catalog_alias(
    db: &std::path::Path,
    tech_name: &str,
    sha: &str,
) -> Result<catalogalias::Undo, String> {
    catalogalias::forget(&db::open(db)?, tech_name, sha).map_err(|e| {
        format!("Silo couldn't update which catalog entry {tech_name} came from, so it didn't add it. Try again. ({e})")
    })
}

/// Put a catalog alias back as it was when the file it was changed for wasn't placed after all.
/// Best effort: logged when it can't be.
fn undo_catalog_alias(db: &std::path::Path, undo: Option<catalogalias::Undo>) {
    let Some(undo) = undo else {
        return;
    };
    if let Err(e) = db::open(db).and_then(|conn| undo.apply(&conn)) {
        log::warn!("[catalog] couldn't put a catalog link back: {e}");
    }
}

/// The database, for reading catalog aliases (#386).
fn alias_db(app: &tauri::AppHandle) -> Result<rusqlite::Connection, String> {
    db_path(app).and_then(|db| db::open(&db))
}

/// The catalog entry Silo installed the library zip at `path` (known as `tech_name`) from,
/// when it recorded one and the file is still those bytes (`catalogalias::link_for_file`).
/// An error when that can't be checked (the record or the file can't be read).
fn file_catalog_link(
    conn: &rusqlite::Connection,
    tech_name: &str,
    path: Option<&str>,
) -> Result<Option<catalogalias::Link>, String> {
    match path {
        Some(p) => catalogalias::link_for_file(conn, tech_name, std::path::Path::new(p)),
        None => Ok(None),
    }
}

const MODS_FOLDER_CHANGED: &str =
    "Your mods folder changed while this downloaded, so Silo didn't add it. Install it again.";

/// What `install_remote_mod` checks again once it holds the library lock: the user may have
/// switched mods folder while the download ran, and the mod must not land in the old one.
fn install_recheck(root: &std::path::Path, dest: &std::path::Path) -> Result<(), String> {
    ensure_library_write(dest)?;
    still_a_mods_folder(root, &mods_roots())
}

/// `Ok` while `root` is still one of the current mods folders.
fn still_a_mods_folder(root: &std::path::Path, mods_roots: &[PathBuf]) -> Result<(), String> {
    if mods_roots.iter().any(|m| modsroot::same_folder(m, root)) {
        Ok(())
    } else {
        Err(MODS_FOLDER_CHANGED.to_string())
    }
}

/// Downloads running now. Each claims its mod file's part name (`key`, by `same_file_key`):
/// downloads run outside the library lock, so two at once for the same file are refused. And
/// each holds the part file it streams into (`path`, its own name), which no sweep removes
/// while it's claimed (`remove_unclaimed_part`).
static PARTS_IN_FLIGHT: std::sync::Mutex<Vec<InFlight>> = std::sync::Mutex::new(Vec::new());

struct InFlight {
    key: FileKey,
    path: FileKey,
}

/// A download's claim on its part file (#400): a name of its own beside the mod
/// (`<file>.<token><suffix>`, `scratch::unique_beside`), recorded in the scratch journal
/// (`scratch::Role::Part`) before it's made, and made only by [`PartClaim::create`], which never
/// opens anything already there and then records which file it made (`scratch::Made`).
/// Dropping the claim removes the part it made (a failed or abandoned download) while that
/// very file is still there, frees the name, and drops the record unless that file is still
/// there. A part a crash leaves stays recorded, for `scratch::sweep_where`.
pub struct PartClaim {
    path: PathBuf,
    /// The mod file it downloads.
    dest: PathBuf,
    key: FileKey,
    /// The database whose journal recorded the part (none in tests that don't need one).
    db: Option<PathBuf>,
    /// The file [`PartClaim::create`] made.
    made: std::sync::OnceLock<scratch::Made>,
    /// What its [`PartFile`] last recorded it may hold (as in the journal: before the chunk
    /// being written, then after it).
    written: std::sync::Mutex<Vec<scratch::Written>>,
}

impl PartClaim {
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Make the part file: a new file at the claim's own name, never one already there (a
    /// clash fails; nothing is removed or written into). Then note which file it is, in the
    /// claim and in the journal, so only that file is ever removed. Every byte goes in through
    /// the returned [`PartFile`].
    pub fn create(&self) -> std::io::Result<PartFile<'_>> {
        if self.made.get().is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "the part was already made",
            ));
        }
        let f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.path)?;
        use sha2::Digest;
        let empty = scratch::Written {
            len: 0,
            sha256: format!("{:x}", sha2::Sha256::new().finalize()),
        };
        let mut out = PartFile {
            claim: self,
            file: f,
            conn: None,
            hasher: sha2::Sha256::new(),
            buf: Vec::with_capacity(PART_CHUNK),
            done: empty.clone(),
        };
        let Some(made) = scratch::Made::of_file(&out.file) else {
            // Nothing proves which file this is: it's never removed (logged when left).
            log::warn!("couldn't read which file {} is", self.path.display());
            return Ok(out);
        };
        let _ = self.made.set(made);
        *self.written.lock().unwrap_or_else(|e| e.into_inner()) = vec![empty.clone()];
        if let Some(db) = &self.db {
            match db::open(db) {
                Ok(conn) => out.conn = Some(conn),
                // The bytes can't be recorded: nothing is written (the download fails).
                Err(e) => return Err(std::io::Error::other(e)),
            }
        }
        out.note(std::slice::from_ref(&empty))?;
        Ok(out)
    }

    /// The very file this claim made is still at its name, holding exactly what its
    /// [`PartFile`] wrote (`scratch::Made::holds_written`).
    fn holds_its_part(&self) -> bool {
        let written = self
            .written
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        match self.made.get() {
            Some(m) => scratch::Made {
                written,
                ..m.clone()
            }
            .holds_written(&self.path),
            None => false,
        }
    }

    /// Stop as a crash would: the claim goes, nothing is removed or forgotten (tests).
    #[cfg(test)]
    pub(crate) fn crash(self) {
        PARTS_IN_FLIGHT
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|c| c.key != self.key);
        std::mem::forget(self);
    }
}

/// How many bytes a download writes at a time; each write is recorded first (`PartFile`).
#[cfg(not(test))]
const PART_CHUNK: usize = 4 << 20;
#[cfg(test)]
const PART_CHUNK: usize = 8;

/// A download's part file (#400), made by [`PartClaim::create`]. Bytes are written in chunks of
/// [`PART_CHUNK`]. Before each chunk goes in, the length and SHA-256 the part will have after it
/// are recorded in the journal beside the ones it has now (`scratch::Written`), and a record
/// that fails stops the download before the chunk is written. So a part a crash leaves holds
/// only what Silo wrote when its length is one of those two and its bytes hash to it, or (a
/// crash inside the write) its length lies between them and its first bytes hash to the one
/// before (`scratch::Made::holds_written`). Anything else is left in place.
pub struct PartFile<'c> {
    claim: &'c PartClaim,
    file: std::fs::File,
    /// The journal's database (none without one: nothing recorded, tests).
    conn: Option<rusqlite::Connection>,
    /// The SHA-256 of every byte written so far.
    hasher: sha2::Sha256,
    buf: Vec<u8>,
    done: scratch::Written,
}

impl PartFile<'_> {
    /// Record `written` as what the part may hold.
    fn note(&self, written: &[scratch::Written]) -> std::io::Result<()> {
        let (Some(conn), Some(made)) = (&self.conn, self.claim.made.get()) else {
            return Ok(());
        };
        let made = scratch::Made {
            written: written.to_vec(),
            ..made.clone()
        };
        scratch::Journal::record(
            &scratch::DbJournal(conn),
            &part_entry(&self.claim.path, &self.claim.dest, Some(made)),
        )
        .map_err(std::io::Error::other)
    }

    /// Write the buffered bytes, recorded first.
    fn write_chunk(&mut self) -> std::io::Result<()> {
        use sha2::Digest;
        use std::io::Write;
        if self.buf.is_empty() {
            return Ok(());
        }
        let mut next = self.hasher.clone();
        next.update(&self.buf);
        let after = scratch::Written {
            len: self.done.len + self.buf.len() as u64,
            sha256: format!("{:x}", next.clone().finalize()),
        };
        let written = vec![self.done.clone(), after.clone()];
        self.note(&written)?;
        *self.claim.written.lock().unwrap_or_else(|e| e.into_inner()) = written;
        self.file.write_all(&self.buf)?;
        self.buf.clear();
        self.hasher = next;
        self.done = after;
        Ok(())
    }

    /// Write what's buffered and flush it to disk.
    pub fn sync_all(&mut self) -> std::io::Result<()> {
        std::io::Write::flush(self)?;
        self.file.sync_all()
    }
}

impl std::io::Write for PartFile<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let room = PART_CHUNK.saturating_sub(self.buf.len()).max(1);
        let n = bytes.len().min(room);
        self.buf.extend_from_slice(&bytes[..n]);
        if self.buf.len() >= PART_CHUNK {
            self.write_chunk()?;
        }
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.write_chunk()?;
        self.file.flush()
    }
}

impl Drop for PartFile<'_> {
    fn drop(&mut self) {
        // Bytes still buffered go in (recorded first) so the part is what the download got.
        if let Err(e) = self.write_chunk() {
            log::warn!("couldn't finish writing {}: {e}", self.claim.path.display());
        }
    }
}

/// The journal entry for a download's part at `path`, of the mod file `dest`.
fn part_entry(
    path: &std::path::Path,
    dest: &std::path::Path,
    made: Option<scratch::Made>,
) -> scratch::Entry {
    scratch::Entry {
        path: path.to_path_buf(),
        role: scratch::Role::Part,
        from: dest.to_path_buf(),
        to: dest.to_path_buf(),
        kind: "zip".into(),
        made,
    }
}

impl Drop for PartClaim {
    fn drop(&mut self) {
        // Only the very file this claim made, holding only what it wrote (never through a
        // link, never another file put at its name since, never one changed since).
        if self.holds_its_part() {
            if let Err(e) = std::fs::remove_file(&self.path) {
                log::warn!("couldn't remove {}: {e}", self.path.display());
            }
        } else if self.path.symlink_metadata().is_ok() {
            log::warn!(
                "left {} in place: it isn't the file Silo's download made, as it made it",
                self.path.display()
            );
        }
        {
            let mut held = PARTS_IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner());
            held.retain(|c| c.key != self.key);
        }
        if let Some(db) = &self.db {
            // Kept recorded only while its own file is still there (its removal failed), for a
            // later sweep. Placed, removed, never made, or another file there: the record goes,
            // so it never stands for anything else.
            if !self.holds_its_part() {
                match db::open(db) {
                    Ok(conn) => scratch::Journal::forget(&scratch::DbJournal(&conn), &self.path),
                    Err(e) => log::warn!("couldn't update the scratch journal: {e}"),
                }
            }
        }
    }
}

/// Two ways of comparing a path that names a file, so that two paths that may be one file on
/// some system are seen as the same: they [`overlap`](FileKey::overlaps) when either way agrees.
/// - `folded`: the name decomposed, lowercased, uppercased and recomposed (NFC). This merges what
///   macOS (APFS, HFS+) merges, case and how an accent is encoded (`é` vs `e` + combining
///   accent), and more (`σ`/`ς`, `k`/Kelvin sign).
/// - `upper`: the name uppercased character by character with nothing reordered, as NTFS
///   compares names (normalizing first can reorder combining marks NTFS would not).
///
/// Over-matching only over-claims (two such variants can't download at once, also on a
/// case-sensitive Linux folder), which is harmless. The parent is resolved (`canonicalize`:
/// real case on Windows, links followed), so two spellings of one folder share keys; a folder
/// that can't be resolved is kept as given.
#[derive(Debug, Clone, PartialEq)]
struct FileKey {
    folded: PathBuf,
    upper: PathBuf,
}

impl FileKey {
    fn overlaps(&self, other: &FileKey) -> bool {
        self.folded == other.folded || self.upper == other.upper
    }
}

/// See [`FileKey`].
fn same_file_key(p: &std::path::Path) -> FileKey {
    let Some(name) = p.file_name() else {
        return FileKey {
            folded: p.to_path_buf(),
            upper: p.to_path_buf(),
        };
    };
    let name = name.to_string_lossy();
    let parent = match p.parent() {
        Some(parent) => std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf()),
        None => PathBuf::new(),
    };
    FileKey {
        folded: parent.join(fold_file_name(&name)),
        upper: parent.join(name.to_uppercase()),
    }
}

/// See [`FileKey`].
pub(crate) fn fold_file_name(name: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    let decomposed: String = name.nfd().collect();
    decomposed.to_lowercase().to_uppercase().nfc().collect()
}

/// Why a part file couldn't be claimed.
#[derive(Debug)]
pub enum ClaimError {
    /// Another download of the same file is running.
    Busy(String),
    /// The scratch journal couldn't record the part, so it was never made.
    Journal(String),
}

impl From<ClaimError> for String {
    fn from(e: ClaimError) -> String {
        match e {
            ClaimError::Busy(m) | ClaimError::Journal(m) => m,
        }
    }
}

/// Suffix of an install's part file (`<file>.<token>.part`). `scan` never lists it (not a zip).
const INSTALL_PART_SUFFIX: &str = ".part";

/// Claim the part file an install of `dest` streams into, refusing while another download of
/// the same file is running. `db`: the database whose scratch journal records it.
fn claim_part(
    dest: &std::path::Path,
    db: Option<&std::path::Path>,
) -> Result<PartClaim, ClaimError> {
    claim_part_at(
        dest,
        &dest.with_extension("zip.part"),
        INSTALL_PART_SUFFIX,
        db,
    )
}

/// Claim a part file for a download of `dest` (see [`PartClaim`]): `name` is what two downloads
/// of one file are told apart by, `suffix` ends the part's own name. The claim is held before
/// the part is recorded, so a sweep never sees a recorded part of a running download
/// unclaimed. A journal that can't record stops the download before it starts.
pub(crate) fn claim_part_at(
    dest: &std::path::Path,
    name: &std::path::Path,
    suffix: &str,
    db: Option<&std::path::Path>,
) -> Result<PartClaim, ClaimError> {
    let key = same_file_key(name);
    let path = scratch::unique_beside(dest, suffix);
    {
        let mut held = PARTS_IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner());
        if held.iter().any(|c| c.key.overlaps(&key)) {
            return Err(ClaimError::Busy(format!(
                "{} is already downloading",
                file_label(dest)
            )));
        }
        held.push(InFlight {
            key: key.clone(),
            path: same_file_key(&path),
        });
    }
    let claim = PartClaim {
        path,
        dest: dest.to_path_buf(),
        key,
        db: db.map(std::path::Path::to_path_buf),
        made: std::sync::OnceLock::new(),
        written: std::sync::Mutex::new(Vec::new()),
    };
    if let Some(db) = db {
        let journal_err = |e: String| {
            ClaimError::Journal(format!(
                "Silo couldn't note the download of {} in its database, so it didn't start: {e}",
                file_label(dest)
            ))
        };
        let conn = db::open(db).map_err(journal_err)?;
        scratch::Journal::record(
            &scratch::DbJournal(&conn),
            &part_entry(&claim.path, dest, None),
        )
        .map_err(journal_err)?;
    }
    Ok(claim)
}

/// Whether a download running now holds the part file `path`.
pub(crate) fn part_in_flight(path: &std::path::Path) -> bool {
    let key = same_file_key(path);
    PARTS_IN_FLIGHT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .any(|c| c.path.overlaps(&key))
}

/// What [`remove_unclaimed_part`] did.
pub(crate) enum PartSweep {
    /// It was the part Silo made and nothing claims it: removed (or the removal's error).
    Removed(std::io::Result<()>),
    /// A download running now claims it: left alone.
    Claimed,
    /// Not the file Silo made there (another file, a link, a folder, or unreadable): left.
    NotIt,
}

/// Remove a recorded part file (`scratch::Role::Part`) no download running now claims, while it
/// is the very file Silo made there (`made`, read without following a link) holding exactly
/// what Silo wrote (`scratch::Made::holds_written`). The claim list
/// stays locked across the check and the removal, so a download can't claim it in between.
pub(crate) fn remove_unclaimed_part(path: &std::path::Path, made: &scratch::Made) -> PartSweep {
    let held = PARTS_IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner());
    let key = same_file_key(path);
    if held
        .iter()
        .any(|c| c.path.overlaps(&key) || c.key.overlaps(&key))
    {
        return PartSweep::Claimed;
    }
    if !made.holds_written(path) {
        return PartSweep::NotIt;
    }
    PartSweep::Removed(std::fs::remove_file(path))
}

/// Anything at `p`, including a broken link (which `Path::exists` would call absent).
fn path_taken(p: &std::path::Path) -> bool {
    std::fs::symlink_metadata(p).is_ok()
}

fn file_label(p: &std::path::Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

fn already_in_library(dest: &std::path::Path) -> String {
    format!("{} is already in your library", file_label(dest))
}

const DOWNLOAD_GONE: &str = "The download disappeared before Silo could add it. Try again.";

/// Move a finished download into the library at `dest`, never replacing anything there.
/// Hardlink then drop the part (the link fails if `dest` exists). Only when the link fails
/// because the filesystem can't hardlink (`no_hardlink_support`, e.g. a cloud-synced folder)
/// does it fall back to `paths::rename_no_replace`, a rename that fails rather than replace
/// `dest`. A taken `dest` refuses, and leaves the part to its claim (`PartClaim`), which removes
/// it only while it's still the file the download made (#400).
///
/// Call under the library lock, which keeps Silo's own writes out.
fn place_part(part: &std::path::Path, dest: &std::path::Path) -> Result<(), String> {
    place_part_with(part, dest, |a, b| std::fs::hard_link(a, b))?;
    fslog::moved("install", part, dest);
    Ok(())
}

fn place_part_with(
    part: &std::path::Path,
    dest: &std::path::Path,
    link: impl Fn(&std::path::Path, &std::path::Path) -> std::io::Result<()>,
) -> Result<(), String> {
    let refuse = || Err(already_in_library(dest));
    let io_err = |e: std::io::Error| {
        if e.kind() == std::io::ErrorKind::NotFound {
            DOWNLOAD_GONE.to_string()
        } else {
            e.to_string()
        }
    };
    match link(part, dest) {
        Ok(()) => {
            let _ = std::fs::remove_file(part);
            return Ok(());
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return refuse(),
        Err(e) if no_hardlink_support(&e) => {} // no-replace rename below
        Err(e) => return Err(io_err(e)),
    }
    match paths::rename_no_replace(part, dest) {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => refuse(),
        other => other.map_err(io_err),
    }
}

/// A hardlink error that means "this filesystem / pair of folders can't hardlink", as
/// opposed to a real failure (permissions on Windows, a missing part, a full disk):
/// - `Unsupported` on any platform;
/// - Windows: 1 ERROR_INVALID_FUNCTION (FAT/exFAT), 17 ERROR_NOT_SAME_DEVICE,
///   50 ERROR_NOT_SUPPORTED, 396 (cloud-synced folders such as OneDrive / Proton Drive);
/// - Unix: EPERM (filesystems without links, e.g. FAT on Linux), EXDEV (cross-device),
///   ENOTSUP / EOPNOTSUPP.
fn no_hardlink_support(e: &std::io::Error) -> bool {
    if e.kind() == std::io::ErrorKind::Unsupported {
        return true;
    }
    #[cfg(windows)]
    const CODES: &[i32] = &[1, 17, 50, 396];
    #[cfg(target_os = "linux")]
    const CODES: &[i32] = &[1, 18, 95];
    #[cfg(target_os = "macos")]
    const CODES: &[i32] = &[1, 18, 45, 102];
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    const CODES: &[i32] = &[1, 18];
    e.raw_os_error().is_some_and(|c| CODES.contains(&c))
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct LocalMod {
    tech_name: String,
    version: Option<String>,
    /// The mod's file, for a catalog alias (#386). Absent: joined by techName only.
    #[serde(default)]
    path: Option<String>,
    /// The file's size and modified time when the library scan read it, so the catalog-link
    /// backfill (#402) only takes the file the scan saw.
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    mtime_ms: Option<u64>,
}

/// The mods in an update check's list as the catalog-link backfill takes them: those with a
/// file, with the size and modified time the scan saw (a file that has changed since then is
/// left to a run after the next scan).
fn local_zips(mods: &[LocalMod]) -> Vec<aliasbackfill::LibraryZip> {
    mods.iter()
        .filter_map(|m| {
            Some(aliasbackfill::LibraryZip {
                path: PathBuf::from(m.path.as_deref()?),
                tech_name: m.tech_name.clone(),
                size: m.size?,
                mtime_ms: m.mtime_ms?,
            })
        })
        .collect()
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CatalogUpdate {
    tech_name: String,
    latest: Option<String>,
    has_update: bool,
    download_url: Option<String>,
    source: Option<String>,
    /// Where to get it by hand when there's no direct download (a ModHub page).
    page_url: Option<String>,
    /// Why this mod couldn't be checked (#386: its catalog link couldn't be read, or its linked
    /// entry couldn't be fetched). The rest of the library is still answered.
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl CatalogUpdate {
    /// The row for a mod that couldn't be checked.
    fn failed(tech_name: &str, why: String) -> Self {
        CatalogUpdate {
            tech_name: tech_name.to_string(),
            latest: None,
            has_update: false,
            download_url: None,
            source: None,
            page_url: None,
            error: Some(format!(
                "Silo couldn't check this mod against its catalog entry: {why}"
            )),
        }
    }
}

/// Check the whole library against the SiloAPI catalog in one request (by tech name),
/// returning which mods have a newer version and where to get it. Covers ModHub mods
/// too — not just GitHub-linked ones. Only mods the catalog knows are returned.
#[tauri::command]
async fn catalog_check_updates(
    app: tauri::AppHandle,
    mods: Vec<LocalMod>,
    link_catalog: Option<bool>,
) -> Result<Vec<CatalogUpdate>, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<Vec<CatalogUpdate>, String> {
        let base = siloapi_base(&app)?;
        // An update check of the library (not the drawer's own) also links the zips Silo
        // installed under another name before it kept catalog links (#402). It runs beside this
        // check, not before it; the Updates list asks again for the mods it links.
        if link_catalog == Some(true) {
            spawn_alias_backfill(app.clone(), local_zips(&mods));
        }
        // A zip Silo installed from a catalog entry under another name is looked up as that
        // entry (#386); a file is hashed only when an alias of its size is recorded for its name.
        // A mod whose link can't be checked, or whose linked entry can't be fetched, gets a
        // row saying so: never another entry's answer, and the rest of the library still gets
        // its own. Only the lookup failing fails the whole check.
        let conn = alias_db(&app)?;
        let links: Vec<Result<Option<catalogalias::Link>, String>> = mods
            .iter()
            .map(|m| file_catalog_link(&conn, &m.tech_name, m.path.as_deref()))
            .collect();
        let asked: Vec<(String, Option<catalogalias::Link>)> = mods
            .iter()
            .zip(&links)
            .map(|(m, l)| (m.tech_name.clone(), l.clone().ok().flatten()))
            .collect();
        let found = resolve_catalog(&base, &asked)?;

        let mut out = Vec::new();
        for ((m, link), answer) in mods.iter().zip(&links).zip(found) {
            let r = match (link, answer) {
                (Err(e), _) => {
                    log::warn!(
                        "[catalog] couldn't check {}'s catalog link: {e}",
                        m.tech_name
                    );
                    out.push(CatalogUpdate::failed(&m.tech_name, e.clone()));
                    continue;
                }
                (Ok(_), Err(e)) => {
                    log::warn!(
                        "[catalog] couldn't fetch {}'s catalog entry: {e}",
                        m.tech_name
                    );
                    out.push(CatalogUpdate::failed(&m.tech_name, e));
                    continue;
                }
                (Ok(_), Ok(None)) => continue,
                (Ok(_), Ok(Some(r))) => r,
            };
            let r = &r;
            let current = m.version.clone().unwrap_or_default();
            let has_update = r
                .latest_version
                .as_deref()
                .is_some_and(|latest| github::is_newer(latest, &current));
            out.push(CatalogUpdate {
                tech_name: m.tech_name.clone(),
                latest: r.latest_version.clone(),
                has_update,
                download_url: r.download.as_ref().map(|d| d.url.clone()),
                source: r.download.as_ref().map(|d| d.source.clone()),
                page_url: r.page_url.clone(),
                error: None,
            });
        }
        Ok(out)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Catalog entries for library mods, given as (techName, catalog link), one per mod in order
/// (`Ok(None)`: not in the catalog; `Err`: that mod's linked entry couldn't be fetched). The
/// outer error is the lookup itself failing. A mod with a link (#386) is its linked entry, by id: found by
/// the entry's techName, else fetched by its id (an entry renamed since, or with no techName).
/// It is never matched to another entry that has the file's name. Others go by techName.
fn resolve_catalog(
    base: &str,
    mods: &[(String, Option<catalogalias::Link>)],
) -> Result<CatalogAnswers, String> {
    let results = siloapi::lookup(base, &catalog_lookup_names(mods))?;
    Ok(mods
        .iter()
        .map(
            |(tech_name, link)| match (catalog_hit(tech_name, link.as_ref(), &results), link) {
                (Some(r), _) => Ok(Some(r.clone())),
                (None, Some(link)) => linked_entry(base, link),
                (None, None) => Ok(None),
            },
        )
        .collect())
}

/// `resolve_catalog`'s answer for each mod.
type CatalogAnswers = Vec<Result<Option<siloapi::LookupResult>, String>>;

/// The techNames to look up: a linked mod's catalog techName, every other mod's own.
fn catalog_lookup_names(mods: &[(String, Option<catalogalias::Link>)]) -> Vec<String> {
    mods.iter()
        .filter_map(|(tech_name, link)| match link {
            Some(l) => l.catalog_tech_name.clone(),
            None => Some(tech_name.clone()),
        })
        .collect()
}

/// A library mod's row among the lookup results: the entry its link names, by catalog id,
/// when it has one (never another entry); else the entry with its own techName.
fn catalog_hit<'a>(
    tech_name: &str,
    link: Option<&catalogalias::Link>,
    results: &'a [siloapi::LookupResult],
) -> Option<&'a siloapi::LookupResult> {
    match link {
        Some(l) => results.iter().find(|r| r.id == l.catalog_id),
        None => results
            .iter()
            .find(|r| r.tech_name.as_deref() == Some(tech_name)),
    }
}

/// A linked entry the lookup by techName didn't return, fetched by its id. None when the
/// catalog no longer has it; any other failure is an error, so a check that couldn't ask about
/// the mod never reads as a check that found nothing.
fn linked_entry(
    base: &str,
    link: &catalogalias::Link,
) -> Result<Option<siloapi::LookupResult>, String> {
    match siloapi::detail(base, &link.catalog_id) {
        Ok(d) => Ok(Some(lookup_from_detail(d))),
        Err(e) if catalog_not_found(&e) => {
            log::info!("[catalog] catalog entry {} is gone", link.catalog_id);
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// Whether a source's version is the record's latest: both given, and the same once a leading
/// `v` and case are set aside. A version that isn't given is unknown, never the same.
fn same_version(source: Option<&str>, latest: Option<&str>) -> bool {
    let norm = |v: &str| {
        let v = v.trim();
        v.strip_prefix(['v', 'V']).unwrap_or(v).to_ascii_lowercase()
    };
    match (source, latest) {
        (Some(a), Some(b)) if !a.trim().is_empty() => norm(a) == norm(b),
        _ => false,
    }
}

/// Whether a catalog request's error is the catalog answering 404 (`siloapi::not_answering`
/// words every error status as "... (error <code>) ...").
fn catalog_not_found(e: &str) -> bool {
    e.contains("(error 404)")
}

/// A full catalog record in the shape the lookup returns: its direct download is the first
/// installable source (sources are listed best-first) that says it is the record's latest
/// version, so an update check never offers another build as the latest. With none, the page.
fn lookup_from_detail(d: siloapi::BrowseMod) -> siloapi::LookupResult {
    let download = d
        .sources
        .iter()
        .find(|s| {
            s.installable
                && s.download_url.is_some()
                && same_version(s.version.as_deref(), d.latest_version.as_deref())
        })
        .and_then(|s| {
            Some(siloapi::Download {
                source: s.source.clone(),
                url: s.download_url.clone()?,
            })
        });
    siloapi::LookupResult {
        tech_name: d.tech_name,
        id: d.id,
        title: d.title,
        latest_version: d.latest_version,
        download,
        page_url: d.page_url,
        tags: d.tags,
    }
}

/// Verify one installed mod against its canonical build: hash the local zip, look the mod
/// up in the catalog, fetch the canonical manifest for the local version, and compare.
/// Verified (byte/content match), Modified (with the exact file diff), or Unverified (no
/// hashed build to compare — not proof of anything). Provenance, not antivirus.
/// Why a mod without a `<version>` can't be verified.
const NO_VERSION: &str =
    "This mod's modDesc.xml has no version, so there's no build to compare against.";

/// The version to look up a canonical build for: the modDesc version, unless it's missing or
/// blank (which the API answers with a 400, not "no build").
fn verifiable_version(version: Option<&str>) -> Option<&str> {
    version.map(str::trim).filter(|v| !v.is_empty())
}

#[tauri::command]
async fn verify_mod(
    app: tauri::AppHandle,
    tech_name: String,
    version: Option<String>,
    path: String,
) -> Result<provenance::VerifyResult, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<provenance::VerifyResult, String> {
        let base = siloapi_base(&app)?;
        let local = provenance::manifest_from_zip(std::path::Path::new(&path))?;

        // A zip Silo installed from a catalog entry under another name is that entry's, when
        // it is still exactly those bytes (#386). Otherwise find the id by tech name.
        let file = std::path::Path::new(&path);
        let link = catalogalias::link_for_hash(
            &alias_db(&app)?,
            &tech_name,
            file,
            &local.archive_sha256,
        )?;
        let catalog_id = match link {
            Some(l) => l.catalog_id,
            None => {
                let hits = siloapi::lookup(&base, std::slice::from_ref(&tech_name))?;
                let Some(hit) = hits
                    .into_iter()
                    .find(|r| r.tech_name.as_deref() == Some(tech_name.as_str()))
                else {
                    return Ok(provenance::VerifyResult::unverified(
                        "This mod isn't in Silo's catalog yet, so there's nothing to compare it with.",
                    ));
                };
                hit.id
            }
        };

        // The server does version-equivalence (ModHub `1.2.0` ↔ GitHub `v1.2.0`), so send
        // the local modDesc version verbatim. 404 → no hashed build for this version.
        let Some(ver) = verifiable_version(version.as_deref()) else {
            // The API refuses a blank version (400 "version query param required").
            return Ok(provenance::VerifyResult::unverified(NO_VERSION));
        };
        match siloapi::manifest(&base, &catalog_id, ver)? {
            Some(cm) => {
                let canon_entries: Vec<provenance::Entry> = cm
                    .entries
                    .iter()
                    .map(|e| provenance::Entry {
                        path: e.path.clone(),
                        sha256: e.sha256.clone(),
                    })
                    .collect();
                let mut r = provenance::compare(
                    &local,
                    cm.archive_sha256.as_deref(),
                    &cm.manifest_hash,
                    &canon_entries,
                );
                r.matched_version = cm.version;
                Ok(r)
            }
            None => Ok(provenance::VerifyResult::unverified(
                "Silo's catalog has no published build of this version to compare with yet.",
            )),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn gh_logout(app: tauri::AppHandle) -> Result<(), String> {
    let db = db_path(&app)?;
    // Counts from the click: no new request with the token is admitted, and no sign-in already
    // out (or started before this ends) can store one. Requests already admitted (they may
    // still be connecting) finish before the Disconnect does.
    let signing_out = GH_SESSION.begin_sign_out();
    off_main(move || {
        signing_out.finish(|marked| {
            let conn = match db::open(&db) {
                Ok(c) => c,
                Err(e) => {
                    // Delete the keychain copy at least; the database can't record anything.
                    if !secrets::forget_keychain("gh_token") {
                        log::warn!("gh_token: couldn't remove it from the keychain either");
                    }
                    return (false, Err(gh_not_removed(&e, marked)));
                }
            };
            let (clean, r) = gh_clear_account(&conn);
            (
                clean,
                r.map_err(|e| if clean { e } else { gh_not_removed(&e, marked) }),
            )
        })
    })
    .await
}

/// Remove the GitHub account from storage: the token and the account details. Returns whether
/// storage is clean (the token can't be read back, even after a restart, and the details are
/// gone; then the signed-out marker can go) and what to tell the user.
///
/// Silo stops using the token even when the keychain keeps a copy it couldn't delete (it's
/// distrusted, and removed once the store allows), or the database keeps a row it couldn't
/// delete (revoked), so that still counts as clean; the leftover is reported. Everything else is
/// cleared even when one step fails.
fn gh_clear_account(conn: &rusqlite::Connection) -> (bool, Result<(), String>) {
    let token = secrets::clear(conn, "gh_token");
    // Legacy flags from before scope capture (gh_write, gh_gist) too, so nothing lingers.
    let rest = ["gh_user", "gh_scopes", "gh_write", "gh_gist"]
        .into_iter()
        .map(|k| db::set_app_setting(conn, k, None))
        .fold(Ok(()), Result::and);
    match (token, rest) {
        (Ok(left), Ok(())) => (true, left.message().map_or(Ok(()), |m| Err(m.into()))),
        (Ok(_), Err(e)) | (Err(e), _) => (false, Err(e)),
    }
}

/// Disconnect couldn't clean up storage (`e`). Silo stays signed out for this run; across a
/// restart only if the signed-out marker was written (`marked`).
fn gh_not_removed(e: &str, marked: bool) -> String {
    if marked {
        format!(
            "Silo couldn't remove your GitHub account from its storage ({e}). It won't use \
             the token, even after a restart. Connecting again replaces it."
        )
    } else {
        format!(
            "Silo couldn't remove your GitHub account from its storage ({e}). It won't use \
             the token while Silo stays open, but may find it again after a restart."
        )
    }
}

// ── Tags ──
#[tauri::command]
async fn get_tags(app: tauri::AppHandle) -> Result<Vec<db::TagRow>, String> {
    let db = db_path(&app)?;
    off_main(move || Ok(db::load_tags(&db::open(&db)?))).await
}

#[tauri::command]
async fn set_tags(
    app: tauri::AppHandle,
    tech_name: String,
    tags: Vec<String>,
) -> Result<(), String> {
    let db = db_path(&app)?;
    off_main(move || {
        let mut conn = db::open(&db)?;
        db::set_tags(&mut conn, &tech_name, &tags)
    })
    .await
}

// ── Manual category overrides ──
#[tauri::command]
async fn get_overrides(app: tauri::AppHandle) -> Result<Vec<db::CategoryOverride>, String> {
    let db = db_path(&app)?;
    off_main(move || Ok(db::load_overrides(&db::open(&db)?))).await
}

#[tauri::command]
async fn set_override(app: tauri::AppHandle, row: db::CategoryOverride) -> Result<(), String> {
    let db = db_path(&app)?;
    off_main(move || db::set_override(&db::open(&db)?, &row)).await
}

// ── Organize / projection engine (writes to the game folder) ──

/// The folders Silo is allowed to write to: the active layout the mods-root commands resolved
/// (user choice > FS25 override > default, see `modsroot`) — its mods folder, its archive and,
/// during an archive move, the other archive — or, before they have run, the auto-detected
/// default. This is the single source of truth for "where may we write"; Rust, not the
/// webview, is the authority.
fn allowed_roots() -> Vec<PathBuf> {
    trusted_roots(modsroot::active(), modsroot::resolved(), true)
}

/// The mods folder(s) only — never an archive. Frontend-supplied roots and default scans
/// resolve against this.
fn mods_roots() -> Vec<PathBuf> {
    trusted_roots(modsroot::active(), modsroot::resolved(), false)
}

/// The active layout's folders (`with_archives`: its archive and a move's other archive too),
/// or the auto-detected default before anything is `resolved`. Once resolved with no layout
/// (the folder is unavailable, or blocked because it shares files with another layout):
/// nothing at all — never a fallback to FS25's default folder.
fn trusted_roots(
    active: Option<modsroot::Active>,
    resolved: bool,
    with_archives: bool,
) -> Vec<PathBuf> {
    match active {
        Some(a) if with_archives => {
            let mut v = vec![a.layout.mods, a.layout.archive];
            v.extend(a.move_to);
            v
        }
        Some(a) => vec![a.layout.mods],
        None if resolved => Vec::new(),
        // The main default (`gameprofile::Place::DefaultModsFolder`): no profile is known yet.
        None => fsgame::default_mods_paths(),
    }
}

/// `then()`, unless the resolved folder is `blocked`: then its reason, so a refusal says what's
/// really wrong rather than "no mods folder" or "outside the mods folder".
fn blocked_first<T>(
    blocked: Option<String>,
    then: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    match blocked {
        Some(why) => Err(why),
        None => then(),
    }
}

/// `paths::ensure_write_under` against the trusted roots, with a blocked folder's reason first.
fn ensure_library_write(dest: &std::path::Path) -> Result<(), String> {
    blocked_first(modsroot::blocked(), || {
        paths::ensure_write_under(&allowed_roots(), dest)
    })
}

/// The active layout (mods folder + archive), validated against a frontend-supplied root. The
/// trusted folder and its layout come from ONE published snapshot, so a mods-folder switch
/// published in between can't pair this folder with another folder's archive.
fn primary_layout(root: Option<String>) -> Result<organize::Layout, String> {
    // `resolved` before the snapshot: it is set after each publish, so true here means the
    // snapshot below is at least that resolution.
    let resolved = modsroot::resolved();
    let (active, blocked) = modsroot::published();
    let mods = root_in(active.clone(), blocked, resolved, root)?;
    Ok(layout_for(active, &mods))
}

/// Pure: the layout for trusted mods folder `mods`: the published `active` layout when it is
/// that folder (its archive may be custom), else the default layout.
fn layout_for(active: Option<modsroot::Active>, mods: &std::path::Path) -> organize::Layout {
    match active {
        Some(a) if modsroot::same_folder(&a.layout.mods, mods) => a.layout,
        _ => organize::Layout::default_for(mods),
    }
}

const STILL_LOADING: &str = "Silo is still loading your library. Try again in a moment.";

/// Refuse writes to the library while an archive move is unfinished — or before the active
/// layout has been worked out (an unfinished move can't be ruled out yet). Once resolved with
/// no usable mods folder there's nothing to move, so choosing a folder stays possible.
fn ensure_no_move() -> Result<(), String> {
    // A move that's starting, copying or finishing counts even before (or after) the layout
    // publishes its marker — it holds the library lock across both ends.
    if move_in_progress() {
        return Err("Finish or cancel the archive move first.".into());
    }
    let (active, blocked) = modsroot::published();
    no_move_refusal(active.as_ref(), blocked.as_ref(), modsroot::resolved())
}

/// Pure: `ensure_no_move` over the published state. A blocked folder's own unfinished move
/// counts too.
fn no_move_refusal(
    active: Option<&modsroot::Active>,
    blocked: Option<&modsroot::Blocked>,
    resolved: bool,
) -> Result<(), String> {
    let unfinished = active
        .or_else(|| blocked.and_then(|b| b.moving.as_deref()))
        .is_some_and(|a| a.move_to.is_some());
    match active {
        _ if unfinished => Err("Finish or cancel the archive move first.".into()),
        Some(_) => Ok(()),
        None if !resolved => Err(STILL_LOADING.into()),
        None => Ok(()),
    }
}

/// Serializes every write to the library (organize, projections, installs, adopt, archive
/// moves). Commands take it inside their blocking body — never across an await; an archive
/// move only `try_lock`s, so it never waits behind, or starts under, another write.
static LIBRARY_WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Refusal when another change holds the library lock (a write that waited too long, or an
/// archive move that won't start under another write).
const LIBRARY_BUSY: &str =
    "Silo is busy with another change to your library. Try again in a moment.";

/// `try_move_lock`'s refusal while another move holds the library lock. Its own text (not
/// relocate's "already running") so `cancel_falls_back` matches this case and nothing else.
const MOVE_LOCK_HELD: &str = "An archive move is already under way.";

/// Set while an archive move holds the library write lock — including the checks before
/// relocate's own guard flips `is_running()` — so writes refuse at once instead of waiting.
static MOVE_HOLDS_LOCK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A move (or its pre-checks) holds the library lock, or relocate is copying.
fn move_in_progress() -> bool {
    MOVE_HOLDS_LOCK.load(std::sync::atomic::Ordering::SeqCst) || relocate::is_running()
}

/// Set by `force_quit` so the close guard lets the window go after the user said "Quit anyway".
static FORCE_QUIT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// True while Silo is changing files: a library write holds the lock, or an archive move is
/// running (including between its lock drops). Never waits for the lock.
fn library_busy() -> bool {
    busy_with(&LIBRARY_WRITE, move_in_progress())
}

/// [`library_busy`] against a given lock. A poisoned lock that nobody holds is not busy.
fn busy_with(lock: &std::sync::Mutex<()>, moving: bool) -> bool {
    moving || matches!(lock.try_lock(), Err(std::sync::TryLockError::WouldBlock))
}

/// Whether Silo is changing files right now (the frontend asks before a self-update).
#[tauri::command]
fn app_busy() -> bool {
    library_busy()
}

/// Quit even though Silo is busy, after the user confirmed it. `exit` raises `ExitRequested`,
/// which `hold_quit` lets through once `FORCE_QUIT` is set.
#[tauri::command]
fn force_quit(app: tauri::AppHandle) {
    FORCE_QUIT.store(true, std::sync::atomic::Ordering::SeqCst);
    app.exit(0);
}

/// The library write lock as held by an archive move; clears `MOVE_HOLDS_LOCK` on drop
/// (before the lock itself is released).
struct MoveLock {
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl Drop for MoveLock {
    fn drop(&mut self) {
        MOVE_HOLDS_LOCK.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

fn lock_result(
    r: std::sync::TryLockResult<std::sync::MutexGuard<'static, ()>>,
) -> Option<std::sync::MutexGuard<'static, ()>> {
    match r {
        Ok(g) => Some(g),
        Err(std::sync::TryLockError::Poisoned(e)) => Some(e.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

/// How long a write waits behind another one before giving up.
const LIBRARY_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// How long a finished download waits to be placed. Longer than `LIBRARY_WAIT` so a brief
/// busy spell never throws away a completed download (move/loading refusals stay instant).
const PLACE_WAIT: std::time::Duration = std::time::Duration::from_secs(600);

/// Take the library write lock, waiting behind another ordinary write (up to `LIBRARY_WAIT`)
/// but never behind an archive move (refused at once, like `ensure_no_move`), then re-check
/// that no move started meanwhile. Hold the guard for the whole write.
fn library_write() -> Result<std::sync::MutexGuard<'static, ()>, String> {
    library_write_within(LIBRARY_WAIT)
}

/// `library_write` with an explicit deadline. Once the lock is held, update leftovers in the
/// active layout are resolved first (`settle_updates_locked`), so no write starts on top of a
/// half-finished mod update.
fn library_write_within(
    max: std::time::Duration,
) -> Result<std::sync::MutexGuard<'static, ()>, String> {
    let guard = wait_for_library(max, library_writable)?;
    settle_updates_locked()?;
    Ok(guard)
}

/// Whether a library write may start: Silo isn't installing its own update and no archive move
/// is starting or running.
fn library_writable() -> Result<(), String> {
    ensure_not_self_updating()?;
    ensure_no_move()?;
    if move_in_progress() {
        return Err("Finish or cancel the archive move first.".into());
    }
    Ok(())
}

/// The library lock for recording a catalog link (#402), waiting at most `max`: the same lock
/// and the same refusals as `library_write_within`, without its settling of half-finished mod
/// updates. A link record only writes the database (for bytes the backfill just hashed and
/// re-checks unchanged), so it needs no file recovery, and settling takes no time limit.
fn catalog_link_write_within(
    max: std::time::Duration,
) -> Result<std::sync::MutexGuard<'static, ()>, String> {
    wait_for_library(max, library_writable)
}

/// Set while Silo installs its own update (`set_self_updating`): library writes and archive
/// moves refuse, so nothing starts changing files just before Silo restarts.
static SELF_UPDATING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

const SELF_UPDATE_RUNNING: &str = "Silo is installing an update.";

fn self_updating() -> bool {
    SELF_UPDATING.load(std::sync::atomic::Ordering::SeqCst)
}

fn ensure_not_self_updating() -> Result<(), String> {
    if self_updating() {
        Err(SELF_UPDATE_RUNNING.into())
    } else {
        Ok(())
    }
}

/// The frontend marks the start (and, if it fails, the end) of installing Silo's own update.
#[tauri::command]
fn set_self_updating(on: bool) {
    SELF_UPDATING.store(on, std::sync::atomic::Ordering::SeqCst);
}

/// Poll for the library lock until `max` has passed. `ready` runs before every attempt and
/// again once the lock is held; its refusal ends the wait at once.
fn wait_for_library(
    max: std::time::Duration,
    ready: impl Fn() -> Result<(), String>,
) -> Result<std::sync::MutexGuard<'static, ()>, String> {
    let start = std::time::Instant::now();
    loop {
        ready()?;
        if let Some(guard) = take_for_write(&LIBRARY_WRITE, &LIBRARY_EPOCH) {
            ready()?;
            return Ok(guard);
        }
        if start.elapsed() >= max {
            return Err(LIBRARY_BUSY.into());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Bumped every time a writer takes the library lock, so a scan can tell whether the library
/// changed while it ran.
static LIBRARY_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Take `lock` for a write without waiting, and count it in `epoch` (bumped while the lock is
/// held, so a scan reading `epoch` under the same lock always sees it).
fn take_for_write(
    lock: &'static std::sync::Mutex<()>,
    epoch: &std::sync::atomic::AtomicU64,
) -> Option<std::sync::MutexGuard<'static, ()>> {
    let guard = lock_result(lock.try_lock())?;
    epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    Some(guard)
}

/// The epoch, if no writer holds `lock` right now.
fn quiet_epoch(
    lock: &'static std::sync::Mutex<()>,
    epoch: &std::sync::atomic::AtomicU64,
) -> Option<u64> {
    let _g = lock_result(lock.try_lock())?;
    Some(epoch.load(std::sync::atomic::Ordering::SeqCst))
}

/// A scan may rewrite the organize manifest only if the library was quiet when it began, is
/// quiet now, and no write took the lock in between.
fn scan_may_reconcile(start: Option<u64>, now: Option<u64>) -> bool {
    matches!((start, now), (Some(a), Some(b)) if a == b)
}

/// Rewrite folder `key`'s organize manifest from what a scan found on disk, unless the library
/// changed since the scan began (`start`, from `quiet_epoch`). `lock` is held while
/// reconciling, so no write starts on top of it. Returns false when the manifest was left
/// alone because the library wasn't quiet. A database error is logged and otherwise ignored:
/// the reconcile is one transaction, so the manifest stays as it was and the next scan retries.
fn reconcile_if_quiet(
    lock: &'static std::sync::Mutex<()>,
    epoch: &std::sync::atomic::AtomicU64,
    start: Option<u64>,
    conn: &mut rusqlite::Connection,
    key: &str,
    rows: &[db::OrganizedRow],
) -> bool {
    let now = lock_result(lock.try_lock());
    let seen = now
        .as_ref()
        .map(|_| epoch.load(std::sync::atomic::Ordering::SeqCst));
    if !scan_may_reconcile(start, seen) {
        return false;
    }
    if let Err(e) = db::reconcile_organized(conn, key, rows) {
        log::warn!("scan: couldn't update the organize manifest for {key}: {e}");
    }
    true
}

/// The library write lock for an archive move, without waiting: a busy library refuses.
fn try_move_lock() -> Result<MoveLock, String> {
    ensure_not_self_updating()?;
    match take_for_write(&LIBRARY_WRITE, &LIBRARY_EPOCH) {
        Some(g) => {
            // A self-update may have started between the check above and taking the lock.
            if self_updating() {
                drop(g);
                return Err(SELF_UPDATE_RUNNING.into());
            }
            // Order matters: clear requests left from before (a pause or cancel nothing was
            // running to receive) only once the lock is ours, and only then announce the move —
            // a cancel that sees the flag from here on is sent to this move and kept.
            relocate::requests().store(0, std::sync::atomic::Ordering::SeqCst);
            // Not while Play is starting the game (#415); then announced, under the launch gate.
            if let Err(e) = admit_move() {
                drop(g);
                return Err(e);
            }
            Ok(MoveLock { _guard: g })
        }
        None if move_in_progress() => Err(MOVE_LOCK_HELD.into()),
        None => Err(LIBRARY_BUSY.into()),
    }
}

/// Refuse organize writes and new archive moves while a custom archive's drive is missing
/// (nothing is deleted; the banner offers Retry / Choose another), or while no layout is known.
fn ensure_archive_available() -> Result<(), String> {
    archive_availability(
        modsroot::active().as_ref(),
        modsroot::resolved(),
        modsroot::blocked().as_deref(),
    )
}

/// Pure: `ensure_archive_available` over the published state. A blocked folder is refused
/// with its reason.
fn archive_availability(
    active: Option<&modsroot::Active>,
    resolved: bool,
    blocked: Option<&str>,
) -> Result<(), String> {
    match active {
        None if !resolved => Err(STILL_LOADING.into()),
        None => Err(blocked.unwrap_or("No mods folder selected").to_string()),
        Some(a) if a.layout.archive_missing() => Err(archive_unavailable(&a.layout.archive)),
        Some(_) => Ok(()),
    }
}

/// Pure: why nothing can be filed into or moved out of a missing archive, and what to do.
fn archive_unavailable(archive: &std::path::Path) -> String {
    format!(
        "Your library archive at {} isn't available. Reconnect its drive, or use Choose another to point Silo at where it is now.",
        archive.display()
    )
}

/// Resolve a frontend-supplied mods folder to a TRUSTED one. Empty → the first detected
/// folder. Otherwise the path must canonicalize to one of the allowed mods folders (never an
/// archive), or it's rejected — so a compromised webview can't redirect an install/organize
/// into an arbitrary directory. Returns the matched trusted path, never the raw frontend string.
fn primary_root(root: Option<String>) -> Result<PathBuf, String> {
    let resolved = modsroot::resolved();
    let (active, blocked) = modsroot::published();
    root_in(active, blocked, resolved, root)
}

/// `primary_root` over one published snapshot (`active`, `blocked`, `resolved`).
fn root_in(
    active: Option<modsroot::Active>,
    blocked: Option<modsroot::Blocked>,
    resolved: bool,
    root: Option<String>,
) -> Result<PathBuf, String> {
    if let Some(b) = blocked {
        return Err(b.reason);
    }
    let allowed = trusted_roots(active, resolved, false);
    match root {
        Some(r) if !r.is_empty() => {
            let want = PathBuf::from(&r)
                .canonicalize()
                .map_err(|_| "That mods folder isn't there anymore.".to_string())?;
            allowed
                .into_iter()
                .find(|a| a.canonicalize().map(|ac| ac == want).unwrap_or(false))
                .ok_or_else(|| {
                    "refusing a mods folder outside the detected game folder(s)".to_string()
                })
        }
        _ => allowed
            .into_iter()
            .next()
            .ok_or_else(|| "No mods folder detected".to_string()),
    }
}

/// Dry run: what organizing would move (read-only).
#[tauri::command]
async fn plan_organize(
    root: Option<String>,
    mods: Vec<organize::ModInput>,
) -> Result<Vec<organize::PlannedMove>, error::AppError> {
    off_main(move || Ok(organize::plan_organize(&primary_root(root)?, &mods)))
        .await
        .map_err(error::AppError::from)
}

#[tauri::command]
async fn apply_organize(
    app: tauri::AppHandle,
    root: Option<String>,
    mods: Vec<organize::ModInput>,
) -> Result<organize::Report, error::AppError> {
    ensure_no_move()?;
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || -> Result<organize::Report, String> {
        let _write = library_write()?;
        ensure_archive_available()?;
        let layout = primary_layout(root)?;
        let conn = db::open(&db)?;
        Ok(organize::apply_organize(
            &conn,
            &layout,
            &layout.mods,
            &mods,
        ))
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(error::AppError::from)
}

/// Adopt the file the user swapped into the mods folder as the mod's new canonical version
/// (old copy kept under backups/). The caller should rescan afterward.
#[tauri::command]
async fn adopt_foreign_file(
    app: tauri::AppHandle,
    root: Option<String>,
    file_name: String,
) -> Result<(), error::AppError> {
    ensure_no_move()?;
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        swap_refusal(gamelaunch::fs25_running())?;
        let _write = library_write()?;
        ensure_archive_available()?;
        let layout = primary_layout(root)?;
        let conn = db::open(&db)?;
        organize::adopt_foreign(&conn, &layout, &file_name)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(error::AppError::from)
}

/// Restore Silo's managed copy over a file the user swapped in (their file kept under
/// backups/). The caller should rescan afterward.
#[tauri::command]
async fn restore_foreign_file(
    app: tauri::AppHandle,
    root: Option<String>,
    file_name: String,
) -> Result<(), error::AppError> {
    ensure_no_move()?;
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        swap_refusal(gamelaunch::fs25_running())?;
        let _write = library_write()?;
        ensure_archive_available()?;
        let layout = primary_layout(root)?;
        let conn = db::open(&db)?;
        organize::restore_projection(&conn, &layout, &file_name)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(error::AppError::from)
}

#[tauri::command]
async fn set_active(
    app: tauri::AppHandle,
    root: Option<String>,
    active: Vec<String>,
) -> Result<organize::Report, error::AppError> {
    ensure_no_move()?;
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || -> Result<organize::Report, String> {
        let _write = library_write()?;
        ensure_archive_available()?;
        let layout = primary_layout(root)?;
        let conn = db::open(&db)?;
        let set: HashSet<String> = active.into_iter().collect();
        Ok(organize::set_active(&conn, &layout, &set))
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(error::AppError::from)
}

/// Whether the mods folder supports hardlinks. False on cloud-synced folders (OneDrive / Google
/// Drive / Proton Drive), where Silo projects the active set as copies instead.
#[tauri::command]
async fn hardlink_support(root: Option<String>) -> Result<bool, String> {
    off_main(move || Ok(organize::supports_hardlinks(&primary_root(root)?))).await
}

/// Repair copy-projections into hardlinks (reclaims duplicated space, makes activate/deactivate
/// O(1)). Emits `relink-progress` `(done, total)` events so the UI can show a bar during the
/// one-time hash-and-relink of a large library.
#[tauri::command]
async fn relink_projections(
    app: tauri::AppHandle,
    root: Option<String>,
) -> Result<organize::RelinkReport, error::AppError> {
    ensure_no_move()?;
    let db = db_path(&app)?;
    let emitter = app.clone();
    tauri::async_runtime::spawn_blocking(move || -> Result<organize::RelinkReport, String> {
        let _write = library_write()?;
        ensure_archive_available()?;
        let layout = primary_layout(root)?;
        let conn = db::open(&db)?;
        Ok(organize::relink_projections(
            &conn,
            &layout,
            |done, total| {
                let _ = emitter.emit("relink-progress", (done, total));
            },
        ))
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(error::AppError::from)
}

#[tauri::command]
async fn flatten(
    app: tauri::AppHandle,
    root: Option<String>,
) -> Result<organize::Report, error::AppError> {
    ensure_no_move()?;
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || -> Result<organize::Report, String> {
        let _write = library_write()?;
        ensure_archive_available()?;
        let layout = primary_layout(root)?;
        let conn = db::open(&db)?;
        Ok(organize::flatten(&conn, &layout))
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(error::AppError::from)
}

// ── Mods folder selection (#162) ──

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct KnownRoot {
    path: String,
    mod_count: i64,
    current: bool,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ModsRootStatus {
    path: Option<String>,
    source: modsroot::Source,
    game_reads: Option<String>,
    mismatch: bool,
    available: bool,
    /// The FS25 user dir exists but has no `mods/` yet (common on Mac until a first download).
    mods_folder_missing: bool,
    known: Vec<KnownRoot>,
    can_write_game_setting: bool,
    game_setting_problem: Option<String>,
    unresolved_override: Option<String>,
    /// Why Silo won't change files in this folder: it shares files with another layout's
    /// archive (`resolve_layout`). Organize, flatten and installs are refused meanwhile.
    layout_problem: Option<String>,
    /// What causes it: an overlap, only FS25's own setting ("Point FS25 here" clears it), or a
    /// database read that failed ("Retry").
    layout_problem_cause: Option<modsroot::BlockCause>,
    /// The folder it runs into, when known.
    layout_conflict: Option<String>,
    /// The mods folder's FS25 profile (#387), when the folder can be one's (`<profile>/mods`).
    /// While it's on, `game_reads` / `mismatch` describe the profile, not the main user folder.
    game_profile: Option<gameprofile::GameProfileStatus>,
}

/// The keys a mods folder's FS25 profile settings are kept under (#387): its identity key
/// (resolved, `modsroot::canonical_key`) and its spelling as chosen (normalized, not resolved).
/// Through a junction whose target is gone the identity key changes; the spelling doesn't, so
/// the profile is still found and nothing falls back to the main user folder.
fn profile_setting_keys(mods: &std::path::Path) -> Vec<String> {
    let mut v = vec![modsroot::canonical_key(mods)];
    let raw = modsroot::normalize_key(&mods.to_string_lossy(), cfg!(windows));
    if !v.contains(&raw) {
        v.push(raw);
    }
    v
}

/// The FS25 profile the mods folder `mods` could belong to, and whether it's on (#387). `None`
/// when the folder can't be a profile's (`gameprofile::candidate_dir`). `main_user_dir` is the
/// main FS25 user folder.
///
/// The folder's choice and "seen as a profile" mark are kept under each of its
/// `profile_setting_keys`, so they're found again while it can't be resolved.
fn game_profile_for(
    conn: &rusqlite::Connection,
    mods: &std::path::Path,
    main_user_dir: Option<&std::path::Path>,
) -> Option<gameprofile::GameProfileStatus> {
    // A profile is started as `-profile <dir>` from the game's own folder: only a full path
    // means the same folder to Silo and to the game.
    if !mods.is_absolute() {
        return None;
    }
    // Files, not just names: a folder called `game.xml` doesn't make a profile.
    let is_file = |p: &std::path::Path| p.is_file();
    let covers = |dir: &std::path::Path, main: &std::path::Path| {
        gameprofile::key_covers(
            &modsroot::canonical_key(dir),
            &modsroot::canonical_key(main),
        )
    };
    let dir = gameprofile::candidate_dir(mods, main_user_dir, covers, |d| {
        gameprofile::is_install_dir(d, is_file)
    })?;
    let keys = profile_setting_keys(mods);
    // Remembered once seen, so a profile whose files can't be seen for a while doesn't turn into
    // "not a profile" and send Diagnose, savegames and Play to the main user folder.
    let seen: Vec<bool> = keys
        .iter()
        .map(|k| db::get_app_setting(conn, &gameprofile::seen_key(k)).is_some())
        .collect();
    let seen_before = seen.iter().any(|s| *s);
    let looks_now = gameprofile::looks_like_profile(&dir, is_file);
    let stored = keys
        .iter()
        .find_map(|k| db::get_app_setting(conn, &gameprofile::setting_key(k)));
    // Every key gets the mark and the choice any key has, so whichever spelling the folder is
    // reached by later (a junction, its target) still finds them while it can't be resolved.
    // Best effort: what's known now still counts for this look.
    let remember = |key: String, value: &str| {
        if let Err(e) = db::set_app_setting(conn, &key, Some(value)) {
            log::warn!("couldn't remember {}'s FS25 profile: {e}", dir.display());
        }
    };
    if looks_now || seen_before {
        for (k, s) in keys.iter().zip(&seen) {
            if !s {
                remember(gameprofile::seen_key(k), gameprofile::ON);
            }
        }
    }
    if let Some(v) = stored.as_deref() {
        for k in &keys {
            if db::get_app_setting(conn, &gameprofile::setting_key(k)).as_deref() != Some(v) {
                remember(gameprofile::setting_key(k), v);
            }
        }
    }
    let detected = gameprofile::detected(looks_now, seen_before);
    let chosen = matches!(
        stored.as_deref(),
        Some(gameprofile::ON) | Some(gameprofile::OFF)
    );
    Some(gameprofile::GameProfileStatus {
        dir: dir.to_string_lossy().into_owned(),
        enabled: gameprofile::enabled(stored.as_deref(), detected, gameprofile::SUPPORTED),
        detected,
        chosen,
        supported: gameprofile::SUPPORTED,
        unsupported_reason: gameprofile::UNSUPPORTED.map(str::to_string),
    })
}

/// The folder FS25 reads mods from when it runs on profile `dir`: that profile's own mods
/// folder setting when it's on (the profile's `gameSettings.xml`), else `<dir>/mods` (also when
/// the file isn't there yet: FS25 then starts with its default). An error when the settings file
/// is there but can't be read, or its setting can't be turned into a folder: Silo then doesn't
/// claim to know where the game reads.
fn profile_game_reads(dir: &std::path::Path) -> Result<PathBuf, String> {
    let xml = match std::fs::read_to_string(dir.join("gameSettings.xml")) {
        Ok(x) => x,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(dir.join("mods")),
        Err(e) => {
            return Err(format!(
                "Silo couldn't read this profile's game settings ({e}), so it can't tell which mods folder the game loads."
            ))
        }
    };
    // Only a whole, well-formed file says "no custom mods folder": an empty or cut-off one
    // (the game was writing it) can't tell, so it's unknown too.
    if ensure_well_formed(&xml).is_err() {
        return Err(
            "This profile's game settings file is damaged or incomplete, so Silo can't tell which mods folder the game loads."
                .into(),
        );
    }
    if modsroot::override_on_without_folder(&xml) {
        return Err(
            "This profile's game settings turn on a custom mods folder but don't name it.".into(),
        );
    }
    match modsroot::read_override(&xml).filter(|o| o.active) {
        None => Ok(dir.join("mods")),
        // Only a full path is a folder Silo can place (`C:\…` or `\\server\share\…`): an empty,
        // relative or drive-relative one would be read against some working folder FS25 and
        // Silo needn't share.
        Some(o) => modsroot::PathMap::identity()
            .to_host(&o.directory)
            .filter(|p| p.is_absolute())
            .ok_or_else(|| {
                format!(
                    "This profile's game settings name a mods folder Silo can't find: {}",
                    o.directory
                )
            }),
    }
}

fn path_map(loc: &fsgame::GameLocation) -> modsroot::PathMap {
    match &loc.prefix {
        Some(p) => modsroot::PathMap::from_prefix(p),
        None => modsroot::PathMap::identity(),
    }
}

/// How many mods a known mods folder's library holds, counted the way a scan lists them: each
/// organized mod once, plus each zip or unpacked folder mod in the folder that isn't an
/// organized mod's projection (#385). Pure.
fn known_mod_count(organized: &[db::OrganizedRow], root_names: &[String]) -> i64 {
    let projected: HashSet<String> = organized
        .iter()
        .map(|r| r.file_name.to_lowercase())
        .collect();
    let loose = root_names
        .iter()
        .filter(|n| !projected.contains(&n.to_lowercase()))
        .count();
    (organized.len() + loose) as i64
}

/// The layout to publish for the resolved mods folder `mods`, whatever chose it (the user,
/// FS25's override, the default) — or why there is none: `mods` shares files with another
/// layout (`mods_folder_check`, with FS25's default folder `default` and the folder it reads,
/// `game_reads`, counted as known), or the check couldn't read the database. When the clash
/// is only with `game_reads` and `mods` is the user's choice (`chosen`), the cause is FS25's
/// own setting: pointing FS25 at `mods` clears it. Checked before anything is published.
/// `profile_reads` is the folder FS25 reads on the folder's own FS25 profile (#387), which
/// counts as known too.
fn resolve_layout(
    conn: &rusqlite::Connection,
    mods: &std::path::Path,
    default: Option<&std::path::Path>,
    game_reads: Option<&std::path::Path>,
    profile_reads: Option<&std::path::Path>,
    chosen: bool,
) -> Result<modsroot::Active, modsroot::Blocked> {
    let known = |with_game: bool| {
        let mut extra: Vec<PathBuf> = default.iter().map(PathBuf::from).collect();
        extra.extend(profile_reads.map(PathBuf::from));
        if with_game {
            extra.extend(game_reads.map(PathBuf::from));
        }
        known_mods_folders(conn, &extra)
    };
    let blocked = |reason: String, cause, conflict| modsroot::Blocked {
        reason,
        cause,
        mods: mods.to_path_buf(),
        conflict,
        moving: unfinished_move(conn, mods).map(Box::new),
    };
    let problem = match mods_folder_check(conn, mods, &known(true)) {
        Ok(None) => return Ok(resolve_active(conn, mods)),
        Ok(Some(p)) => p,
        Err(e) => {
            return Err(blocked(
                folder_check_unreadable(&e),
                modsroot::BlockCause::ReadError,
                None,
            ))
        }
    };
    let game = game_reads.filter(|g| chosen && !modsroot::same_folder(g, mods));
    if let Some(g) = game {
        if matches!(mods_folder_check(conn, mods, &known(false)), Ok(None)) {
            return Err(blocked(
                format!(
                    "Farming Simulator 25's mods folder setting points at {}, which shares files with this folder. Point Farming Simulator 25 at this folder, or choose another one.",
                    g.display()
                ),
                modsroot::BlockCause::GameSetting,
                Some(problem.conflict),
            ));
        }
    }
    Err(blocked(
        problem.message,
        modsroot::BlockCause::Overlap,
        Some(problem.conflict),
    ))
}

/// The layout of `mods`' unfinished archive move, if it has one: the archive it started from
/// (`Layout`'s rules, else the default) and the one it's going to. No side effects (unlike
/// `resolve_active`, nothing is rediscovered or stored).
fn unfinished_move(
    conn: &rusqlite::Connection,
    mods: &std::path::Path,
) -> Option<modsroot::Active> {
    let m = relocate::load_marker(conn, &modsroot::canonical_key(mods))?;
    let layout = organize::Layout::new(mods.to_path_buf(), m.from)
        .unwrap_or_else(|_| organize::Layout::default_for(mods));
    Some(modsroot::Active {
        layout,
        move_to: Some(m.to),
    })
}

/// The layout a move's resume or cancel runs in: the published one, or — while the folder is
/// blocked — that folder's own unfinished move, so it can still be finished or brought home.
fn resumable_layout(
    active: Option<modsroot::Active>,
    blocked: Option<modsroot::Blocked>,
    resolved: bool,
) -> Result<organize::Layout, String> {
    match (active, blocked) {
        (Some(a), _) => Ok(a.layout),
        (None, Some(b)) => b.moving.map(|m| m.layout).ok_or(b.reason),
        (None, None) if !resolved => Err(STILL_LOADING.into()),
        (None, None) => Err("No mods folder selected".into()),
    }
}

/// The folder "Point FS25 here" writes into FS25's setting: the managed folder, or — while
/// FS25's own setting is what blocks it — the folder the user chose.
fn game_point_target(
    active: Option<&modsroot::Active>,
    blocked: Option<&modsroot::Blocked>,
) -> Result<PathBuf, String> {
    match (active, blocked) {
        (Some(a), _) => Ok(a.layout.mods.clone()),
        (None, Some(b)) if b.cause == modsroot::BlockCause::GameSetting => Ok(b.mods.clone()),
        (None, Some(b)) => Err(b.reason.clone()),
        (None, None) => Err("No mods folder selected".into()),
    }
}

/// The trusted layout for mods folder `mods`: during an unfinished move its archive is where
/// the move started (the other one is `move_to`); otherwise the stored custom archive, or the
/// default. A stored archive that fails `Layout::new`'s checks is ignored (default used).
fn resolve_active(conn: &rusqlite::Connection, mods: &std::path::Path) -> modsroot::Active {
    let key = modsroot::canonical_key(mods);
    let marker = relocate::load_marker(conn, &key);
    let stored = match &marker {
        Some(m) => Some(m.from.clone()),
        None => match db::get_app_setting(conn, &relocate::archive_setting_key(&key)) {
            Some(s) => Some(PathBuf::from(s)),
            // No setting: a lost database may have forgotten a custom archive the notes name.
            None => relocate::rediscover_archive(mods)
                .filter(|found| rediscovered_archive_fits(conn, mods, found))
                .inspect(|found| {
                    if relocate::store_archive_setting(conn, &key, mods, found).is_ok() {
                        notices::push(format!(
                            "Silo found your library archive at {} and is using it again.",
                            found.display()
                        ));
                    }
                }),
        },
    };
    let layout = match stored {
        Some(archive) => organize::Layout::new(mods.to_path_buf(), archive).unwrap_or_else(|e| {
            log::warn!(
                "ignoring the library archive stored for {}: {e}",
                mods.display()
            );
            organize::Layout::default_for(mods)
        }),
        None => organize::Layout::default_for(mods),
    };
    modsroot::Active {
        layout,
        move_to: marker.map(|m| m.to),
    }
}

/// An archive found again from its notes passes the same checks as one the user picks: the
/// `Layout` rules and the fences (other mods folders and their archives, FS25's own folder,
/// Silo's data folder, which is where the database lives).
fn rediscovered_archive_fits(
    conn: &rusqlite::Connection,
    mods: &std::path::Path,
    found: &std::path::Path,
) -> bool {
    let layout = organize::Layout::default_for(mods);
    let app_data = conn
        .path()
        .map(std::path::Path::new)
        .and_then(std::path::Path::parent)
        .filter(|p| !p.as_os_str().is_empty());
    let problems = match fences(conn, &layout, app_data) {
        Ok(f) => fence_problems(&layout, found, &f),
        Err(e) => vec![e],
    };
    if !problems.is_empty() {
        log::warn!(
            "not using the archive {} named in {}: {}",
            found.display(),
            relocate::ARCHIVE_NOTE,
            problems.join(" ")
        );
    }
    problems.is_empty()
}

/// Serializes `compute_mods_root_status`, from reading the stored choice to publishing what it
/// resolved: a status that read the old choice can't publish after one that read the new choice.
/// Lock order: the library lock (when held) first, then this one; nothing holding this one
/// takes the library lock. `launch_game` holds it too, from reading the published FS25 profile
/// to starting the game (#387), so the profile it starts is never half switched.
static MODS_ROOT_RESOLVE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Resolve the live mods folder, publish it (and its archive) as the trusted write roots, and
/// describe it. Callers hold the library lock, so the trusted roots never change under a write
/// (a refresh that may not hold it uses `refresh_mods_root`).
fn compute_mods_root_status(conn: &rusqlite::Connection) -> ModsRootStatus {
    resolve_mods_root(conn, true)
}

/// `compute_mods_root_status`, publishing only when `publish` (else it just describes what it
/// would publish).
fn resolve_mods_root(conn: &rusqlite::Connection, publish: bool) -> ModsRootStatus {
    let _resolving = MODS_ROOT_RESOLVE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let loc = fsgame::locate();
    let default = loc.as_ref().map(|l| l.user_dir.join("mods"));
    let map = loc.as_ref().map(path_map).unwrap_or_default();
    let (ov, game_setting_problem) = match loc.as_ref() {
        None => (
            None,
            Some("Silo can't find your Farming Simulator 25 user folder.".to_string()),
        ),
        Some(l) => match std::fs::read_to_string(l.user_dir.join("gameSettings.xml")) {
            Err(_) => (
                None,
                Some(
                    "Start Farming Simulator 25 once so it creates its settings, then try again."
                        .to_string(),
                ),
            ),
            Ok(xml) => match modsroot::read_override(&xml) {
                Some(o) => (Some(o), None),
                None => (
                    None,
                    Some("The game's settings don't have a mods folder entry yet.".to_string()),
                ),
            },
        },
    };
    // Rows from before folders were tracked belong to the default folder: it was the only one.
    if let Some(d) = default.as_ref() {
        let _ = db::claim_unscoped(conn, &modsroot::canonical_key(d));
    }
    let choice = db::get_app_setting(conn, modsroot::CHOICE_KEY).map(PathBuf::from);
    let r = modsroot::resolve(
        choice,
        ov.as_ref(),
        default.clone(),
        &map,
        modsroot::same_folder,
    );
    let available = r.path.as_ref().map(|p| p.is_dir()).unwrap_or(false);
    let mods_folder_missing = !available
        && r.source == modsroot::Source::Default
        && loc.as_ref().map(|l| l.user_dir.is_dir()).unwrap_or(false);
    // The folder's FS25 profile (#387): when it's on, Play starts FS25 on it and the game's own
    // files are read from it (`gameprofile::Place`).
    let game_profile = r
        .path
        .as_deref()
        .and_then(|p| game_profile_for(conn, p, loc.as_ref().map(|l| l.user_dir.as_path())));
    // A refresh that doesn't publish describes the launch setting the backend actually uses
    // (the published one), so the status never claims a profile Play wouldn't start.
    let game_profile = game_profile.map(|mut g| {
        if !publish {
            g.enabled = gameprofile::active()
                .is_some_and(|d| modsroot::same_folder(&d, std::path::Path::new(&g.dir)));
        }
        g
    });
    // A folder that belongs to a profile, even with its launch off, never gets the main
    // profile's setting changed for it.
    let profile = game_profile
        .as_ref()
        .filter(|g| gameprofile::belongs(g))
        .map(|g| {
            let dir = PathBuf::from(&g.dir);
            // Only where Silo launches profiles (Windows): elsewhere (Proton) its setting is in the
            // game's drive letters, which Silo doesn't translate for a profile, so it isn't judged.
            let reads = if gameprofile::SUPPORTED {
                profile_game_reads(&dir).map(Some)
            } else {
                Ok(None)
            };
            (dir, g.enabled, reads)
        });
    let profile_dir = profile
        .as_ref()
        .filter(|(_, launch, _)| *launch)
        .map(|(d, _, _)| d.clone());
    let profile_reads = profile
        .as_ref()
        .and_then(|(_, _, reads)| reads.as_ref().ok().cloned().flatten());
    // The folder the main FS25 profile reads counts as a known mods folder for every overlap
    // check, whichever profile is in use: it stays a mods folder of its own. So does the one
    // the folder's own profile reads (`profile_reads`, published with the profile below).
    if publish {
        modsroot::set_game_reads(r.game_reads.clone());
    }
    // Where the folder's profile has FS25 read mods can't be told (its settings can't be read):
    // that folder can't be fenced off, so nothing is written until it can (`Try again`).
    let profile_unknown = profile
        .as_ref()
        .and_then(|(_, _, reads)| reads.as_ref().err().cloned());
    let (active, blocked) = match r.path.as_deref() {
        Some(p) if available && profile_unknown.is_some() => {
            let reason = profile_unknown.clone().unwrap_or_default();
            log::warn!("not managing {}: {reason}", p.display());
            (
                None,
                Some(modsroot::Blocked {
                    reason,
                    cause: modsroot::BlockCause::ReadError,
                    mods: p.to_path_buf(),
                    conflict: None,
                    moving: unfinished_move(conn, p).map(Box::new),
                }),
            )
        }
        Some(p) if available => match resolve_layout(
            conn,
            p,
            default.as_deref(),
            r.game_reads.as_deref(),
            profile_reads.as_deref(),
            // "Point FS25 at this folder" is never the fix for a profile folder: it would write
            // the main profile's setting. Its overlap is reported as an overlap instead.
            r.source == modsroot::Source::User && profile.is_none(),
        ) {
            Ok(a) => (Some(a), None),
            Err(b) => {
                log::warn!("not managing {}: {}", p.display(), b.reason);
                (None, Some(b))
            }
        },
        _ => (None, None),
    };
    let layout_problem = blocked.as_ref().map(|b| b.reason.clone());
    let layout_problem_cause = blocked.as_ref().map(|b| b.cause);
    let layout_conflict = blocked
        .as_ref()
        .and_then(|b| b.conflict.as_ref())
        .map(|c| c.to_string_lossy().into_owned());
    // Together, so no command sees "no layout" without its reason. The profile first: Play waits
    // for `modsroot::resolved()`, which this publish sets, so it never starts the main profile
    // for a folder whose profile isn't published yet.
    if publish {
        gameprofile::publish(
            profile
                .as_ref()
                .map(|(dir, launch, _)| gameprofile::Published {
                    dir: dir.clone(),
                    launch: *launch,
                    reads: profile_reads.clone(),
                }),
        );
        modsroot::publish(active, blocked);
    }

    let current_key = r.path.as_ref().map(|p| modsroot::canonical_key(p));
    let mut paths: Vec<PathBuf> =
        modsroot::parse_known(db::get_app_setting(conn, modsroot::KNOWN_KEY).as_deref())
            .into_iter()
            .map(PathBuf::from)
            .collect();
    for p in default.iter().chain(r.path.iter()) {
        if p.is_dir() && !paths.iter().any(|k| modsroot::same_folder(k, p)) {
            paths.push(p.clone());
        }
    }
    let known = paths
        .iter()
        .map(|p| {
            let key = modsroot::canonical_key(p);
            KnownRoot {
                path: p.to_string_lossy().into_owned(),
                mod_count: known_mod_count(
                    &db::load_organized(conn, &key),
                    &scan::root_mod_names(p),
                ),
                current: current_key.as_deref() == Some(key.as_str()),
            }
        })
        .collect();

    // On a profile that launches, what FS25 reads is that profile's mods folder (unknown, and
    // said so, when its settings can't be read). For any profile folder the main profile's
    // setting is not Silo's to change (no "Point FS25 here").
    let (game_reads, mismatch, can_write_game_setting, game_setting_problem) =
        match profile.as_ref() {
            Some((_, true, Ok(Some(reads)))) => {
                let mismatch = r
                    .path
                    .as_deref()
                    .is_some_and(|p| !modsroot::same_folder(p, reads));
                (Some(reads.clone()), mismatch, false, None)
            }
            Some((_, true, Err(why))) => (None, false, false, Some(why.clone())),
            Some((_, false, _) | (_, true, Ok(None))) => (r.game_reads, r.mismatch, false, None),
            None => (
                r.game_reads,
                r.mismatch,
                game_setting_problem.is_none(),
                game_setting_problem,
            ),
        };

    ModsRootStatus {
        path: r.path.map(|p| p.to_string_lossy().into_owned()),
        source: r.source,
        game_reads: game_reads.map(|p| p.to_string_lossy().into_owned()),
        mismatch,
        available,
        mods_folder_missing,
        known,
        can_write_game_setting,
        game_setting_problem,
        // The main profile's setting: not what FS25 reads on another profile.
        unresolved_override: r.unresolved_override.filter(|_| profile_dir.is_none()),
        layout_problem,
        layout_problem_cause,
        layout_conflict,
        game_profile,
    }
}

/// Point FS25's `modsDirectoryOverride` at `folder`. `guarded_xml_write` keeps the untouched
/// original (`gameSettings.xml.silo-original.bak`) and the last three versions (`.bak`,
/// `.bak.2`, `.bak.3`), then swaps the new file in atomically.
///
/// This writes the main profile's `gameSettings.xml` (`gameprofile::Place::MainGameSettings`),
/// so it's refused while the mods folder belongs to its own FS25 profile (#387), with its launch
/// on or off: that profile has its own settings, and the main one must not be touched for it.
fn point_game_at(folder: &std::path::Path) -> Result<(), String> {
    main_settings_refusal(gameprofile::published().map(|p| p.dir).as_deref())?;
    let loc = fsgame::locate().ok_or_else(|| "Silo couldn't find Farming Simulator 25's user folder. Start the game once, then try again.".to_string())?;
    let gs = loc.user_dir.join("gameSettings.xml");
    let roots = [loc.user_dir.clone()];
    let xml = paths::guarded_xml_read(&roots, &gs)?;
    let out = modsroot::write_override(&xml, &path_map(&loc).to_game(folder))?;
    paths::guarded_xml_write(&roots, &gs, &out)
}

/// Held through a config write into the FS25 user folder that follows the profile (#387): the
/// folder's profile can't be published in between, so the folder a write is confined to is the
/// one in use when it lands (one queued from before a switch is refused by its confinement).
/// Same lock as the mods-folder resolution; nothing holding it takes the library lock.
fn game_files_guard() -> std::sync::MutexGuard<'static, ()> {
    MODS_ROOT_RESOLVE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

const SAVES_CHANGED: &str =
    "Your savegames changed with this mods folder's FS25 profile. Look at the list again.";

/// Pure: refuse a write to the main profile's `gameSettings.xml` while `profile` is in use.
fn main_settings_refusal(profile: Option<&std::path::Path>) -> Result<(), String> {
    match profile {
        Some(_) => Err(PROFILE_KEEPS_MAIN_SETTINGS.into()),
        None => Ok(()),
    }
}

const PROFILE_KEEPS_MAIN_SETTINGS: &str ="This mods folder belongs to its own Farming Simulator 25 profile, so Silo leaves the main game settings alone.";

/// Turn "Launch FS25 with this folder's profile" on or off for the current mods folder (#387).
/// Stored per folder; the status (and the published profile) follows at once. Changes only
/// which FS25 user folder Silo reads and which one Play starts: no file is touched.
#[tauri::command]
async fn set_game_profile(
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<ModsRootStatus, error::AppError> {
    ensure_no_move()?;
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || -> Result<ModsRootStatus, String> {
        // Publishes a new profile beside the same layout: same lock as a folder switch.
        let _write = mods_root_switch_lock()?;
        let conn = db::open(&db)?;
        let st = resolve_mods_root(&conn, false);
        let (Some(path), Some(_)) = (st.path.as_deref(), st.game_profile.as_ref()) else {
            return Err(
                "This mods folder isn't inside a separate Farming Simulator 25 profile.".into(),
            );
        };
        if let Some(why) = gameprofile::UNSUPPORTED {
            return Err(why.into());
        }
        let value = if enabled {
            gameprofile::ON
        } else {
            gameprofile::OFF
        };
        for k in profile_setting_keys(std::path::Path::new(path)) {
            if enabled {
                // Turned on by the user: this folder is a profile's from now on, so turning the
                // launch off later keeps the main settings protected (`gameprofile::belongs`).
                db::set_app_setting(&conn, &gameprofile::seen_key(&k), Some(gameprofile::ON))?;
            }
            db::set_app_setting(&conn, &gameprofile::setting_key(&k), Some(value))?;
        }
        Ok(compute_mods_root_status(&conn))
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(error::AppError::from)
}

/// The live mods folder and everything the Settings / banners need to describe it.
#[tauri::command]
async fn mods_root_status(app: tauri::AppHandle) -> Result<ModsRootStatus, String> {
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let conn = db::open(&db)?;
        refresh_mods_root(&conn)
    })
    .await
    .map_err(|e| e.to_string())?
}

const STATUS_WAITS: &str =
    "Silo is busy with your library. The mods folder status updates when it's done.";

/// Resolve and describe the mods folder for a refresh (the status command, startup). A refresh
/// can pick up a change of FS25's own folder setting, so it publishes only under the library
/// lock (`status_publish_lock`), like a switch, or before the first resolution (`may_publish`);
/// otherwise it only describes — and only when that describes the folder and FS25 profile in
/// use. A different one (FS25's own setting changed meanwhile) is refused (`STATUS_WAITS`):
/// the app never shows a folder or profile Play, Diagnose and the game-file commands don't use
/// (#387).
fn refresh_mods_root(conn: &rusqlite::Connection) -> Result<ModsRootStatus, String> {
    let lock = status_publish_lock();
    let publish = may_publish(lock.is_some(), modsroot::resolved());
    if !publish {
        log::info!("mods folder status read while the library is busy; not republished");
    }
    let st = resolve_mods_root(conn, publish);
    if !publish {
        let (active, blocked) = modsroot::published();
        let folder = active
            .map(|a| a.layout.mods)
            .or_else(|| blocked.map(|b| b.mods));
        let profile = gameprofile::published().map(|p| p.dir);
        let candidate = st.game_profile.as_ref().map(|g| PathBuf::from(&g.dir));
        if status_differs(
            st.path.as_deref(),
            folder.as_deref(),
            candidate.as_deref(),
            profile.as_deref(),
        ) {
            return Err(STATUS_WAITS.into());
        }
    }
    Ok(st)
}

/// Pure: whether a described status names another mods folder than the published `folder`
/// (when one is), or another FS25 profile than the published `profile` (`candidate`: the
/// described folder's profile, if it can be one).
fn status_differs(
    path: Option<&str>,
    folder: Option<&std::path::Path>,
    candidate: Option<&std::path::Path>,
    profile: Option<&std::path::Path>,
) -> bool {
    let other_folder = match (path, folder) {
        (Some(p), Some(f)) => !modsroot::same_folder(std::path::Path::new(p), f),
        (None, Some(_)) => true,
        _ => false,
    };
    let other_profile = match (candidate, profile) {
        (_, None) => false,
        (Some(c), Some(p)) => !modsroot::same_folder(c, p),
        (None, Some(_)) => true,
    };
    other_folder || other_profile
}

/// Pure: whether a refresh may publish: holding the library lock, or before anything was ever
/// resolved (every write refuses until then, so nothing can rely on the trusted roots yet).
fn may_publish(locked: bool, resolved: bool) -> bool {
    locked || !resolved
}

/// The library lock for a status refresh: waits behind an ordinary write (up to `LIBRARY_WAIT`)
/// but not behind an archive move, which republishes the layout itself when it ends. Doesn't
/// move the epoch on: a refresh that finds the same folder changes nothing a scan relies on,
/// and a scan's manifest is keyed by the folder it scanned. `None` when the lock isn't had.
fn status_publish_lock() -> Option<std::sync::MutexGuard<'static, ()>> {
    let start = std::time::Instant::now();
    loop {
        if move_in_progress() {
            return None;
        }
        if let Some(guard) = lock_result(LIBRARY_WRITE.try_lock()) {
            return Some(guard);
        }
        if start.elapsed() >= LIBRARY_WAIT {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// The library lock for a command that switches (or re-publishes) the mods folder: no write may
/// run on the old trusted roots while the new ones are stored and published, and taking the
/// lock moves the epoch on, so a scan that began on the old folder doesn't reconcile. Refused
/// during an archive move or a self-update like `library_write`, but update leftovers aren't
/// settled first: they belong to the folder being left and mustn't keep the user from leaving
/// it (the next write there settles them).
fn mods_root_switch_lock() -> Result<std::sync::MutexGuard<'static, ()>, String> {
    wait_for_library(LIBRARY_WAIT, || {
        ensure_not_self_updating()?;
        ensure_no_move()
    })
}

/// Use `path` as the mods folder (remembered, added to the known list). Never moves files.
#[tauri::command]
async fn set_mods_root(
    app: tauri::AppHandle,
    path: String,
) -> Result<ModsRootStatus, error::AppError> {
    ensure_no_move()?;
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || -> Result<ModsRootStatus, String> {
        let _write = mods_root_switch_lock()?;
        let conn = db::open(&db)?;
        let p = std::path::Path::new(&path);
        let default =
            gameprofile::user_dir(gameprofile::Place::DefaultModsFolder).map(|d| d.join("mods"));
        let picked = picked::allows(picked::Purpose::ModsFolder, p);
        apply_mods_root_choice(
            &conn,
            &path,
            picked,
            default.as_deref(),
            &live_mods_folders(),
        )?;
        // Used: a later call can't replay it (the folder is in the known list now anyway).
        picked::forget(picked::Purpose::ModsFolder);
        Ok(compute_mods_root_status(&conn))
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(error::AppError::from)
}

/// `set_mods_root` without the picker and the status: refuse a folder that wasn't picked and
/// isn't known (or FS25's default), then one that shares files with another layout (`live`:
/// the folders in use, see `live_mods_folders`) — both before the write probe — then remember
/// `path` as known and chosen.
fn apply_mods_root_choice(
    conn: &rusqlite::Connection,
    path: &str,
    picked: bool,
    default: Option<&std::path::Path>,
    live: &[PathBuf],
) -> Result<(), String> {
    let p = std::path::Path::new(path);
    let known: Vec<PathBuf> =
        modsroot::parse_known(db::get_app_setting(conn, modsroot::KNOWN_KEY).as_deref())
            .into_iter()
            .map(PathBuf::from)
            .collect();
    if !mods_root_choice_allowed(p, picked, &known, default) {
        return Err(picked::NOT_PICKED.into());
    }
    let mut extra: Vec<PathBuf> = default.map(PathBuf::from).into_iter().collect();
    extra.extend(live.iter().cloned());
    let all = known_mods_folders(conn, &extra);
    if let Some(why) = mods_folder_check(conn, p, &all).map_err(|e| folder_check_unreadable(&e))? {
        return Err(why.message);
    }
    modsroot::validate_folder(p)?;
    let known = modsroot::add_known(
        db::get_app_setting(conn, modsroot::KNOWN_KEY).as_deref(),
        path,
    );
    db::set_app_setting(conn, modsroot::KNOWN_KEY, Some(&known))?;
    db::set_app_setting(conn, modsroot::CHOICE_KEY, Some(path))
}

/// Forget the user's choice and follow the folder FS25 reads.
#[tauri::command]
async fn clear_mods_root(app: tauri::AppHandle) -> Result<ModsRootStatus, error::AppError> {
    ensure_no_move()?;
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || -> Result<ModsRootStatus, String> {
        let _write = mods_root_switch_lock()?;
        let conn = db::open(&db)?;
        db::set_app_setting(&conn, modsroot::CHOICE_KEY, None)?;
        Ok(compute_mods_root_status(&conn))
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(error::AppError::from)
}

/// Point FS25 at the folder Silo currently manages — or at the folder the user chose, while
/// FS25's own setting is what blocks it (`game_point_target`).
#[tauri::command]
async fn point_game_at_mods_root(app: tauri::AppHandle) -> Result<ModsRootStatus, error::AppError> {
    ensure_no_move()?;
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || -> Result<ModsRootStatus, String> {
        // Pointing FS25 at the folder can unblock it, which publishes a new layout.
        let _write = mods_root_switch_lock()?;
        // Checked once the lock is held: FS25 may have started while this waited for it.
        config_write_refusal(gamelaunch::fs25_running())?;
        let folder = game_point_target(
            modsroot::active().as_ref(),
            modsroot::blocked_detail().as_ref(),
        )?;
        let conn = db::open(&db)?;
        // Classified again now, not only as last published: FS25 may have made the folder's
        // parent a profile since (#387). A profile's folder never gets the main setting changed.
        let main = fsgame::user_dir();
        let fresh = game_profile_for(&conn, &folder, main.as_deref())
            .filter(gameprofile::belongs)
            .map(|g| PathBuf::from(g.dir));
        main_settings_refusal(fresh.as_deref())?;
        point_game_at(&folder)?;
        Ok(compute_mods_root_status(&conn))
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(error::AppError::from)
}

/// Create `<user dir>/mods` when FS25 hasn't made it yet. The main user folder's
/// (`gameprofile::Place::DefaultModsFolder`): it's only offered for the default folder.
#[tauri::command]
async fn create_mods_folder(app: tauri::AppHandle) -> Result<ModsRootStatus, error::AppError> {
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || -> Result<ModsRootStatus, String> {
        // The new folder becomes the published layout.
        let _write = mods_root_switch_lock()?;
        let conn = db::open(&db)?;
        // Only while Silo is on the game's default folder and it's missing (the only time the
        // offer shows): never for another folder, an FS25 profile's included (#387). Resolved
        // and published here under the switch lock, so what's checked is what's in use.
        if !compute_mods_root_status(&conn).mods_folder_missing
            || gameprofile::published().is_some()
        {
            return Err("There's no missing mods folder to create.".into());
        }
        let loc = fsgame::locate().ok_or_else(|| "Silo couldn't find Farming Simulator 25's user folder. Start the game once, then try again.".to_string())?;
        std::fs::create_dir_all(loc.user_dir.join("mods")).map_err(|e| e.to_string())?;
        Ok(compute_mods_root_status(&conn))
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(error::AppError::from)
}

/// True when a folder holds no mods yet (no `.zip`, no folder with a `modDesc.xml`, no
/// `archive/`), so the UI can ask "use it anyway?".
#[tauri::command]
async fn folder_looks_empty(path: String) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let dir = std::path::Path::new(&path);
        let rd = std::fs::read_dir(dir).map_err(|e| e.to_string())?;
        let has_mod = rd.flatten().any(|e| {
            let p = e.path();
            e.file_name() == "archive"
                || p.extension()
                    .map(|x| x.eq_ignore_ascii_case("zip"))
                    .unwrap_or(false)
                || p.join("modDesc.xml").is_file()
        });
        Ok(!has_mod)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Show the native folder picker and remember the choice for `purpose` (see `picked`).
#[tauri::command]
async fn pick_folder(
    window: tauri::Window,
    purpose: picked::Purpose,
) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    // `blocking_pick_folder` posts the dialog to the main thread and waits for the answer, so
    // it must never run there; `off_main` runs it on the blocking pool.
    off_main(move || {
        let builder = window.dialog().file();
        // Modal to Silo's window where the platform supports it (as the JS dialog does).
        #[cfg(any(windows, target_os = "macos"))]
        let builder = builder.set_parent(&window);
        let Some(fp) = builder.blocking_pick_folder() else {
            return Ok(None);
        };
        let path = fp.into_path().map_err(|e| e.to_string())?;
        picked::remember(purpose, &path);
        Ok(Some(path.to_string_lossy().into_owned()))
    })
    .await
}

/// The mods folder may change to a folder picked in Silo's picker, one Silo already knows (the
/// known list, which only `set_mods_root` adds to), or FS25's default folder.
fn mods_root_choice_allowed(
    path: &std::path::Path,
    picked: bool,
    known: &[PathBuf],
    default: Option<&std::path::Path>,
) -> bool {
    picked
        || default.is_some_and(|d| modsroot::same_folder(d, path))
        || known.iter().any(|k| modsroot::same_folder(k, path))
}

const FOLDER_MISSING: &str = "That folder isn't there right now.";

/// The folders Settings can open, named by the webview, resolved by Rust.
#[derive(serde::Deserialize, Clone, Copy)]
#[serde(rename_all = "camelCase")]
enum KnownFolder {
    ModsFolder,
    Archive,
    GameInstall,
    /// Where the log plugin writes `silo.log` (`app_log_dir()`, see `log_plugin`).
    Logs,
}

fn existing_dir(p: Option<PathBuf>) -> Result<PathBuf, String> {
    p.filter(|p| p.is_dir())
        .ok_or_else(|| FOLDER_MISSING.to_string())
}

/// The folder `which` names, if it exists. `log_dir` is the app's log folder (`app_log_dir()`),
/// passed in so this resolves without a running app.
fn known_folder_path(which: KnownFolder, log_dir: Option<PathBuf>) -> Result<PathBuf, String> {
    existing_dir(match which {
        KnownFolder::ModsFolder => modsroot::active().map(|a| a.layout.mods),
        KnownFolder::Archive => modsroot::active().map(|a| a.layout.archive),
        KnownFolder::GameInstall => gamelaunch::detect().map(|g| PathBuf::from(g.install_dir)),
        KnownFolder::Logs => log_dir,
    })
}

/// Open one of Silo's folders in the file manager. Rust picks the path; the webview only names
/// which folder, so it can't open (or run) anything else.
#[tauri::command]
async fn open_known_folder(app: tauri::AppHandle, which: KnownFolder) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let log_dir = profile::app_log_dir(&app).ok();
    // Resolving the game folder reads Steam's files, and the shell call can stall: both off main.
    off_main(move || {
        let path = known_folder_path(which, log_dir)?;
        app.opener()
            .open_path(path.to_string_lossy(), None::<&str>)
            .map_err(|e| format!("Couldn't open {}: {e}", path.display()))
    })
    .await
}

// ── Library archive location (#166) ──

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ArchiveStatus {
    /// The active archive — during an unfinished move, where the move started.
    path: Option<String>,
    is_default: bool,
    /// False when a custom archive's folder is missing (its drive unplugged).
    available: bool,
    /// The unfinished move (paused, or copying right now — see `running`).
    moving: Option<relocate::MoveMarker>,
    /// A move is copying right now (Pause applies, Launch is blocked). `moving` set with
    /// `running` false means the move is paused (Resume / Cancel apply).
    running: bool,
}

/// The active layout and its mods-folder key, or why there isn't one yet.
fn active_layout() -> Result<(organize::Layout, String), String> {
    blocked_first(modsroot::blocked(), || match modsroot::active() {
        Some(a) => {
            let key = modsroot::canonical_key(&a.layout.mods);
            Ok((a.layout, key))
        }
        None if !modsroot::resolved() => Err(STILL_LOADING.into()),
        None => Err("No mods folder selected".into()),
    })
}

/// A folder's identity key as a path, for component-wise `starts_with` comparisons.
fn key_path(p: &std::path::Path) -> PathBuf {
    PathBuf::from(modsroot::canonical_key(p))
}

/// Folders a new archive must keep clear of, as identity keys (`key_path`).
struct Fences {
    /// The active mods folder.
    mods: PathBuf,
    current_archive: PathBuf,
    /// Every known mods folder except the active one (the active one is `path_problems`').
    other_mods: Vec<PathBuf>,
    /// Those folders' archives (chosen, or their default).
    other_archives: Vec<PathBuf>,
    user_dir: Option<PathBuf>,
    /// The FS25 profile folders of the mods folders that belong to one (#387): each is an FS25
    /// user folder of its own, fenced like `user_dir`.
    profile_dirs: Vec<PathBuf>,
    app_data: Option<PathBuf>,
}

/// Every mods folder Silo knows: the known list, plus `extra` — FS25's default mods folder and
/// the folders in use right now (`live_mods_folders`), which count even when never picked.
fn known_mods_folders(conn: &rusqlite::Connection, extra: &[PathBuf]) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> =
        modsroot::parse_known(db::get_app_setting(conn, modsroot::KNOWN_KEY).as_deref())
            .into_iter()
            .map(PathBuf::from)
            .collect();
    v.extend(extra.iter().cloned());
    v
}

/// The mods folders in use right now: the folder FS25 reads (its override), the one the current
/// folder's FS25 profile reads, and the one Silo manages, as last resolved.
fn live_mods_folders() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = modsroot::game_reads().into_iter().collect();
    v.extend(modsroot::active().map(|a| a.layout.mods));
    // The folder FS25 reads on the current folder's own FS25 profile (#387).
    v.extend(gameprofile::published().and_then(|p| p.reads));
    v
}

/// A mods folder's archive: the one chosen for it, or the default `<mods>/archive`.
fn archive_of(conn: &rusqlite::Connection, mods: &std::path::Path) -> PathBuf {
    db::get_app_setting(
        conn,
        &relocate::archive_setting_key(&modsroot::canonical_key(mods)),
    )
    .map(PathBuf::from)
    .unwrap_or_else(|| organize::Layout::default_for(mods).archive)
}

/// The fences around `layout` (see `Fences`). `other_archives` is every archive Silo knows
/// except this folder's own (`other_archives`): each other known folder's, and every stored
/// archive choice — also one for a folder Silo only ever followed through FS25's override,
/// which never joined the known list, so a re-point can't make two layouts share an archive.
/// An unreadable setting list is an error: the fences must never pass because Silo couldn't
/// look.
fn fences(
    conn: &rusqlite::Connection,
    layout: &organize::Layout,
    app_data: Option<&std::path::Path>,
) -> Result<Fences, String> {
    // The main default mods folder (`gameprofile::Place::DefaultModsFolder`) is always fenced off.
    let user_dir = gameprofile::user_dir(gameprofile::Place::DefaultModsFolder);
    let mut extra: Vec<PathBuf> = user_dir.iter().map(|u| u.join("mods")).collect();
    extra.extend(live_mods_folders());
    fences_with(conn, layout, &extra, user_dir.as_deref(), app_data)
}

/// `fences` with the folders that count as known besides the list (`extra`: FS25's default
/// mods folder, the folders in use) and FS25's user folder passed in (tests).
fn fences_with(
    conn: &rusqlite::Connection,
    layout: &organize::Layout,
    extra: &[PathBuf],
    user_dir: Option<&std::path::Path>,
    app_data: Option<&std::path::Path>,
) -> Result<Fences, String> {
    let others = with_layout_owners(conn, known_mods_folders(conn, extra))
        .map_err(|e| archive_list_unreadable(&e))?;
    let mine = key_path(&layout.mods);
    let mut other_mods: Vec<PathBuf> = Vec::new();
    for p in &others {
        let k = key_path(p);
        if k != mine && !other_mods.contains(&k) {
            other_mods.push(k);
        }
    }
    let other_archives = other_archives(conn, &layout.mods, &others)
        .map_err(|e| archive_list_unreadable(&e))?
        .iter()
        .map(|a| key_path(a))
        .collect();
    Ok(Fences {
        mods: mine,
        current_archive: key_path(&layout.archive),
        other_mods,
        other_archives,
        user_dir: user_dir.map(key_path),
        profile_dirs: profile_dirs_of(conn, &layout.mods, &others, user_dir),
        app_data: app_data.map(key_path),
    })
}

/// The FS25 profile folders (as `key_path`s, once each) of `mods` and of `others` where that
/// folder belongs to a profile (`gameprofile::belongs`: its launch is on, or it is or was seen
/// as one), judged against the main user folder `main_user_dir` (#387).
fn profile_dirs_of(
    conn: &rusqlite::Connection,
    mods: &std::path::Path,
    others: &[PathBuf],
    main_user_dir: Option<&std::path::Path>,
) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    for p in std::iter::once(mods).chain(others.iter().map(PathBuf::as_path)) {
        let Some(g) = game_profile_for(conn, p, main_user_dir).filter(gameprofile::belongs) else {
            continue;
        };
        let k = key_path(std::path::Path::new(&g.dir));
        if !v.contains(&k) {
            v.push(k);
        }
    }
    v
}

/// Why a folder can't be checked as a mods folder: Silo couldn't read what it knows of the
/// other mods folders and their archives.
/// The detail goes to the log; the message stays in plain words.
fn folder_check_unreadable(e: &str) -> String {
    log::warn!("couldn't check a mods folder against the other layouts: {e}");
    FOLDER_CHECK_UNREADABLE.into()
}

const FOLDER_CHECK_UNREADABLE: &str =
    "Silo couldn't check this folder against your other mods folders. Try again.";

/// Why a folder can't be checked as an archive: Silo couldn't read the archives it knows.
/// The detail goes to the log; the message stays in plain words.
fn archive_list_unreadable(e: &str) -> String {
    log::warn!("couldn't read the stored archives: {e}");
    ARCHIVE_LIST_UNREADABLE.into()
}

const ARCHIVE_LIST_UNREADABLE: &str =
    "Silo couldn't read its list of archives, so it can't check this folder. Try again.";

/// Pure: `a` and `b` (identity keys) are the same folder, or one is inside the other.
fn overlaps(a: &std::path::Path, b: &std::path::Path) -> bool {
    a == b || a.starts_with(b) || b.starts_with(a)
}

/// Pure: one of `a` and `b` (identity keys) is inside the other (not the same folder).
fn nests(a: &std::path::Path, b: &std::path::Path) -> bool {
    a != b && overlaps(a, b)
}

/// A known mods folder's archive, if it has one worth fencing: its stored choice, or the
/// default `<X>/archive` once Silo has managed the folder (organized rows) or that folder
/// exists. A folder Silo never managed — say one only FS25's override points at — has no
/// archive yet, so only the folder itself counts.
fn managed_archive(conn: &rusqlite::Connection, mods: &std::path::Path) -> Option<PathBuf> {
    let key = modsroot::canonical_key(mods);
    if let Some(s) = db::get_app_setting(conn, &relocate::archive_setting_key(&key)) {
        return Some(PathBuf::from(s));
    }
    let default = organize::Layout::default_for(mods).archive;
    (db::count_organized(conn, &key) > 0 || default.exists()).then_some(default)
}

/// Every archive Silo knows of except `mods`' own: each stored archive choice
/// (`archive_root:*`, whichever mods folder it belongs to), and the `managed_archive` of each
/// other known mods folder and of each other folder Silo has organized mods for (one it only
/// followed through FS25's override never joins the known list, and its default `<X>/archive`
/// has no stored setting). One entry per folder (by identity key), spelled as stored. An
/// unreadable setting or library list is an error: the check must never pass because it
/// couldn't look.
fn other_archives(
    conn: &rusqlite::Connection,
    mods: &std::path::Path,
    known: &[PathBuf],
) -> Result<Vec<PathBuf>, String> {
    let mine = modsroot::canonical_key(mods);
    let mut v: Vec<PathBuf> = Vec::new();
    let stored = db::app_settings_with_prefix(conn, relocate::ARCHIVE_SETTING_PREFIX)?;
    for (k, archive) in stored {
        if k != relocate::archive_setting_key(&mine) {
            v.push(PathBuf::from(archive));
        }
    }
    for k in known {
        if modsroot::canonical_key(k) != mine {
            v.extend(managed_archive(conn, k));
        }
    }
    for root in db::organized_roots(conn)? {
        if root != mine {
            v.extend(managed_archive(conn, std::path::Path::new(&root)));
        }
    }
    let mut seen = HashSet::new();
    v.retain(|p| seen.insert(key_path(p)));
    Ok(v)
}

/// `known` plus every other folder Silo has a layout for: each folder it has organized mods
/// for (`db::organized_roots`) and each folder with a stored archive choice (`archive_root:<its
/// key>`). A folder Silo only followed through FS25's override never joins the known list, but
/// its layout is still on disk. An unreadable library or setting list is an error.
fn with_layout_owners(
    conn: &rusqlite::Connection,
    mut known: Vec<PathBuf>,
) -> Result<Vec<PathBuf>, String> {
    known.extend(db::organized_roots(conn)?.into_iter().map(PathBuf::from));
    for (k, _) in db::app_settings_with_prefix(conn, relocate::ARCHIVE_SETTING_PREFIX)? {
        if let Some(owner) = k.strip_prefix(relocate::ARCHIVE_SETTING_PREFIX) {
            if !owner.is_empty() {
                known.push(PathBuf::from(owner));
            }
        }
    }
    Ok(known)
}

/// Why a folder can't be a mods folder, and the folder it runs into.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FolderProblem {
    message: String,
    conflict: PathBuf,
}

/// Pure: why `mods` can't be a mods folder — it overlaps another folder's archive (it is one,
/// a category inside one, or holds one), or its own archive (`own_archive`: chosen, or the
/// default `<mods>/archive`) would overlap another archive or another mods folder
/// (`other_mods`). Two layouts sharing files would shuffle each other's parked mods. All
/// identity keys; `conflict` is the one it runs into.
fn mods_folder_problem(
    mods: &std::path::Path,
    own_archive: &std::path::Path,
    archives: &[PathBuf],
    other_mods: &[PathBuf],
) -> Option<FolderProblem> {
    let hit = |list: &[PathBuf], p: &std::path::Path, message: &str| {
        list.iter().find(|a| overlaps(p, a)).map(|a| FolderProblem {
            message: message.into(),
            conflict: a.clone(),
        })
    };
    hit(archives, mods, OVERLAPS_AN_ARCHIVE)
        .or_else(|| {
            hit(
                archives,
                own_archive,
                "That folder's archive would be another mods folder's archive. Pick another folder.",
            )
        })
        .or_else(|| {
            hit(
                other_mods,
                own_archive,
                "That folder's archive would overlap another mods folder. Pick another folder.",
            )
        })
}

/// Why `mods` can't be a mods folder, given every mods folder Silo knows (`known`, which may
/// include `mods` itself): `mods_folder_problem` over the other layouts in the database, with
/// the conflicting folder spelled as Silo knows it. An unreadable database is an error, never
/// a pass.
fn mods_folder_check(
    conn: &rusqlite::Connection,
    mods: &std::path::Path,
    known: &[PathBuf],
) -> Result<Option<FolderProblem>, String> {
    let mine = key_path(mods);
    let known = with_layout_owners(conn, known.to_vec())?;
    let archives = other_archives(conn, mods, &known)?;
    let mut others: Vec<&PathBuf> = known.iter().filter(|k| key_path(k) != mine).collect();
    let mut seen = HashSet::new();
    others.retain(|p| seen.insert(key_path(p)));
    let shown: HashMap<PathBuf, PathBuf> = archives
        .iter()
        .chain(others.iter().copied())
        .map(|p| (key_path(p), p.clone()))
        .collect();
    let keys = |v: &mut dyn Iterator<Item = &PathBuf>| v.map(|p| key_path(p)).collect::<Vec<_>>();
    let problem = mods_folder_problem(
        &mine,
        &key_path(&archive_of(conn, mods)),
        &keys(&mut archives.iter()),
        &keys(&mut others.iter().copied()),
    );
    Ok(problem.map(|p| FolderProblem {
        conflict: shown.get(&p.conflict).cloned().unwrap_or(p.conflict),
        message: p.message,
    }))
}

const OVERLAPS_A_PROFILE: &str = "Pick a folder outside your Farming Simulator 25 profile folders.";

const OVERLAPS_AN_ARCHIVE: &str =
    "That folder overlaps another mods folder's archive. Pick a folder outside it.";

/// Pure: where a new archive (`new`, an identity key) runs into a folder it must keep clear
/// of. Equality with the current archive / another archive is `path_problems`' to report.
fn overlap_problems(new: &std::path::Path, f: &Fences) -> Vec<String> {
    if new.parent().is_none() {
        return vec!["Pick a folder, not a whole drive.".into()];
    }
    let overlaps = |a: &PathBuf| overlaps(new, a);
    let nests = |a: &PathBuf| nests(new, a);
    let mut v = Vec::new();
    if nests(&f.current_archive) {
        v.push(
            "Pick a folder that isn't inside your current archive and doesn't contain it.".into(),
        );
    }
    if f.other_mods.iter().any(overlaps) {
        v.push(
            "That folder overlaps another mods folder Silo knows. Pick a folder outside it.".into(),
        );
    }
    if f.other_archives.iter().any(nests) {
        v.push(OVERLAPS_AN_ARCHIVE.into());
    }
    // The FS25 user dir, a folder containing it, or anything inside it — except inside a
    // mods folder (the default `<mods>/archive`; a wrong spot there gets its own message).
    let in_mods = new.starts_with(&f.mods) || f.other_mods.iter().any(|m| new.starts_with(m));
    if !in_mods && f.user_dir.as_ref().is_some_and(overlaps) {
        v.push("Pick a folder outside Farming Simulator 25's own folder.".into());
    }
    // The same for a mods folder's own FS25 profile folder (#387): it's a user folder too.
    if !in_mods && f.profile_dirs.iter().any(overlaps) {
        v.push(OVERLAPS_A_PROFILE.into());
    }
    if f.app_data.as_ref().is_some_and(overlaps) {
        v.push("Pick a folder outside Silo's own data folder.".into());
    }
    v
}

fn foreign_mods_problem(n: usize) -> String {
    let (count, them) = if n == 1 {
        ("1 mod".to_string(), "it")
    } else {
        (format!("{n} mods"), "them")
    };
    format!(
        "This folder already has {count} in subfolders Silo doesn't manage. Choose another folder, or move {them} to the folder's top level so Silo can file {them} in."
    )
}

/// Where a folder can't be the archive at all: `path_problems` (already there, inside the mods
/// folder, another folder's archive, `Layout::new`'s rules) plus the `Fences` overlaps. Shared
/// by a move and a re-point. Mostly pure (`same_folder` canonicalizes).
fn fence_problems(layout: &organize::Layout, new: &std::path::Path, f: &Fences) -> Vec<String> {
    let mut v = relocate::path_problems(
        &layout.archive,
        new,
        &layout.mods,
        &f.other_archives,
        modsroot::same_folder,
    );
    v.extend(overlap_problems(&key_path(new), f));
    v
}

/// Everything about the folder itself that rules it out as a move's destination. Checked
/// before any probe writes a file there.
fn target_problems(
    conn: &rusqlite::Connection,
    layout: &organize::Layout,
    new: &std::path::Path,
    app_data: Option<&std::path::Path>,
) -> Vec<String> {
    let mut v = match fences(conn, layout, app_data) {
        Ok(f) => fence_problems(layout, new, &f),
        Err(e) => return vec![e],
    };
    // Mods filed in `<new>/<subfolder>/` that Silo doesn't know would mix with the library.
    // (Loose mods at the top level are fine — that's adopt.)
    match relocate::unknown_mods(new, &[]) {
        Ok(foreign) if !foreign.is_empty() => v.push(foreign_mods_problem(foreign.len())),
        Ok(_) => {}
        Err(e) => v.push(relocate::unreadable_archive_message(
            &e,
            "or choose another folder.",
        )),
    }
    v
}

/// The free-space facts a move depends on (see `move_problems`).
#[derive(Clone, Copy)]
struct Space {
    /// The old and new archive share a drive.
    archive_same_drive: bool,
    /// The old and new archive are known to be one volume, so each mod is renamed, not copied
    /// (`relocate::RunCtx::same_volume`): the archive needs no extra room. A rename that falls
    /// back to a copy checks its own room first.
    renames: bool,
    /// The whole archive.
    bytes: u64,
    /// The largest single mod.
    largest: u64,
    /// Free on the new archive's drive (unknown never blocks).
    free: Option<u64>,
    /// The largest mod that isn't `relink_kept`: with kept hardlinks, the move's peak is all
    /// of them plus one other mod's copy in flight.
    largest_unkept: u64,
    /// Active zips whose hardlink a move keeps as their standalone copy, because a new link
    /// into the new archive might be a copy (`LinkPolicy::can_only_copy`): on one drive their
    /// old bytes stay used. From `relocate::relink_cost`.
    relink_kept: u64,
    /// Active mods whose mods-folder entry a move makes again as a full copy there (zips where
    /// only a copy is possible, folders where a folder link may not work). From
    /// `relocate::relink_cost`.
    relink_copied: u64,
    /// The largest single mod in `relink_copied`.
    relink_largest_copied: u64,
    /// The mods folder is on the new archive's drive (known mounts only).
    mods_same_drive: bool,
    /// The old archive is on the mods folder's drive (known mounts only): each copy made in the
    /// mods folder is followed by deleting that mod's old copy there, so only one at a time
    /// needs room.
    old_on_mods_drive: bool,
    /// Free on the mods folder's drive (unknown never blocks).
    mods_free: Option<u64>,
}

/// Pure inputs to `move_problems`.
struct MoveFacts {
    /// Problems with the folder itself (`target_problems`, the write probe).
    target: Vec<String>,
    /// Mods in the archive the library doesn't know, phrased (`relocate::unknown_messages`).
    unknown: Vec<String>,
    /// Library mods whose archived copy is missing (the move would pause on each).
    missing: usize,
    /// Names already at the destination.
    collisions: Vec<String>,
    space: Space,
}

fn missing_mods_problem(n: usize) -> String {
    if n == 1 {
        "1 mod in your library is missing from the archive. Rescan first.".into()
    } else {
        format!("{n} mods in your library are missing from the archive. Rescan first.")
    }
}

/// Pure: everything that blocks a move, in the order the dialog lists them: problems with the
/// folder itself, mods the library doesn't know (they'd be left behind), library mods missing
/// from the archive (each would stop the move), names already at the destination, then free
/// space. Mods renamed on one volume need no room at all. Otherwise, on one drive each mod's old
/// copy is deleted before the next is copied — and an active mod's hardlink is re-linked to the
/// new copy first, so the delete really frees it — so only the largest mod needs room at a time;
/// across drives the whole archive must fit.
///
/// Where a new link into the new archive might be a copy (see `relocate::relink_cost`): an
/// active mod's hardlink is kept as its standalone copy, so on one drive its old bytes stay used
/// (`relink_kept`); and an active mod whose entry is missing or links into the old archive gets
/// a full copy in the mods folder (`relink_copied`), counted on the new archive's drive when the
/// mods folder shares it, else as its own check on the mods folder's drive. Otherwise the mods
/// folder's drive needs nothing extra.
fn move_problems(f: MoveFacts) -> Vec<String> {
    let mut v = f.target;
    v.extend(f.unknown);
    if f.missing > 0 {
        v.push(missing_mods_problem(f.missing));
    }
    v.extend(f.collisions);
    let s = f.space;
    // On one drive a kept hardlink's bytes stay used once its mod is moved, so the peak is every
    // kept mod plus the largest other one in flight; a kept mod's own copy is already in that
    // sum.
    let mut needed = match (s.archive_same_drive, s.relink_kept) {
        _ if s.renames => 0,
        (false, _) => s.bytes,
        (true, 0) => s.largest,
        (true, kept) => kept + s.largest_unkept,
    };
    let mods_needed = if s.mods_same_drive {
        needed += s.relink_copied;
        0
    } else if s.old_on_mods_drive {
        s.relink_largest_copied
    } else {
        s.relink_copied
    };
    if needed > 0 {
        v.extend(relocate::space_problem(needed, s.free));
    }
    if mods_needed > 0 {
        v.extend(relocate::mods_space_problem(mods_needed, s.mods_free));
    }
    v
}

/// The facts a move to `new` depends on (the dialog's preflight, and the start command's
/// server-side re-check — the webview isn't trusted to have run the preflight).
struct MoveCheck {
    mod_count: usize,
    bytes: u64,
    free: Option<u64>,
    problems: Vec<String>,
}

fn check_move(
    conn: &rusqlite::Connection,
    layout: &organize::Layout,
    key: &str,
    new: &std::path::Path,
    target: Vec<String>,
) -> MoveCheck {
    let rows = db::load_organized(conn, key);
    let archived =
        |r: &db::OrganizedRow| organize::archive_path(&layout.archive, &r.category, &r.file_name);
    let sizes: Vec<u64> = rows
        .iter()
        .map(|r| relocate::size_of(&archived(r)))
        .collect();
    let bytes: u64 = sizes.iter().sum();
    let missing = rows
        .iter()
        .filter(|r| archived(r).symlink_metadata().is_err())
        .count();
    let new_mount = organize::mount_of(new);
    let free = new_mount.as_ref().map(|(_, f)| *f);
    // For space, an unknown mount is another drive: the whole archive must fit.
    let old_mount = organize::mount_of(&layout.archive);
    let archive_same_drive = organize::known_same_mount(&old_mount, &new_mount);
    // The move's own rename decision (`with_move_ctx`): a rename keeps hardlinks valid and
    // takes no room.
    let renames = organize::volume_relation(&layout.archive, new) == Some(true);
    let mods_mount = organize::mount_of(&layout.mods);
    // Re-links only cost space where a new link might be a copy. Asking runs the link probes
    // in the new folder, so only once it is cleared (`target` empty) and only when an active
    // mod would be re-linked at all.
    let cost = relocate::relink_cost(&layout.mods, &layout.archive, &rows);
    let (mut relink_kept, mut relink_copied, mut relink_largest_copied) = (0, 0, 0);
    if target.is_empty() && cost.kept + cost.copied_zips + cost.copied_dirs > 0 {
        let to = organize::Layout {
            mods: layout.mods.clone(),
            archive: new.to_path_buf(),
        };
        if cost.kept + cost.copied_zips > 0 && organize::LinkPolicy::new(&to).can_only_copy() {
            // A renamed mod's hardlink stays its link (the same file): nothing is kept apart.
            relink_kept = if renames { 0 } else { cost.kept };
            relink_copied = cost.copied_zips;
            relink_largest_copied = cost.largest_copied_zip;
        }
        if cost.copied_dirs > 0 && organize::dir_link_may_copy(&to) {
            relink_copied += cost.copied_dirs;
            relink_largest_copied = relink_largest_copied.max(cost.largest_copied_dir);
        }
    }
    // An archive folder Silo can't list may hold unrecorded mods the move would leave behind.
    let unknown = match relocate::unknown_mods(&layout.archive, &rows) {
        Ok(unknown) => relocate::unknown_messages(
            &unknown,
            &rows,
            "then try again.",
            "rescan, then try again.",
        ),
        Err(e) => vec![relocate::unreadable_archive_message(&e, "then try again.")],
    };
    let problems = move_problems(MoveFacts {
        target,
        unknown,
        missing,
        collisions: relocate::collisions(new, &rows),
        space: Space {
            archive_same_drive,
            renames,
            bytes,
            largest: sizes.iter().copied().max().unwrap_or(0),
            free,
            largest_unkept: if relink_kept > 0 {
                cost.largest_unkept
            } else {
                0
            },
            relink_kept,
            relink_copied,
            relink_largest_copied,
            mods_same_drive: organize::known_same_mount(&mods_mount, &new_mount),
            old_on_mods_drive: organize::known_same_mount(&old_mount, &mods_mount),
            mods_free: mods_mount.as_ref().map(|(_, f)| *f),
        },
    });
    MoveCheck {
        mod_count: rows.len(),
        bytes,
        free,
        problems,
    }
}

/// Pure: the log line for a start / resume / cancel outcome.
fn move_outcome(what: &str, out: &Result<Option<relocate::MoveMarker>, String>) -> String {
    match out {
        Ok(None) => format!("archive move {what}: finished"),
        Ok(Some(m)) => {
            let reason = match (&m.pause_reason, m.errors.is_empty()) {
                (Some(r), _) => r.as_str(),
                (None, false) => "stopped on errors",
                (None, true) => "on request",
            };
            format!(
                "archive move {what}: paused ({reason}; {} error(s))",
                m.errors.len()
            )
        }
        Err(e) => format!("archive move {what}: refused: {e}"),
    }
}

/// Pure: how loud the outcome's log line is. Finished, or paused because the user asked, is
/// info; a pause for a reason (the game started) or on errors, or a refusal, is a warning.
fn move_outcome_level(out: &Result<Option<relocate::MoveMarker>, String>) -> log::Level {
    match out {
        Ok(None) => log::Level::Info,
        Ok(Some(m)) if m.pause_reason.is_none() && m.errors.is_empty() => log::Level::Info,
        _ => log::Level::Warn,
    }
}

/// Run `f` with the production move context (the command layer's request flag, the real
/// FS25 process check, progress emitted as `archive-move-progress`) under the library write
/// lock — refused, never waited for, when another write holds it — then re-resolve the
/// published layout (the archive setting may have switched, the marker paused or cleared)
/// before releasing the lock. Logs the outcome.
fn with_move_ctx(
    app: &tauri::AppHandle,
    db: &std::path::Path,
    what: &str,
    f: impl FnOnce(
        &mut relocate::RunCtx<'_>,
        &organize::Layout,
    ) -> Result<Option<relocate::MoveMarker>, String>,
) -> Result<Option<relocate::MoveMarker>, String> {
    let run = || {
        let _write = try_move_lock()?;
        // A resume or cancel also reaches a blocked folder's own unfinished move.
        let (layout, key) = match what {
            "resume" | "cancel" => {
                let (active, blocked) = modsroot::published();
                let l = resumable_layout(active, blocked, modsroot::resolved())?;
                let key = modsroot::canonical_key(&l.mods);
                (l, key)
            }
            _ => active_layout()?,
        };
        let conn = db::open(db)?;
        let emitter = app.clone();
        let mut progress = move |p: &relocate::Progress| {
            let _ = emitter.emit("archive-move-progress", p.clone());
        };
        let mut ctx = relocate::RunCtx {
            conn: &conn,
            mods: &layout.mods,
            mods_key: &key,
            // A launch Silo just asked for counts too (#415).
            game_running: &game_running_for_move,
            game_check_interval: std::time::Duration::from_secs(2),
            request: relocate::requests(),
            progress: &mut progress,
            free_space: &|p| organize::mount_of(p).map(|(_, free)| free),
            same_volume: &organize::volume_relation,
        };
        let out = f(&mut ctx, &layout);
        let _ = compute_mods_root_status(&conn);
        out
    };
    let out = run();
    // A cancel that finds a move holding the lock becomes a request (logged there), not a
    // refusal.
    let handed_off = what == "cancel" && matches!(&out, Err(e) if cancel_falls_back(e));
    if !handed_off {
        log::log!(move_outcome_level(&out), "{}", move_outcome(what, &out));
    }
    out
}

/// Start moving the archive to `new` (already `modsroot::real_folder`-normalized, or for
/// `create`, a normalized mods folder joined with `archive`) and run until it finishes
/// (`None`) or pauses (the marker). Refuses — writing nothing, not even a probe — while a
/// move is unfinished or the folder is ruled out; `create` makes the folder only after that.
fn start_move(
    app: &tauri::AppHandle,
    db: &std::path::Path,
    new: PathBuf,
    create: bool,
) -> Result<Option<relocate::MoveMarker>, String> {
    ensure_no_move()?;
    let app_data = profile::app_data_dir(app).ok();
    with_move_ctx(app, db, "start", |ctx, layout| {
        // Nothing to move out of a missing archive — and every mod would look "missing".
        if layout.archive_missing() {
            return Err(archive_unavailable(&layout.archive));
        }
        let target = target_problems(ctx.conn, layout, &new, app_data.as_deref());
        if !target.is_empty() {
            return Err(target.join("\n"));
        }
        if create {
            std::fs::create_dir_all(&new)
                .map_err(|e| format!("Couldn't create {}: {e}", new.display()))?;
        }
        modsroot::validate_folder(&new)?;
        let check = check_move(ctx.conn, layout, ctx.mods_key, &new, Vec::new());
        if !check.problems.is_empty() {
            return Err(check.problems.join("\n"));
        }
        log::info!(
            "moving the library archive {} -> {}",
            layout.archive.display(),
            new.display()
        );
        relocate::start(ctx, layout.archive.clone(), new)
    })
}

#[tauri::command]
async fn archive_status(app: tauri::AppHandle) -> Result<ArchiveStatus, error::AppError> {
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        let running = relocate::is_running();
        if !modsroot::resolved() {
            return Err(error::AppError::new(
                error::code::STILL_LOADING,
                STILL_LOADING,
            ));
        }
        // A blocked folder's own unfinished move is still shown (it can be resumed or
        // cancelled).
        let (active, blocked) = modsroot::published();
        let Some(a) = active.or_else(|| blocked.and_then(|b| b.moving).map(|m| *m)) else {
            return Ok(ArchiveStatus {
                path: None,
                is_default: true,
                available: false,
                moving: None,
                running,
            });
        };
        let conn = db::open(&db)?;
        let key = modsroot::canonical_key(&a.layout.mods);
        let is_default = a.layout.archive_is_default();
        Ok(ArchiveStatus {
            path: Some(a.layout.archive.to_string_lossy().into_owned()),
            is_default,
            available: is_default || a.layout.archive.is_dir(),
            moving: relocate::load_marker(&conn, &key),
            running,
        })
    })
    .await
    .map_err(|e| error::AppError::from(e.to_string()))?
}

/// What moving the archive to `path` involves, and anything that blocks it. Read-only apart
/// from the write probe, the symlink probe and the hardlink probe — on a drive the disk list
/// doesn't know, or when an active mod would be re-linked (`check_move`) — each removed again,
/// which only run when the folder itself isn't ruled out.
#[tauri::command]
async fn archive_move_preflight(
    app: tauri::AppHandle,
    path: String,
) -> Result<relocate::Preflight, error::AppError> {
    let db = db_path(&app)?;
    let app_data = profile::app_data_dir(&app).ok();
    tauri::async_runtime::spawn_blocking(move || {
        ensure_no_move()?;
        ensure_archive_available()?;
        let (layout, key) = active_layout()?;
        let new = modsroot::real_folder(std::path::Path::new(&path))?;
        let conn = db::open(&db)?;
        let mut target = target_problems(&conn, &layout, &new, app_data.as_deref());
        let probed = target.is_empty()
            && match modsroot::validate_folder(&new) {
                Ok(()) => true,
                Err(e) => {
                    target.push(e);
                    false
                }
            };
        // Refused as a move but fine as a re-point: an archive holding no mods (nothing to
        // move or orphan) and a folder clear of every fence — its own filed mods are welcome.
        let can_repoint = !archive_holds_mods(&layout)
            && fences(&conn, &layout, app_data.as_deref())
                .is_ok_and(|f| fence_problems(&layout, &new, &f).is_empty());
        let check = check_move(&conn, &layout, &key, &new, target);
        // Links as `LinkPolicy` will decide them there: a drive the disk list doesn't know is
        // probed (only once the folder is cleared), and symlinks never leave a Mac App Store
        // sandbox. The space check above keeps "unknown = another drive".
        let same_drive =
            organize::same_volume_or_probe(organize::volume_relation(&layout.mods, &new), || {
                probed
                    .then(|| organize::hardlinks_between(&layout.mods, &new))
                    .flatten()
            });
        let symlinks_available = probed
            && organize::symlinks_usable(&organize::Layout {
                mods: layout.mods.clone(),
                archive: new.clone(),
            });
        Ok(relocate::Preflight {
            mod_count: check.mod_count,
            bytes: check.bytes,
            free_bytes: check.free,
            same_drive,
            symlinks_available,
            loose_mods: relocate::loose_mods(&new).len(),
            problems: check.problems,
            can_repoint,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Move the archive to `path`; runs until the move finishes (`null`) or pauses (the marker).
#[tauri::command]
async fn archive_move_start(
    app: tauri::AppHandle,
    path: String,
) -> Result<Option<relocate::MoveMarker>, error::AppError> {
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || -> Result<Option<relocate::MoveMarker>, String> {
        let new = modsroot::real_folder(std::path::Path::new(&path))?;
        if !picked::allows(picked::Purpose::Archive, &new) {
            return Err(picked::NOT_PICKED.into());
        }
        let moved = start_move(&app, &db, new, false)?;
        // Used: a later call can't replay it. (A refused start keeps it for "use as it is".)
        picked::forget(picked::Purpose::Archive);
        Ok(moved)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(error::AppError::from)
}

/// Resume the paused move (in whichever direction it was going).
#[tauri::command]
async fn archive_move_resume(
    app: tauri::AppHandle,
) -> Result<Option<relocate::MoveMarker>, error::AppError> {
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        with_move_ctx(&app, &db, "resume", |ctx, _| relocate::resume(ctx))
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(error::AppError::from)
}

/// Ask the running move to pause at the next mod boundary.
#[tauri::command]
fn archive_move_pause() {
    relocate::request_pause();
}

/// Cancel: a running move is asked to turn around (returns `null` at once — `archive_status`
/// shows when it lands); a paused one is run back home here.
#[tauri::command]
async fn archive_move_cancel(
    app: tauri::AppHandle,
) -> Result<Option<relocate::MoveMarker>, error::AppError> {
    // A move holds the library lock; reach it through the request flag instead.
    if move_in_progress() {
        return Ok(request_turn_around());
    }
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        match with_move_ctx(&app, &db, "cancel", |ctx, _| relocate::cancel(ctx)) {
            // A start/resume took the lock in between (its checks may not have flipped
            // `is_running()` yet): ask it to turn around rather than report "busy".
            Err(e) if cancel_falls_back(&e) => Ok(request_turn_around()),
            other => other,
        }
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(error::AppError::from)
}

/// Pure: a cancel refused because a move holds the library lock becomes a request to that move.
/// (An ordinary write holding it stays an error, so the cancel isn't silently dropped.)
fn cancel_falls_back(err: &str) -> bool {
    err == MOVE_LOCK_HELD
}

fn request_turn_around() -> Option<relocate::MoveMarker> {
    relocate::request_cancel();
    log::info!("archive move cancel: asked the running move to turn around");
    None
}

/// Move the archive back to the default `<mods>/archive` (created once the checks pass).
#[tauri::command]
async fn archive_use_default(
    app: tauri::AppHandle,
) -> Result<Option<relocate::MoveMarker>, error::AppError> {
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        ensure_no_move()?;
        let (layout, _) = active_layout()?;
        if layout.archive_is_default() {
            return Err("Your library archive is already in its default place.".into());
        }
        ensure_archive_available()?;
        let mods = modsroot::real_folder(&layout.mods)?;
        let default = organize::Layout::default_for(&mods).archive;
        start_move(&app, &db, default, true)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(error::AppError::from)
}

/// Any mod (zip, or folder with a `modDesc.xml`) filed in one of the archive's category
/// folders — the same candidates a move would carry (Silo's own temps don't count). A missing
/// folder holds none; one that exists but can't be read is assumed to hold mods, so nothing
/// is ever re-pointed away from mods Silo couldn't see.
fn archive_holds_mods(layout: &organize::Layout) -> bool {
    relocate::filed_mods(&layout.archive)
        .map(|v| !v.is_empty())
        .unwrap_or(true)
}

/// Pure: why the archive can't be re-pointed right now, if it can't. Re-pointing (changing
/// where Silo looks, moving nothing) is for a custom archive that isn't there, or an archive
/// that holds no mods (nothing can be orphaned — e.g. the way back after starting fresh with
/// the default). An archive holding mods is moved instead, and never while a move is unfinished.
fn repoint_refusal(
    layout: &organize::Layout,
    unfinished_move: bool,
    holds_mods: bool,
) -> Option<String> {
    if unfinished_move {
        return Some("Finish or cancel the archive move first.".into());
    }
    if !layout.archive_missing() && holds_mods {
        return Some("Your library archive holds mods. Use Change… to move them instead.".into());
    }
    None
}

/// Point Silo at another archive folder (`None` = the default `<mods>/archive`) WITHOUT moving
/// anything — for a custom archive that has gone missing (the user moved or restored it
/// elsewhere, or starts fresh with the default; whatever is on the old drive stays there,
/// untouched), or an archive that holds no mods (e.g. back to a restored archive after
/// starting fresh). The same path/overlap fences as a move apply, but mods already filed in the
/// folder are welcome — adopting them is the point. Republishes the layout; the frontend then
/// rescans so the library reconciles against the new archive.
#[tauri::command]
async fn archive_repoint(
    app: tauri::AppHandle,
    path: Option<String>,
) -> Result<(), error::AppError> {
    ensure_no_move()?;
    let db = db_path(&app)?;
    let app_data = profile::app_data_dir(&app).ok();
    tauri::async_runtime::spawn_blocking(move || {
        let _write = library_write()?;
        let (layout, key) = active_layout()?;
        let conn = db::open(&db)?;
        let unfinished = relocate::load_marker(&conn, &key).is_some();
        if let Some(why) = repoint_refusal(&layout, unfinished, archive_holds_mods(&layout)) {
            return Err(why);
        }
        let new = match &path {
            Some(p) => {
                let new = modsroot::real_folder(std::path::Path::new(p))?;
                if !picked::allows(picked::Purpose::Archive, &new) {
                    return Err(picked::NOT_PICKED.into());
                }
                new
            }
            None => organize::Layout::default_for(&modsroot::real_folder(&layout.mods)?).archive,
        };
        let problems = fence_problems(&layout, &new, &fences(&conn, &layout, app_data.as_deref())?);
        if !problems.is_empty() {
            return Err(problems.join("\n"));
        }
        // A chosen folder must be usable now; the default is created when first filed into.
        if path.is_some() {
            modsroot::validate_folder(&new)?;
        }
        relocate::store_archive_setting(&conn, &key, &layout.mods, &new)?;
        // The folder left behind holds no mods (checked above), so its note is stale (#394).
        if !layout.archive_is_default() && !modsroot::same_folder(&layout.archive, &new) {
            relocate::retire_note(&layout.archive, &layout.mods);
        }
        if path.is_some() {
            picked::forget(picked::Purpose::Archive);
        }
        log::info!(
            "re-pointed the library archive {} -> {} (nothing moved)",
            layout.archive.display(),
            new.display()
        );
        let _ = compute_mods_root_status(&conn);
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(error::AppError::from)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct AdoptPreview {
    plan: Vec<organize::PlannedMove>,
    inputs: Vec<organize::ModInput>,
    /// File names left where they are: their techName is already in the library, or repeats
    /// one earlier in this batch.
    duplicates: Vec<String>,
}

/// Pure: split scanned loose mods into adoptable ones and duplicates (file names) — a techName
/// already in the library, or repeated within the batch (the first by file name wins).
fn split_adopt(
    mut found: Vec<organize::ModInput>,
    known: &HashSet<String>,
) -> (Vec<organize::ModInput>, Vec<String>) {
    found.sort_by(|a, b| a.file_name.cmp(&b.file_name));
    let mut seen: HashSet<String> = HashSet::new();
    let mut inputs = Vec::new();
    let mut duplicates = Vec::new();
    for m in found {
        if known.contains(&m.tech_name) || !seen.insert(m.tech_name.clone()) {
            duplicates.push(m.file_name);
        } else {
            inputs.push(m);
        }
    }
    (inputs, duplicates)
}

/// Pure: the candidates the user picked (by file name), and the picked names that aren't
/// candidates any more.
fn select_adopt(
    candidates: Vec<organize::ModInput>,
    file_names: &[String],
) -> (Vec<organize::ModInput>, Vec<String>) {
    let want: HashSet<&str> = file_names.iter().map(String::as_str).collect();
    let have: HashSet<String> = candidates.iter().map(|c| c.file_name.clone()).collect();
    let missing = file_names
        .iter()
        .filter(|n| !have.contains(n.as_str()))
        .cloned()
        .collect();
    let picked = candidates
        .into_iter()
        .filter(|c| want.contains(c.file_name.as_str()))
        .collect();
    (picked, missing)
}

/// The loose mods at the archive's top level that could be filed into it, read from disk by
/// the server (techName, kind and category never come from the webview), plus duplicates.
fn adopt_candidates(
    conn: &rusqlite::Connection,
    layout: &organize::Layout,
    key: &str,
) -> (Vec<organize::ModInput>, Vec<String>) {
    let known: HashSet<String> = db::load_organized(conn, key)
        .into_iter()
        .map(|r| r.tech_name)
        .collect();
    // Scan the archive's loose top level as if it were a mods folder (no archives).
    let found = scan::scan_layouts(
        vec![(layout.archive.clone(), vec![])],
        &Default::default(),
        |_, _| {},
    );
    let mods = found
        .result
        .mods
        .into_iter()
        .filter_map(|m| {
            let file_name = std::path::Path::new(&m.path)
                .file_name()?
                .to_string_lossy()
                .into_owned();
            Some(organize::ModInput {
                tech_name: m.tech_name,
                file_name,
                kind: m.kind,
                category: m.category,
                subcategory: m.subcategory,
            })
        })
        .collect();
    split_adopt(mods, &known)
}

/// Loose mods at the archive's top level (e.g. a folder that already held mods) that could be
/// filed into it. Read-only.
#[tauri::command]
async fn archive_adopt_preview(app: tauri::AppHandle) -> Result<AdoptPreview, error::AppError> {
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || -> Result<AdoptPreview, String> {
        let (layout, key) = active_layout()?;
        let conn = db::open(&db)?;
        let (inputs, duplicates) = adopt_candidates(&conn, &layout, &key);
        Ok(AdoptPreview {
            plan: organize::plan_organize(&layout.archive, &inputs),
            inputs,
            duplicates,
        })
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(error::AppError::from)
}

/// File the loose mods the user picked (by file name, from the preview) into the archive's
/// category folders, never overwriting. The candidates are re-read from disk here.
#[tauri::command]
async fn archive_adopt_apply(
    app: tauri::AppHandle,
    file_names: Vec<String>,
) -> Result<organize::Report, error::AppError> {
    ensure_no_move()?;
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || -> Result<organize::Report, String> {
        let _write = library_write()?;
        ensure_archive_available()?;
        let (layout, key) = active_layout()?;
        let conn = db::open(&db)?;
        let (candidates, _) = adopt_candidates(&conn, &layout, &key);
        let (picked, missing) = select_adopt(candidates, &file_names);
        let mut report = organize::apply_organize(&conn, &layout, &layout.archive, &picked);
        for n in missing {
            report.skipped += 1;
            report.errors.push(format!(
                "{n}: no longer a loose mod Silo can file in, so it was left untouched."
            ));
        }
        log::info!(
            "adopted {} loose mod(s) into the library archive ({} skipped, {} errors)",
            report.changed,
            report.skipped,
            report.errors.len()
        );
        Ok(report)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(error::AppError::from)
}

// ── Loadouts (named active-mod sets) ──
#[tauri::command]
async fn get_loadouts(app: tauri::AppHandle) -> Result<Vec<db::Loadout>, String> {
    let db = db_path(&app)?;
    off_main(move || Ok(db::load_loadouts(&db::open(&db)?))).await
}

#[tauri::command]
async fn save_loadout(
    app: tauri::AppHandle,
    id: Option<i64>,
    name: String,
    mods: Vec<String>,
) -> Result<i64, error::AppError> {
    let db = db_path(&app)?;
    off_main(move || {
        let mut conn = db::open(&db)?;
        db::save_loadout(&mut conn, id, &name, &mods)
    })
    .await
    .map_err(error::AppError::from)
}

/// One set's new mod list in a `replace_loadout_mods` batch.
#[derive(Debug, serde::Deserialize)]
struct LoadoutModsEdit {
    id: i64,
    mods: Vec<String>,
}

/// Replace several saved sets' mods in one transaction (the Library's "Mod sets…" dialog).
#[tauri::command]
async fn replace_loadout_mods(
    app: tauri::AppHandle,
    edits: Vec<LoadoutModsEdit>,
) -> Result<(), error::AppError> {
    let db = db_path(&app)?;
    off_main(move || {
        let mut conn = db::open(&db)?;
        let edits: Vec<(i64, Vec<String>)> = edits.into_iter().map(|e| (e.id, e.mods)).collect();
        db::replace_loadout_mods(&mut conn, &edits)
    })
    .await
    .map_err(error::AppError::from)
}

#[tauri::command]
async fn delete_loadout(app: tauri::AppHandle, id: i64) -> Result<(), error::AppError> {
    let db = db_path(&app)?;
    off_main(move || {
        let mut conn = db::open(&db)?;
        db::delete_loadout(&mut conn, id)
    })
    .await
    .map_err(error::AppError::from)
}

/// Whether a shared file the user picked is a Multiplayer check file (`"multiplayer"`: checked
/// against the active mods) or a mod set (`"set"`: previewed). Decided by its content, so a check
/// file saved as `.json` still opens as one.
#[tauri::command]
async fn shared_file_kind(path: String) -> Result<String, String> {
    off_main(move || {
        let text = paths::read_inbound_text(
            std::path::Path::new(&path),
            &["silo", "silomp", "json"],
            paths::SHARED_FILE_MAX_BYTES,
        )?;
        let kind = if mpsync::is_manifest(&text) {
            "multiplayer"
        } else {
            "set"
        };
        Ok(kind.to_string())
    })
    .await
}

/// Save an automatic "Before …" snapshot of the active mods before a switch; only the newest
/// few are kept (`db::AUTO_SNAPSHOTS_KEPT`).
#[tauri::command]
async fn save_auto_snapshot(
    app: tauri::AppHandle,
    name: String,
    mods: Vec<String>,
) -> Result<i64, error::AppError> {
    let db = db_path(&app)?;
    off_main(move || {
        let mut conn = db::open(&db)?;
        db::save_auto_snapshot(&mut conn, &name, &mods)
    })
    .await
    .map_err(error::AppError::from)
}

/// Write a mod set to a `.silo` file the user can send any way they like.
#[tauri::command]
async fn export_set_file(name: String, mods: Vec<String>, path: String) -> Result<(), String> {
    off_main(move || {
        let file = collection::SetFile {
            silo: 1,
            name,
            mods,
        };
        let json = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
        paths::safe_outbound(std::path::Path::new(&path), &["silo"])?;
        std::fs::write(&path, json).map_err(|e| e.to_string())
    })
    .await
}

// ── Conflict detection (over the active set) ──
#[tauri::command]
async fn detect_conflicts(
    mods: Vec<conflicts::ConflictInput>,
) -> Result<Vec<conflicts::Conflict>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        conflicts::detect_with(&mods, Some(conflicts::GameLists::cached()))
    })
    .await
    .map_err(|e| e.to_string())
}

// ── Game launch ──
// Off the main thread: detection walks Steam libraries and, on macOS, may shell out to
// Spotlight (`mdfind`); launch re-detects to pick Steam vs. the `.app` bundle.
#[tauri::command]
async fn detect_game() -> Result<Option<gamelaunch::GameInfo>, String> {
    tauri::async_runtime::spawn_blocking(gamelaunch::detect)
        .await
        .map_err(|e| e.to_string())
}

const PROFILE_CHANGED: &str =
    "The mods folder's FS25 profile changed. Check Settings → Mods folder, then press Play again.";

/// Pure: whether the profile Silo would start (`active`) differs from the one the app showed
/// when Play was pressed (`expected`; `None` = the main profile).
fn launch_profile_mismatch(active: Option<&std::path::Path>, expected: Option<&str>) -> bool {
    match (active, expected) {
        (None, None) => false,
        (Some(a), Some(e)) => !modsroot::same_folder(a, std::path::Path::new(e)),
        _ => true,
    }
}

/// How many of a paused move's unreachable mods Play's question names before "and N more".
const UNREACHABLE_NAMED: usize = 8;

/// Pure: Play's question when, with an archive move paused, FS25 can't open the active `mods`
/// (sorted techNames, at least one) from the mods folder (#415).
fn unreachable_message(mods: &[String]) -> String {
    let (who, them) = match mods {
        [one] => (one.clone(), "it"),
        [a, b] => (format!("{a} and {b}"), "them"),
        _ => {
            let shown = mods[..mods.len().min(UNREACHABLE_NAMED)].join(", ");
            let more = mods.len().saturating_sub(UNREACHABLE_NAMED);
            let list = if more > 0 {
                format!("{shown} and {more} more")
            } else {
                shown
            };
            (format!("{} mods ({list})", mods.len()), "them")
        }
    };
    format!(
        "While the library move is paused, Farming Simulator 25 can't open {who} from your mods \
         folder, so it would start without {them}. Resume the move from the notice at the top \
         to mend the links it made, or fix {them} yourself, then press Play again."
    )
}

/// Pure: Play's archive-move gate, for every launch (Play, guided bisection, a `-profile`
/// start). `moving`: a move holds the library lock or is copying (`move_in_progress`): refused,
/// pause it first, even with `play_anyway`. `unreachable`: the active mods FS25 can't open
/// while a move is paused (`relocate::unreachable_mods`; empty without one), or why Silo
/// couldn't look.
/// Either of those asks first (`MOVE_LINKS_BROKEN`) unless `play_anyway`. A paused move with
/// nothing to name plays as before (#415).
fn move_play_gate(
    moving: bool,
    unreachable: Result<Vec<String>, String>,
    play_anyway: bool,
) -> Result<(), error::AppError> {
    if moving {
        return Err(error::AppError::new(
            error::code::MOVE_RUNNING,
            "Silo is moving your library archive. Pause the move to play.",
        ));
    }
    let message = match unreachable {
        _ if play_anyway => return Ok(()),
        Ok(mods) if mods.is_empty() => return Ok(()),
        Ok(mods) => unreachable_message(&mods),
        Err(e) => format!(
            "Silo couldn't check whether Farming Simulator 25 can open your active mods while \
             the library move is paused ({e}). Resume the move from the notice at the top \
             first, or play anyway."
        ),
    };
    Err(error::AppError::new(
        error::code::MOVE_LINKS_BROKEN,
        message,
    ))
}

/// The active mods an unfinished (paused) archive move of the folder FS25 will read
/// (`launch_reads`) left it unable to open (`relocate::unreachable_mods`). Empty with no move.
/// Reads the marker from the DB, not the published layout, so a move that paused or crashed
/// since the last publish is seen as it is; a DB Silo can't read is an error, never "no move".
fn paused_move_unreachable(db: &std::path::Path) -> Result<Vec<String>, String> {
    let (active, blocked) = modsroot::published();
    let Some(mods) = launch_reads(
        gameprofile::published(),
        modsroot::game_reads(),
        launch_mods_folder(active, blocked),
    ) else {
        return Ok(Vec::new());
    };
    unreachable_in(&db::open(db)?, &mods)
}

/// Pure: the mods folder Silo manages, as published: the active one, or a blocked folder's
/// (whose own unfinished move is still shown). `None` before there is one.
fn launch_mods_folder(
    active: Option<modsroot::Active>,
    blocked: Option<modsroot::Blocked>,
) -> Option<PathBuf> {
    match (active, blocked) {
        (Some(a), _) => Some(a.layout.mods),
        (None, Some(b)) => Some(b.mods),
        (None, None) => None,
    }
}

/// Pure: the mods folder the game Play starts will read (#415): on a profile it launches
/// (#387), that profile's (`Published::reads`); otherwise the main profile's
/// (`modsroot::game_reads`). Where Silo couldn't read that, the folder it manages (`managed`),
/// which is what Play assumes everywhere else.
fn launch_reads(
    profile: Option<gameprofile::Published>,
    game_reads: Option<PathBuf>,
    managed: Option<PathBuf>,
) -> Option<PathBuf> {
    match profile {
        Some(p) if p.launch => p.reads,
        _ => game_reads,
    }
    .or(managed)
}

/// `relocate::unreachable_mods` for the unfinished move of the mods folder `mods`, as the DB
/// records it; empty when it has none. A marker or library record Silo can't read is an error.
fn unreachable_in(
    conn: &rusqlite::Connection,
    mods: &std::path::Path,
) -> Result<Vec<String>, String> {
    let key = modsroot::canonical_key(mods);
    if relocate::try_load_marker(conn, &key)?.is_none() {
        return Ok(Vec::new());
    }
    let rows = db::try_load_organized(conn, &key)?;
    Ok(relocate::unreachable_mods(mods, &rows))
}

/// How long after Silo started FS25 an archive move still won't start or resume, unless the
/// game has shown up (and so is checked directly) by then (#415). The game takes a few seconds
/// to appear, longer through Steam.
const LAUNCH_GRACE_MS: u64 = 30_000;

/// Who may go between a launch and an archive move (#415): one short-lived lock arbitrates a
/// Play starting and a move being admitted, so they never overlap.
/// - A Play is admitted (`begin_launch`) only while no move is (`move_in_progress`, read under
///   this lock); it stays in flight, with no deadline, through its checks and the start itself.
/// - A move is admitted (`admit_move`, which raises `MOVE_HOLDS_LOCK` under this lock) only
///   with no Play in flight and none started in the last `LAUNCH_GRACE_MS`.
/// - Only a start that went out (`end_launch(true)`) sets that deadline; a refused Play leaves
///   an earlier start's deadline as it was.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct LaunchGate {
    /// Plays admitted and not yet finished.
    in_flight: u32,
    /// When the last start went out (`launch_clock` ms), until the game shows up or the grace
    /// passes.
    started: Option<u64>,
}

impl LaunchGate {
    /// Admit a Play unless a move is admitted (`moving`). Pure.
    fn begin_launch(&mut self, moving: bool) -> bool {
        if moving {
            return false;
        }
        self.in_flight += 1;
        true
    }

    /// A Play admitted by `begin_launch` is done; `started`: the game start went out at `now`.
    fn end_launch(&mut self, started: bool, now: u64) {
        self.in_flight = self.in_flight.saturating_sub(1);
        if started {
            self.started = Some(now);
        }
    }

    /// A launch is starting or was started moments ago (`LAUNCH_GRACE_MS`).
    fn launching(&self, now: u64) -> bool {
        self.in_flight > 0
            || self
                .started
                .is_some_and(|t| now.saturating_sub(t) < LAUNCH_GRACE_MS)
    }
}

static LAUNCH_GATE: std::sync::Mutex<LaunchGate> = std::sync::Mutex::new(LaunchGate {
    in_flight: 0,
    started: None,
});

fn launch_gate() -> std::sync::MutexGuard<'static, LaunchGate> {
    LAUNCH_GATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn launch_clock() -> u64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let start = *START.get_or_init(std::time::Instant::now);
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Refused move admission while a launch is starting (`LaunchGate::launching`).
const LAUNCH_STARTING: &str = "Close Farming Simulator 25 first. Silo won't move mods while \
     the game is starting or using them. If it didn't start, try again in half a minute.";

/// Admit an archive move (#415): raise `MOVE_HOLDS_LOCK` under the launch gate, unless a Play
/// is in flight or just started the game. The caller holds the library write lock.
fn admit_move() -> Result<(), String> {
    let gate = launch_gate();
    if gate.launching(launch_clock()) {
        return Err(LAUNCH_STARTING.into());
    }
    MOVE_HOLDS_LOCK.store(true, std::sync::atomic::Ordering::SeqCst);
    Ok(())
}

/// A Play admitted through the launch gate; `started` records that the start went out. Dropped
/// without it (a refusal, an error, a panic), the Play is simply done.
struct LaunchTicket {
    started: bool,
}

impl LaunchTicket {
    /// Admit a Play, or refuse it (`MOVE_RUNNING`) while a move is admitted.
    fn take() -> Result<LaunchTicket, error::AppError> {
        if launch_gate().begin_launch(move_in_progress()) {
            return Ok(LaunchTicket { started: false });
        }
        Err(move_play_gate(true, Ok(Vec::new()), true).expect_err("refused while a move is"))
    }
}

impl Drop for LaunchTicket {
    fn drop(&mut self) {
        launch_gate().end_launch(self.started, launch_clock());
    }
}

/// What an archive move takes for "the game is running" (`RunCtx::game_running`): FS25 is, or
/// Silo started it moments ago and it hasn't shown up yet (`LaunchGate::launching`). Once seen
/// running, the start's deadline goes: from then on the game is checked directly.
fn game_running_for_move() -> bool {
    if gamelaunch::fs25_running() {
        launch_gate().started = None;
        return true;
    }
    launch_gate().launching(launch_clock())
}

/// Start FS25. `check_profile`: refuse unless the FS25 profile that would start is
/// `expected_profile` (the one the app shows; `None` = the main profile), so Play never starts
/// a profile the app didn't show (#387). Left out (guided bisection), no such check. Refused
/// while an archive move runs; while a paused one left active mods FS25 can't open, refused
/// with `MOVE_LINKS_BROKEN` (naming them) unless `play_anyway` (#415, `move_play_gate`).
#[tauri::command]
async fn launch_game(
    app: tauri::AppHandle,
    expected_profile: Option<String>,
    check_profile: Option<bool>,
    play_anyway: Option<bool>,
) -> Result<(), error::AppError> {
    if profile::active() {
        return Err(
            "The demo profile can't start the game: FS25 would load your real mods.".into(),
        );
    }
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        // The mods folder's FS25 profile, when it's on (#387): FS25 then starts on that folder.
        // Read and launched under the resolution lock, so no folder switch publishes in between:
        // the profile started is the one published with the current folder, never a mix.
        let _resolving = MODS_ROOT_RESOLVE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Before the mods folder is first resolved it isn't known yet, and the main profile
        // would start with the wrong mods.
        if !modsroot::resolved() {
            return Err(STILL_LOADING.into());
        }
        // Admitted through the launch gate (refused while a move is admitted); from here until
        // it's done, and for a while after a start, no move is admitted (`LaunchGate`).
        let mut ticket = LaunchTicket::take()?;
        // No move can be admitted now; what a paused one left.
        let unreachable = if play_anyway.unwrap_or(false) {
            Ok(Vec::new())
        } else {
            paused_move_unreachable(&db)
        };
        if let Ok(mods) = &unreachable {
            if !mods.is_empty() {
                log::warn!(
                    "play: the paused archive move left {} mod(s) unreachable: {}",
                    mods.len(),
                    mods.join(", ")
                );
            }
        }
        move_play_gate(false, unreachable, play_anyway.unwrap_or(false))?;
        let game_profile = gameprofile::active();
        if check_profile.unwrap_or(false)
            && launch_profile_mismatch(game_profile.as_deref(), expected_profile.as_deref())
        {
            return Err(PROFILE_CHANGED.into());
        }
        if play_anyway.unwrap_or(false) {
            log::warn!("play: Play anyway, past the paused archive move's link check");
        }
        gamelaunch::launch(game_profile.as_deref()).map_err(error::AppError::from)?;
        ticket.started = true;
        Ok(())
    })
    .await
    .map_err(|e| error::AppError::from(e.to_string()))?
}

/// Write text to a user-chosen path (used by the diagnostics report export). Confined to a
/// report file type with no traversal, so it can't be turned into an arbitrary-file write.
#[tauri::command]
async fn save_text(path: String, content: String) -> Result<(), String> {
    off_main(move || {
        paths::safe_outbound(std::path::Path::new(&path), &["md", "txt"])?;
        std::fs::write(&path, content).map_err(|e| e.to_string())
    })
    .await
}

/// The diagnostics report (built by the frontend) plus, if asked, the recent log, with the
/// home folder and user name removed from both.
#[tauri::command]
async fn diagnostics_bundle(
    app: tauri::AppHandle,
    report: String,
    include_log: bool,
) -> Result<String, String> {
    let log_dir = profile::app_log_dir(&app).ok();
    let log_error = app.try_state::<LogFileError>().and_then(|s| s.0.clone());
    let version = app.package_info().version.to_string();
    off_main(move || {
        let header = format!("Silo {version} on {}", std::env::consts::OS);
        let parts = include_log.then(|| {
            let files = log_dir
                .as_deref()
                .map(diagnostics::log_files)
                .unwrap_or_default();
            diagnostics::LogParts {
                warnings: diagnostics::recent_warnings(&files, 50, 16 * 1024),
                recent: diagnostics::recent_log(&files, 300, 64 * 1024),
                unavailable: log_error,
            }
        });
        let home = dirs::home_dir().map(|h| h.to_string_lossy().into_owned());
        let user = std::env::var("USERNAME")
            .or_else(|_| std::env::var("USER"))
            .ok();
        Ok(diagnostics::bundle(
            &header,
            &report,
            parts.as_ref(),
            home.as_deref(),
            user.as_deref(),
        ))
    })
    .await
}

/// Put `text` on the system clipboard. From Rust because WebKit (macOS, Linux) refuses the
/// webview's clipboard after an awaited call; the webview itself has no clipboard permission.
#[tauri::command]
async fn copy_text(app: tauri::AppHandle, text: String) -> Result<(), String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    off_main(move || app.clipboard().write_text(text).map_err(|e| e.to_string())).await
}

/// Clear the scan cache so the next scan re-parses & re-categorizes every mod.
#[tauri::command]
async fn clear_scan_cache(app: tauri::AppHandle) -> Result<(), String> {
    let db = db_path(&app)?;
    off_main(move || {
        db::clear_cache(&db::open(&db)?);
        Ok(())
    })
    .await
}

/// The FS25 user data dir (parent of mods/, savegameN/, game.xml).
#[tauri::command]
async fn user_dir_path() -> Result<Option<String>, String> {
    off_main(|| {
        Ok(gameprofile::user_dir(gameprofile::Place::GameConfig)
            .map(|p| p.to_string_lossy().into_owned()))
    })
    .await
}

/// Generate a filltype-compatibility bridge companion mod into the library root.
/// Returns the created zip filename; the frontend rescans to pick it up.
#[tauri::command]
async fn generate_bridge(spec: bridge::BridgeSpec, root: Option<String>) -> Result<String, String> {
    ensure_no_move()?;
    let root = primary_root(root)?;
    tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
        let _write = library_write()?;
        let filename = format!("{}.zip", spec.tech_name);
        let dest = root.join(&filename);
        if dest.exists() {
            return Err(format!("{filename} already exists in your mods folder"));
        }
        bridge::generate(&spec, &dest)?;
        fslog::changed("generate", &dest);
        Ok(filename)
    })
    .await
    .map_err(|e| e.to_string())?
}

// ── Multiplayer mod-set sync ──

/// What a saved Multiplayer check file holds.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct MpExportResult {
    /// How many mods the file lists.
    count: usize,
    /// Packaged mods whose zip Silo couldn't read: listed without a fingerprint, so friends
    /// will see them as "can't check" (#355).
    unreadable: Vec<String>,
}

/// Hash the host's active set and write a shareable manifest to `path`. Returns the
/// number of mods written and the zips that couldn't be fingerprinted.
#[tauri::command]
async fn mp_export(mods: Vec<mpsync::ModRef>, path: String) -> Result<MpExportResult, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<MpExportResult, String> {
        let manifest = mpsync::build_manifest(&mods);
        let json = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
        paths::safe_outbound(std::path::Path::new(&path), &["silomp", "json"])?;
        std::fs::write(&path, json).map_err(|e| e.to_string())?;
        Ok(MpExportResult {
            count: manifest.mods.len(),
            unreadable: mpsync::unreadable(&mods, &manifest.mods),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Read a shared manifest from `path`, hash the joiner's local set, and diff.
#[tauri::command]
async fn mp_verify_file(
    path: String,
    local: Vec<mpsync::ModRef>,
) -> Result<mpsync::VerifyReport, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<mpsync::VerifyReport, String> {
        let text = paths::read_inbound_text(
            std::path::Path::new(&path),
            &["silomp", "json"],
            paths::SHARED_FILE_MAX_BYTES,
        )?;
        let manifest: mpsync::Manifest = serde_json::from_str(&text)
            .map_err(|_| "That file isn't a multiplayer check file.".to_string())?;
        let local_hashed = mpsync::hash_mods(&local);
        Ok(mpsync::diff(&manifest.mods, &local_hashed))
    })
    .await
    .map_err(|e| e.to_string())?
}

// ── Collections (share a mod set) ──

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ExportResult {
    /// The shareable gist URL the user copies.
    url: String,
    /// How many mods went into the collection.
    count: usize,
    /// Dev/unpacked (directory) mods left out — they have no stable bytes to pin.
    omitted: Vec<String>,
    /// Packaged mods that went in without the curator's fingerprint (their zip couldn't be
    /// read), so friends can't confirm they got the same build (#355).
    unpinned: Vec<String>,
    /// The link works, but something around it didn't (the README couldn't be written).
    warning: Option<String>,
}

/// An active set pinned into collection entries (see `build_collection_entries`).
struct PinnedSet {
    /// Sorted by tech-name.
    entries: Vec<collection::CollectionMod>,
    /// Unpacked (directory) mods, left out.
    omitted: Vec<String>,
    /// Packaged mods listed without a fingerprint: their zip couldn't be read.
    unpinned: Vec<String>,
}

/// Pin an active set into collection entries: drop dev/unpacked (directory) mods (no stable
/// bytes to pin), and for each packaged mod stamp its version, catalog source/id (one batch
/// lookup, best-effort), and the curator's own provenance hash. A mod whose hash failed still
/// goes in (an importer reads it as unverified) but is named in `unpinned` for the curator.
/// Shared by export and update.
fn build_collection_entries(
    base: &str,
    conn: Option<&rusqlite::Connection>,
    mods: Vec<mpsync::ModRef>,
) -> PinnedSet {
    let (zips, dirs): (Vec<_>, Vec<_>) = mods.into_iter().partition(|m| m.kind == "zip");
    let omitted: Vec<String> = dirs.into_iter().map(|m| m.tech_name).collect();

    // The curator's own build hash — the trust anchor. Best-effort per mod; a failure is
    // reported through `unpinned`, not dropped silently.
    // (archive SHA-256, manifest hash) per zip: only the two hashes are kept, not the entries.
    let manifests: Vec<Option<(String, String)>> = zips
        .iter()
        .map(
            |m| match provenance::manifest_from_zip(std::path::Path::new(&m.path)) {
                Ok(lm) => Some((lm.archive_sha256, lm.manifest_hash)),
                Err(e) => {
                    log::warn!("collection: couldn't fingerprint {}: {e}", m.tech_name);
                    None
                }
            },
        )
        .collect();
    // A zip Silo installed from a catalog entry under another name is that entry's (#386).
    // A link that can't be checked leaves that mod without catalog metadata, never another
    // entry's; so does a linked entry that can't be fetched. Other mods keep theirs.
    let links: Vec<Result<Option<catalogalias::Link>, String>> = zips
        .iter()
        .zip(&manifests)
        .map(|(m, lm)| match (conn, lm) {
            (Some(c), Some((sha, _))) => {
                let file = std::path::Path::new(&m.path);
                catalogalias::link_for_hash(c, &m.tech_name, file, sha)
            }
            // Not fingerprinted: it may be a linked file, unless no alias has its name.
            (Some(c), None) => match catalogalias::has_any(c, &m.tech_name) {
                Ok(false) => Ok(None),
                Ok(true) => Err("its file couldn't be read to check its catalog link".to_string()),
                Err(e) => Err(e),
            },
            (None, _) => Ok(None),
        })
        .collect();
    let asked: Vec<(String, Option<catalogalias::Link>)> = zips
        .iter()
        .zip(&links)
        .map(|(m, l)| (m.tech_name.clone(), l.clone().ok().flatten()))
        .collect();
    let answers = resolve_catalog(base, &asked).unwrap_or_else(|_| vec![Ok(None); asked.len()]);
    let hits: Vec<Option<siloapi::LookupResult>> = answers
        .into_iter()
        .zip(zips.iter().zip(&links))
        .map(|(answer, (m, link))| {
            let why = match (answer, link) {
                (Ok(hit), Ok(_)) => return hit,
                (Err(e), _) => e,
                (_, Err(e)) => e.clone(),
            };
            log::warn!(
                "collection: couldn't check {}'s catalog entry: {why}",
                m.tech_name
            );
            None
        })
        .collect();

    let mut entries: Vec<collection::CollectionMod> = zips
        .iter()
        .zip(manifests)
        .zip(hits.iter().zip(&asked))
        .map(|((m, lm), (hit, (_, link)))| {
            let manifest_hash = lm.map(|(_, h)| h);
            let hit = hit.as_ref();
            let dl = hit.and_then(|r| r.download.as_ref());
            // An importer finds mods by techName and installs only a file named after the mod,
            // so the set doesn't claim a direct install for a file linked by an alias.
            let direct = dl.is_some() && link.is_none();
            collection::CollectionMod {
                tech_name: m.tech_name.clone(),
                version: m.version.clone(),
                source: dl.map(|d| d.source.clone()),
                source_url: None,
                manifest_hash,
                installable: hit.map(|_| direct),
                catalog_id: hit.map(|r| r.id.clone()),
            }
        })
        .collect();
    entries.sort_by(|a, b| a.tech_name.cmp(&b.tech_name));
    let unpinned = unpinned_names(&entries);
    PinnedSet {
        entries,
        omitted,
        unpinned,
    }
}

/// The entries that carry no curator fingerprint, by tech-name (already sorted).
fn unpinned_names(entries: &[collection::CollectionMod]) -> Vec<String> {
    entries
        .iter()
        .filter(|e| e.manifest_hash.is_none())
        .map(|e| e.tech_name.clone())
        .collect()
}

/// Export a mod set as a shareable Collection: pin each mod's identity (version +
/// canonical provenance hash of the curator's own build), enrich with a catalog source so
/// an importer can fetch it, and publish it to the user's GitHub — a secret gist (private,
/// `gist` scope) or, when `public`, a public repo with a generated README (`public_repo`
/// scope, forkable/discoverable — the P2 transport).
///
/// The trust field is the LOCAL provenance `manifestHash` — content-addressed, so an
/// importer can verify they got the exact build the curator shared even from a re-zipped
/// source.
#[tauri::command]
async fn collection_export(
    app: tauri::AppHandle,
    name: String,
    description: Option<String>,
    created_at: Option<String>,
    public: Option<bool>,
    mods: Vec<mpsync::ModRef>,
) -> Result<ExportResult, error::AppError> {
    let base = siloapi_base(&app)?;
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || -> Result<ExportResult, String> {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err("Give the set a name.".into());
        }
        let conn = db::open(&db)?;
        let token = gh_bearer_required(&conn)?;
        let author = db::get_app_setting(&conn, "gh_user");

        // Pin the packaged mods (dev/unpacked ones can't be byte-pinned — reported as omitted).
        let PinnedSet {
            entries,
            omitted,
            unpinned,
        } = build_collection_entries(&base, Some(&conn), mods);
        if entries.is_empty() {
            return Err(
                "Nothing to share. A shared set needs at least one packaged (.zip) mod.".into(),
            );
        }

        let coll = collection::Collection {
            schema: collection::SCHEMA.to_string(),
            name: name.clone(),
            description: description.filter(|s| !s.trim().is_empty()),
            author,
            created_at,
            savegame: None,
            mods: entries,
        };
        let json = collection::to_json(&coll)?;
        // The shareable link is always the silo.hllmr.com/c/ handoff page — never the raw
        // gist/repo URL (which dead-ends recipients on JSON and can't carry a working
        // "Open in Silo" link, since GitHub strips silo:// schemes).
        let mut warning = None;
        let url = if public.unwrap_or(false) {
            // P2: a public, forkable repo holding the collection + a human-readable README.
            // A retry after a failed first upload carries on in the user's own still-empty
            // repo of that name instead of failing with "name taken" (#355).
            let repo = github::create_public_repo(
                &token,
                &collection::repo_slug(&name),
                &format!(
                    "A Silo mod set for Farming Simulator 25 ({} {})",
                    coll.mods.len(),
                    if coll.mods.len() == 1 { "mod" } else { "mods" }
                ),
            )?;
            let (owner, rname) = repo
                .full_name
                .split_once('/')
                .map(|(o, r)| (o.to_string(), r.to_string()))
                .unwrap_or_default();
            let page_url = collection::repo_page_url(&owner, &rname);
            let readme = collection::readme(&coll, &page_url, &repo.html_url);
            github::create_repo_file(
                &token,
                &owner,
                &rname,
                collection::FILE_NAME,
                &json,
                "Add mod set (via Silo)",
            )
            .map_err(|e| repo_upload_failed(&repo.html_url, &e))?;
            // The link already works without the README (the page and importers read only the
            // list), so a failure here is a warning, as it is for a gist.
            if let Err(e) = github::put_repo_file(
                &token,
                &owner,
                &rname,
                "README.md",
                &readme,
                "Add README (via Silo)",
            ) {
                warning = Some(readme_failed(&e));
            }
            page_url
        } else {
            // P1: a secret (unlisted) gist for private / group sharing. Create with the JSON,
            // then attach a README linking the handoff page (its URL needs the new gist id).
            let gist = github::create_secret_gist(
                &token,
                &format!("{name}: a Silo mod set"),
                collection::FILE_NAME,
                &json,
            )?;
            let page_url = collection::gist_page_url(&gist.id);
            let readme = collection::readme(&coll, &page_url, &gist.html_url);
            // Best-effort: the JSON + share link already work without it.
            let _ = github::update_gist_file(&token, &gist.id, "README.md", &readme);
            page_url
        };
        Ok(ExportResult {
            url,
            count: coll.mods.len(),
            omitted,
            unpinned,
            warning,
        })
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(error::AppError::from)
}

/// A public collection's repo was created but saving its list failed: name the repo. A retry
/// carries on there while it's still empty (`create_public_repo` resumes into it). If the save
/// did land and only its answer was lost, the repo isn't empty and the list is already shared:
/// it shows under "Your shared links".
fn repo_upload_failed(repo_url: &str, cause: &str) -> String {
    format!(
        "Silo created {repo_url} on your GitHub but couldn't confirm the mod list was saved \
         to it. Try again: if the list never arrived, Silo carries on in that repository; if it \
         did, the link is already under Your shared links. ({cause})"
    )
}

/// The README of a public collection couldn't be written; the link works anyway.
fn readme_failed(cause: &str) -> String {
    format!(
        "The link works, but Silo couldn't add the page that describes the set on GitHub. \
         Updating the link later tries again. ({cause})"
    )
}

/// Update an existing collection in place: re-pin `mods` (the current active set) and write
/// the new list back to the *same* gist/repo, keeping its name/description/author/date — so
/// the share link never changes. Returns the (unchanged) page URL + new count.
#[tauri::command]
async fn collection_update(
    app: tauri::AppHandle,
    reference: String,
    mods: Vec<mpsync::ModRef>,
) -> Result<ExportResult, error::AppError> {
    let base = siloapi_base(&app)?;
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || -> Result<ExportResult, String> {
        let conn = db::open(&db)?;
        let token = gh_bearer_required(&conn)?;
        let where_ = collection::parse_collection_ref(&reference)
            .ok_or_else(|| "That shared link isn't valid.".to_string())?;

        // Preserve the collection's identity (name/description/author/date); re-pin the mods.
        let existing = read_collection_json(&reference, Some(&token))?;
        let PinnedSet {
            entries,
            omitted,
            unpinned,
        } = build_collection_entries(&base, Some(&conn), mods);
        if entries.is_empty() {
            return Err(
                "Nothing to update. Your active mods include no packaged (.zip) mods.".into(),
            );
        }
        let coll = collection::Collection {
            schema: collection::SCHEMA.to_string(),
            name: existing.name,
            description: existing.description,
            author: existing.author,
            created_at: existing.created_at,
            savegame: existing.savegame,
            mods: entries,
        };
        let json = collection::to_json(&coll)?;

        let mut warning = None;
        let url = match where_ {
            collection::CollectionRef::Gist(id) => {
                let page_url = collection::gist_page_url(&id);
                let source_url = format!("https://gist.github.com/{id}");
                let readme = collection::readme(&coll, &page_url, &source_url);
                github::update_gist_file(&token, &id, collection::FILE_NAME, &json)?;
                let _ = github::update_gist_file(&token, &id, "README.md", &readme);
                page_url
            }
            collection::CollectionRef::Repo { owner, repo } => {
                let page_url = collection::repo_page_url(&owner, &repo);
                let source_url = format!("https://github.com/{owner}/{repo}");
                let readme = collection::readme(&coll, &page_url, &source_url);
                github::put_repo_file(
                    &token,
                    &owner,
                    &repo,
                    collection::FILE_NAME,
                    &json,
                    "Update collection (via Silo)",
                )?;
                // The list is updated already, so a README failure is a warning, as on export.
                if let Err(e) = github::put_repo_file(
                    &token,
                    &owner,
                    &repo,
                    "README.md",
                    &readme,
                    "Update README (via Silo)",
                ) {
                    warning = Some(readme_failed(&e));
                }
                page_url
            }
        };
        Ok(ExportResult {
            url,
            count: coll.mods.len(),
            omitted,
            unpinned,
            warning,
        })
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(error::AppError::from)
}

/// Resolve a pasted collection link — a gist (P1) or a public repo (P2) — and read +
/// parse its `silo-collection.json`. Token optional (public sources read anonymously).
fn read_collection_json(
    url_or_id: &str,
    token: Option<&gh_session::Bearer>,
) -> Result<collection::Collection, String> {
    read_collection_raw(url_or_id, token).map(|(c, _)| c)
}

/// A shared set to preview or install: a `.silo` / collection file the user picked (`file`), or
/// else a gist / repo link. The returned text is what the fingerprint covers.
fn read_set_source(
    url_or_id: &str,
    file: Option<&str>,
    token: Option<&gh_session::Bearer>,
) -> Result<(collection::Collection, String), String> {
    let Some(path) = file else {
        return read_collection_raw(url_or_id, token);
    };
    let json = paths::read_inbound_text(
        std::path::Path::new(path),
        &["silo", "json"],
        paths::SHARED_FILE_MAX_BYTES,
    )?;
    Ok((collection::parse_set_file(&json)?, json))
}

/// `read_collection_json`, plus the file's exact text (for `collection::fingerprint`, so apply
/// can refuse a collection that changed after its preview).
fn read_collection_raw(
    url_or_id: &str,
    token: Option<&gh_session::Bearer>,
) -> Result<(collection::Collection, String), String> {
    let reference = collection::parse_collection_ref(url_or_id)
        .ok_or_else(|| {
            "That doesn't look like a Silo set link. Links look like https://silo.hllmr.com/c/… or a GitHub gist."
                .to_string()
        })?;
    let json = match &reference {
        collection::CollectionRef::Gist(id) => {
            github::read_gist_file(id, collection::FILE_NAME, token)
        }
        collection::CollectionRef::Repo { owner, repo } => {
            github::read_repo_file(owner, repo, collection::FILE_NAME, token)
        }
    }?;
    Ok((collection::parse(&json)?, json))
}

/// Resolve every mod of a collection by techName with one catalog lookup, and fingerprint the
/// file together with that result. Preview and apply both go through here, so apply installs
/// exactly what the preview showed or refuses (`COLLECTION_CHANGED`).
///
/// A failed lookup is an error, not an empty answer: an empty answer would show every mod as
/// "not in the catalog" while the catalog is only unreachable.
fn resolve_collection(
    base: &str,
    coll: &collection::Collection,
    json: &str,
) -> Result<(Vec<collection::Target>, String), String> {
    let names: Vec<String> = coll.mods.iter().map(|m| m.tech_name.clone()).collect();
    let hits = siloapi::lookup(base, &names).map_err(|e| catalog_lookup_failed(&e))?;
    let targets = collection::resolve_targets(&coll.mods, &catalog_matches(&hits));
    let fingerprint = collection::fingerprint(json, &targets);
    Ok((targets, fingerprint))
}

/// What a shared set's preview or install says when the catalog lookup fails (the cause stays
/// in the text, so it still classifies as offline / timeout).
fn catalog_lookup_failed(cause: &str) -> String {
    format!("Silo couldn't look up this set's mods in its catalog, so nothing was changed. Try again in a moment. ({cause})")
}

/// The catalog lookup keyed by techName, for `collection::resolve_targets`.
fn catalog_matches(
    hits: &[siloapi::LookupResult],
) -> std::collections::HashMap<String, collection::CatalogMatch> {
    hits.iter()
        .filter_map(|r| {
            Some((
                r.tech_name.clone()?,
                collection::CatalogMatch {
                    id: r.id.clone(),
                    download_source: r.download.as_ref().map(|d| d.source.clone()),
                },
            ))
        })
        .collect()
}

/// A collection the user has published, for the "Your collections" management list.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CollectionSummary {
    /// `"gist"` | `"repo"`.
    kind: String,
    /// Gist id, or `"owner/repo"` — what `collection_delete` takes.
    reference: String,
    name: String,
    mod_count: usize,
    created_at: Option<String>,
    /// The silo.hllmr.com/c/ share link.
    page_url: String,
    /// The raw gist/repo URL on GitHub.
    source_url: String,
    /// Whether Silo can delete it in-app (gists: yes; repos need a scope we don't hold).
    can_delete: bool,
}

/// The "Your collections" list, and what's missing from it when part of it couldn't be read.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CollectionList {
    items: Vec<CollectionSummary>,
    /// Set when the list is partial: a listing (gists or repos) or some collections couldn't
    /// be read.
    warning: Option<String>,
}

/// One listing's collections, and how many it found but couldn't read (a request that failed,
/// not a file that's absent or isn't a collection, which are simply not collections).
#[derive(Default)]
struct Listing {
    items: Vec<CollectionSummary>,
    unreadable: usize,
}

/// Merge the gist and repo listings (#355). Both failed: the error, so a broken token or an
/// outage never shows as "nothing shared yet". One failed, or some collections couldn't be
/// read: what was found, plus a warning.
fn merge_collection_listings(
    gists: Result<Listing, String>,
    repos: Result<Listing, String>,
) -> Result<CollectionList, String> {
    let mut warnings: Vec<String> = Vec::new();
    let (gists, repos) = match (gists, repos) {
        (Err(g), Err(_)) => return Err(g),
        (Ok(g), Err(e)) => {
            warnings.push(format!(
                "Silo couldn't list your public GitHub repositories, so links shared that way \
                 aren't shown. ({e})"
            ));
            (g, Listing::default())
        }
        (Err(e), Ok(r)) => {
            warnings.push(format!(
                "Silo couldn't list your unlisted links on GitHub, so they aren't shown. ({e})"
            ));
            (Listing::default(), r)
        }
        (Ok(g), Ok(r)) => (g, r),
    };
    let unreadable = gists.unreadable + repos.unreadable;
    if unreadable > 0 {
        warnings.push(format!(
            "Silo couldn't read {unreadable} of your shared links, so {} shown. Try again in a \
             moment.",
            if unreadable == 1 {
                "it isn't"
            } else {
                "they aren't"
            }
        ));
    }
    let mut items = gists.items;
    items.extend(repos.items);
    // Newest first (gists and repos interleaved by creation time).
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(CollectionList {
        items,
        warning: (!warnings.is_empty()).then(|| warnings.join(" ")),
    })
}

/// List the collections the user has published to their GitHub — secret gists (carrying the
/// collection file) and `silo-`-prefixed public repos. A repo without the collection file, or
/// a file that doesn't parse, is skipped (not a collection). A request that fails is not
/// skipped silently: see `merge_collection_listings`.
#[tauri::command]
async fn collections_list(app: tauri::AppHandle) -> Result<CollectionList, String> {
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || -> Result<CollectionList, String> {
        let conn = db::open(&db)?;
        let token = gh_bearer_required(&conn)?;

        let gists = github::list_user_gists(&token).map(|gists| {
            let mut out = Listing::default();
            for g in gists {
                if !g.filenames.iter().any(|f| f == collection::FILE_NAME) {
                    continue;
                }
                // The listing says the file is there, so a failed read is a real failure.
                let Ok(json) = github::read_gist_file(&g.id, collection::FILE_NAME, Some(&token))
                else {
                    out.unreadable += 1;
                    continue;
                };
                if let Ok(coll) = collection::parse(&json) {
                    out.items.push(CollectionSummary {
                        kind: "gist".into(),
                        reference: g.id.clone(),
                        name: coll.name,
                        mod_count: coll.mods.len(),
                        created_at: g.created_at,
                        page_url: collection::gist_page_url(&g.id),
                        source_url: g.html_url,
                        can_delete: true,
                    });
                }
            }
            out
        });

        let repos = github::list_owned_repos(&token).map(|repos| {
            let mut out = Listing::default();
            for r in repos {
                if !r.name.starts_with("silo-") {
                    continue;
                }
                let (owner, rname) = r.full_name.split_once('/').unwrap_or(("", r.name.as_str()));
                let json = match github::read_repo_file_if_present(
                    owner,
                    rname,
                    collection::FILE_NAME,
                    Some(&token),
                ) {
                    Ok(Some(json)) => json,
                    Ok(None) => continue, // a silo- repo that isn't a collection
                    Err(_) => {
                        out.unreadable += 1;
                        continue;
                    }
                };
                if let Ok(coll) = collection::parse(&json) {
                    out.items.push(CollectionSummary {
                        kind: "repo".into(),
                        reference: r.full_name.clone(),
                        name: coll.name,
                        mod_count: coll.mods.len(),
                        created_at: r.created_at,
                        page_url: collection::repo_page_url(owner, rname),
                        source_url: r.html_url,
                        can_delete: false,
                    });
                }
            }
            out
        });

        merge_collection_listings(gists, repos)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Delete a published collection. Gists (the private-share default) delete in-app with the
/// `gist` scope; public repos need `delete_repo` (not requested) so those are removed on GitHub.
#[tauri::command]
async fn collection_delete(
    app: tauri::AppHandle,
    kind: String,
    reference: String,
) -> Result<(), error::AppError> {
    // Validate the reference before any request: a gist id must be a real gist id, and a
    // repo reference a real `owner/repo` (repos are refused below, but never on bad input).
    match kind.as_str() {
        "gist" => github::check_gist_id(&reference)?,
        _ => {
            let (owner, repo) = reference
                .split_once('/')
                .unwrap_or((reference.as_str(), ""));
            github::check_repo(owner, repo)?;
        }
    }
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let conn = db::open(&db)?;
        let token = gh_bearer_required(&conn)?;
        match kind.as_str() {
            "gist" => github::delete_gist(&token, &reference),
            _ => Err(
                "Silo can delete unlisted links here. For a public repository, delete it on GitHub."
                    .into(),
            ),
        }
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(error::AppError::from)
}

/// One mod in an import plan, tagged with what the importer would do about it.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PlanRow {
    tech_name: String,
    /// Version pinned by the collection.
    version: Option<String>,
    /// The source the importer would use / send the user to.
    source: Option<String>,
    /// For `version_drift`: the version already in the library.
    installed_version: Option<String>,
}

/// A read-only preview of importing a shared collection: what you already have, what's a
/// different version, what Silo can install for you (a directly-downloadable source), and
/// what you'll need to fetch yourself (ModHub gates downloads) or that isn't catalogued.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportPlan {
    name: String,
    description: Option<String>,
    author: Option<String>,
    will_install: Vec<PlanRow>,
    open_page: Vec<PlanRow>,
    already_present: Vec<PlanRow>,
    version_drift: Vec<PlanRow>,
    unresolved: Vec<PlanRow>,
    /// Hash of the collection file and its catalog matches as previewed (`resolve_collection`);
    /// `collection_apply` refuses if either changed.
    fingerprint: String,
}

/// Fetch a shared collection by gist link/id, parse it, and bucket every mod against the
/// caller's installed set + the catalog — WITHOUT touching a single file. One batch catalog
/// lookup covers the whole list. The apply step (which does install) is a separate command.
#[tauri::command]
async fn collection_import_preview(
    app: tauri::AppHandle,
    url_or_id: String,
    installed: Vec<LocalMod>,
    file: Option<String>,
) -> Result<ImportPlan, error::AppError> {
    let base = siloapi_base(&app)?;
    let db = db_path(&app)?;
    tauri::async_runtime::spawn_blocking(move || -> Result<ImportPlan, String> {
        let conn = db::open(&db)?;
        let token = gh_bearer(&conn);
        let (coll, json) = read_set_source(&url_or_id, file.as_deref(), token.as_ref())?;

        let installed_by: std::collections::HashMap<&str, Option<&str>> = installed
            .iter()
            .map(|m| (m.tech_name.as_str(), m.version.as_deref()))
            .collect();

        // One lookup resolves catalog presence + best installable source for every mod, by
        // techName, exactly as apply resolves them.
        let (targets, fingerprint) = resolve_collection(&base, &coll, &json)?;

        let mut plan = ImportPlan {
            name: coll.name.clone(),
            description: coll.description.clone(),
            author: coll.author.clone(),
            will_install: Vec::new(),
            open_page: Vec::new(),
            already_present: Vec::new(),
            version_drift: Vec::new(),
            unresolved: Vec::new(),
            fingerprint,
        };

        for (m, t) in coll.mods.iter().zip(&targets) {
            let row = |installed_version: Option<String>| PlanRow {
                tech_name: m.tech_name.clone(),
                version: m.version.clone(),
                source: t.display_source.clone(),
                installed_version,
            };

            match installed_by.get(m.tech_name.as_str()) {
                Some(have) => {
                    let have = have.map(|s| s.to_string());
                    // A pinned version that differs from what's installed is a drift row;
                    // no pin (or an equal pin) counts as satisfied.
                    let drift = matches!((&m.version, &have), (Some(w), Some(h)) if w != h);
                    if drift {
                        plan.version_drift.push(row(have));
                    } else {
                        plan.already_present.push(row(have));
                    }
                }
                None => {
                    if t.download_source.is_some() {
                        plan.will_install.push(row(None));
                    } else if t.catalog_id.is_some() {
                        // In the catalog, but no direct download (ModHub gates it).
                        plan.open_page.push(row(None));
                    } else {
                        plan.unresolved.push(row(None));
                    }
                }
            }
        }
        Ok(plan)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(error::AppError::from)
}

/// The outcome for one mod in an apply run.
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ApplyRow {
    tech_name: String,
    /// The installed filename, when Silo downloaded it this run.
    filename: Option<String>,
    /// "installed" (downloaded now) · "present" (already had it) · "skipped" (get it
    /// yourself / not catalogued) · "failed".
    status: String,
    /// Provenance verdict for what landed: "verified" / "modified" / "unverified".
    verdict: Option<String>,
    /// A failure or open-page reason.
    detail: Option<String>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ApplyReport {
    /// The loadout created from the collection's full mod list; `None` when the library
    /// stayed busy and the finished downloads couldn't be added (those rows say so).
    loadout_id: Option<i64>,
    installed: usize,
    failed: usize,
    rows: Vec<ApplyRow>,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ApplyProgress {
    done: usize,
    total: usize,
    current: String,
}

/// Import a shared collection: download every directly-installable mod that's missing,
/// verify each against the version the collection pinned, and save the whole set as a
/// loadout. Mods that must be fetched from ModHub (or aren't catalogued) are recorded
/// but never faked — the loadout still lists them so it completes once the user grabs them.
///
/// Every download reuses the same guarded path as a Browse install (basename check, stream
/// to `.part`, archive + identity validation, skip-if-present) — no new file-write surface.
///
/// Two phases: every download runs first, without the library lock, into its own `.part`;
/// then the lock is taken only to place the parts (re-checking each destination is still
/// free) and save the loadout. Any part left over on an error is removed.
#[tauri::command]
async fn collection_apply(
    app: tauri::AppHandle,
    url_or_id: String,
    installed: Vec<LocalMod>,
    fingerprint: String,
    root: Option<String>,
    file: Option<String>,
) -> Result<ApplyReport, error::AppError> {
    ensure_no_move()?;
    let base = siloapi_base(&app)?;
    let db = db_path(&app)?;
    let root = primary_root(root)?;
    let emitter = app.clone();
    tauri::async_runtime::spawn_blocking(move || -> Result<ApplyReport, String> {
        let mut conn = db::open(&db)?;
        let token = gh_bearer(&conn);
        let (coll, json) = read_set_source(&url_or_id, file.as_deref(), token.as_ref())?;
        // Resolve every mod by techName, exactly as the preview did. A collection's own
        // `catalog_id` and source hint never decide what downloads. The fingerprint covers the
        // file and this resolution, so a changed gist or catalog answer is refused.
        let (targets, fp) = resolve_collection(&base, &coll, &json)?;
        const COLLECTION_CHANGED: &str =
            "This set changed since you previewed it. Preview it again before installing.";
        if fp != fingerprint {
            return Err(COLLECTION_CHANGED.into());
        }

        let have: std::collections::HashSet<&str> =
            installed.iter().map(|m| m.tech_name.as_str()).collect();

        let total = coll.mods.len();
        let mut rows: Vec<ApplyRow> = Vec::with_capacity(total);
        let mut installed_count = 0usize;
        let mut failed_count = 0usize;
        // Finished downloads waiting to be placed under the lock (their rows are filled then).
        let mut pending: Vec<PendingPlace> = Vec::new();
        // (row, row of the pending download of the same file) for a set naming one file twice.
        let mut duplicates: Vec<(usize, usize)> = Vec::new();

        for (i, (m, t)) in coll.mods.iter().zip(&targets).enumerate() {
            let _ = emitter.emit(
                "collection:progress",
                ApplyProgress {
                    done: i,
                    total,
                    current: m.tech_name.clone(),
                },
            );

            // Already in the library — don't re-download; still verify if we can.
            if have.contains(m.tech_name.as_str()) {
                rows.push(ApplyRow {
                    tech_name: m.tech_name.clone(),
                    filename: None,
                    status: "present".into(),
                    verdict: None,
                    detail: None,
                });
                continue;
            }

            let Some(cid) = t.catalog_id.as_deref() else {
                rows.push(ApplyRow {
                    tech_name: m.tech_name.clone(),
                    filename: None,
                    status: "skipped".into(),
                    verdict: None,
                    detail: Some("Not in Silo's catalog. Find it on its own page.".into()),
                });
                continue;
            };
            // Catalogued, but no direct download (ModHub gates it): the preview's open-page row.
            if t.download_source.is_none() {
                rows.push(gated_row(&m.tech_name));
                continue;
            }

            match siloapi::resolve_download(&base, cid, t.download_source.as_deref()) {
                Ok(resolved) => {
                    // A plain basename, named after the mod this set asks for (never renamed).
                    if let Err(e) = paths::safe_file_name(&resolved.filename).and_then(|_| {
                        siloapi::check_download_name(&resolved.filename, &m.tech_name)
                    }) {
                        rows.push(fail_row(&m.tech_name, e));
                        failed_count += 1;
                        continue;
                    }
                    let dest = root.join(&resolved.filename);
                    // Already there, or another mod in this collection downloads that very file.
                    let same_name = |p: &PendingPlace| {
                        p.dest.file_name().is_some_and(|n| {
                            n.to_string_lossy().to_lowercase() == resolved.filename.to_lowercase()
                        })
                    };
                    if path_taken(&dest) {
                        rows.push(present_row(&m.tech_name, &resolved.filename));
                        continue;
                    }
                    // Settled once that download is placed (or isn't): see `settle_duplicates`.
                    if let Some(p) = pending.iter().find(|p| same_name(p)) {
                        duplicates.push((rows.len(), p.row));
                        rows.push(present_row(&m.tech_name, &resolved.filename));
                        continue;
                    }
                    let part = match claim_part(&dest, Some(&db)) {
                        Ok(part) => part,
                        // Silo couldn't record the part: nothing was downloaded.
                        Err(ClaimError::Journal(e)) => {
                            rows.push(fail_row(&m.tech_name, e));
                            failed_count += 1;
                            continue;
                        }
                        Err(ClaimError::Busy(_)) => {
                            // A different name the claim treats as the same file as one of this
                            // set's own downloads: never reported as present.
                            let key = same_file_key(&dest);
                            if let Some(p) = pending
                                .iter()
                                .find(|p| same_file_key(&p.dest).overlaps(&key))
                            {
                                rows.push(fail_row(
                                    &m.tech_name,
                                    format!(
                                        "Its file name can't be told apart from {} in this set. \
                                     Install it on its own afterwards.",
                                        file_label(&p.dest)
                                    ),
                                ));
                                failed_count += 1;
                                continue;
                            }
                            // Taken only by another download running right now (e.g. from Browse).
                            rows.push(ApplyRow {
                                tech_name: m.tech_name.clone(),
                                filename: Some(resolved.filename.clone()),
                                status: "skipped".into(),
                                verdict: None,
                                detail: Some("Already downloading elsewhere in Silo.".into()),
                            });
                            continue;
                        }
                    };
                    match siloapi::download_to_part(
                        &resolved.url,
                        &part,
                        resolved.expected_sha256.as_deref(),
                        |_done, _total| {},
                    ) {
                        Ok(()) => {
                            // Same bytes as the placed file will have; hashed before the lock.
                            let verdict =
                                verdict_against_pin(part.path(), m.manifest_hash.as_deref());
                            pending.push(PendingPlace {
                                row: rows.len(),
                                part,
                                dest,
                                verdict,
                            });
                            rows.push(ApplyRow {
                                tech_name: m.tech_name.clone(),
                                filename: Some(resolved.filename.clone()),
                                status: "installed".into(),
                                verdict: None,
                                detail: None,
                            });
                        }
                        Err(e) => {
                            rows.push(fail_row(&m.tech_name, e));
                            failed_count += 1;
                        }
                    }
                }
                // No direct download (ModHub gates it) — leave it for the user.
                Err(siloapi::ResolveError::Gated(_)) => rows.push(gated_row(&m.tech_name)),
                // Anything else is a failure, with its cause, not a site restriction.
                Err(siloapi::ResolveError::Operational(e)) => {
                    rows.push(fail_row(&m.tech_name, e));
                    failed_count += 1;
                }
            }
        }

        // Phase 2, under the lock: place each download whose destination is still free, then
        // save the loadout. Dropping a `PendingPlace` (any early return) removes its part.
        // Finished downloads wait out a busy spell (`PLACE_WAIT`) rather than being dropped.
        let lock = library_write_within(PLACE_WAIT);
        if let Err(e) = &lock {
            if pending.is_empty() {
                return Err(e.clone()); // nothing downloaded, nothing lost
            }
        }
        let locked = lock.as_ref().map(|_| ()).map_err(String::clone);
        // The mods folder may have been switched while the downloads ran.
        let (placed, place_failed) = place_pending(&mut rows, pending, locked, |dest| {
            install_recheck(&root, dest)
        });
        installed_count += placed;
        failed_count += place_failed + settle_duplicates(&mut rows, &duplicates);

        // The loadout lists the whole collection; applying it activates the ones you have,
        // and it completes automatically once you fetch the rest and rescan. Not saved when
        // the library stayed busy (the report says which downloads couldn't be added).
        let loadout_id = match lock {
            Ok(_write) => Some(db::save_loadout(
                &mut conn,
                None,
                &coll.name,
                &coll.tech_names(),
            )?),
            Err(_) => None,
        };

        let _ = emitter.emit(
            "collection:progress",
            ApplyProgress {
                done: total,
                total,
                current: String::new(),
            },
        );
        Ok(ApplyReport {
            loadout_id,
            installed: installed_count,
            failed: failed_count,
            rows,
        })
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(error::AppError::from)
}

/// A collection download that finished and waits to be placed under the library lock.
struct PendingPlace {
    /// Index of its row in the report.
    row: usize,
    part: PartClaim,
    dest: PathBuf,
    verdict: String,
}

/// Fill in the rows of finished collection downloads: placed (`installed`, with its verdict),
/// `present` (the file appeared meanwhile; theirs is kept), or `failed`. `locked` is whether
/// the library lock is held; without it every pending row fails with that reason. `recheck`
/// is asked again for each destination under the lock (`install_recheck`); a refusal fails
/// that row. Each part not placed is removed as its claim drops. Returns (installed, failed).
fn place_pending(
    rows: &mut [ApplyRow],
    pending: Vec<PendingPlace>,
    locked: Result<(), String>,
    recheck: impl Fn(&std::path::Path) -> Result<(), String>,
) -> (usize, usize) {
    let (mut installed, mut failed) = (0, 0);
    for p in pending {
        let row = &mut rows[p.row];
        if let Err(e) = locked.clone().and_then(|()| recheck(&p.dest)) {
            *row = fail_row(&row.tech_name, e);
            failed += 1;
        } else if path_taken(&p.dest) {
            // Appeared while we downloaded (another install): keep theirs.
            row.status = "present".into();
        } else {
            match place_part(p.part.path(), &p.dest) {
                Ok(()) => {
                    installed += 1;
                    row.verdict = Some(p.verdict);
                }
                Err(e) => {
                    *row = fail_row(&row.tech_name, e);
                    failed += 1;
                }
            }
        }
    }
    (installed, failed)
}

/// A row that shares its file with another row's download is `present` only if that download
/// was placed (or found already there); when it failed, so does this row, with the same reason.
/// Returns how many rows failed.
fn settle_duplicates(rows: &mut [ApplyRow], duplicates: &[(usize, usize)]) -> usize {
    let mut failed = 0;
    for &(row, of) in duplicates {
        if rows[of].status == "failed" {
            let detail = rows[of].detail.clone().unwrap_or_default();
            rows[row] = fail_row(&rows[row].tech_name, detail);
            failed += 1;
        }
    }
    failed
}

fn present_row(tech_name: &str, filename: &str) -> ApplyRow {
    ApplyRow {
        tech_name: tech_name.to_string(),
        filename: Some(filename.to_string()),
        status: "present".into(),
        verdict: None,
        detail: None,
    }
}

fn fail_row(tech_name: &str, detail: String) -> ApplyRow {
    ApplyRow {
        tech_name: tech_name.to_string(),
        filename: None,
        status: "failed".into(),
        verdict: None,
        detail: Some(detail),
    }
}

/// A catalogued mod with no direct download (ModHub gates it), left for the user.
fn gated_row(tech_name: &str) -> ApplyRow {
    ApplyRow {
        tech_name: tech_name.to_string(),
        filename: None,
        status: "skipped".into(),
        verdict: None,
        detail: Some("This site doesn't allow direct downloads. Get it from its page.".into()),
    }
}

/// Hash a freshly-installed zip and compare it to the hash the collection pinned:
/// "verified" (content match), "modified" (differs — not what the curator shared), or
/// "unverified" (the collection carried no hash to check against).
fn verdict_against_pin(zip: &std::path::Path, pinned: Option<&str>) -> String {
    let Some(pinned) = pinned else {
        return "unverified".into();
    };
    match provenance::manifest_from_zip(zip) {
        Ok(local) if local.manifest_hash.eq_ignore_ascii_case(pinned) => "verified".into(),
        Ok(_) => "modified".into(),
        Err(_) => "unverified".into(),
    }
}

/// Silo's own folders inside the mods folders the game may read, as `logscan::folder_key`s:
/// each one's `backups/`, and its archive when that's the default `<mods>/archive` (a custom
/// archive can't be inside the mods folder). The game looks inside every folder in mods for a
/// `modDesc.xml`, so it logs a failure for these on every start (#385). Without `conn`, only
/// the folders in use right now, with their published or default archive.
fn silo_folders_in_mods(conn: Option<&rusqlite::Connection>) -> Vec<String> {
    let active = modsroot::active();
    let mut folders = live_mods_folders();
    if let Some(c) = conn {
        folders.extend(known_mods_folders(c, &[]));
    }
    silo_folder_keys(&folders, |mods| match &active {
        Some(a) if modsroot::same_folder(&a.layout.mods, mods) => a.layout.archive.clone(),
        _ => conn.map_or_else(
            || organize::Layout::default_for(mods).archive,
            |c| archive_of(c, mods),
        ),
    })
}

/// Pure but for comparing folders: [`silo_folders_in_mods`] over these mods folders, each with
/// the archive `archive_of` gives it. One key per folder.
fn silo_folder_keys(
    folders: &[PathBuf],
    archive_of: impl Fn(&std::path::Path) -> PathBuf,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |p: PathBuf| {
        let k = logscan::folder_key(&p.to_string_lossy());
        if !out.contains(&k) {
            out.push(k);
        }
    };
    for mods in folders {
        let layout = organize::Layout {
            mods: mods.clone(),
            archive: archive_of(mods),
        };
        if layout.archive_is_default() {
            // Spelled from the mods folder, the way the game's log spells it.
            push(organize::Layout::default_for(mods).archive);
        }
        push(mods.join("backups"));
    }
    out
}

/// Parse the FS25 log.txt and report which mods are throwing errors/warnings, whether
/// the last run crashed, and the culprit ranking. Read-only; touches nothing.
///
/// `path`: the log a `log_status` named, so the report is of that log even if the mods folder's
/// FS25 profile changed since (#387); `None` reads the current one. Only a log Silo reads is
/// accepted (`log_target`).
#[tauri::command]
async fn scan_log(
    app: tauri::AppHandle,
    path: Option<String>,
) -> Result<logscan::LogReport, error::AppError> {
    let path = log_target(path.as_deref(), fsgame::user_dir(), gameprofile::published())?
        .ok_or_else(|| "Silo couldn't find Farming Simulator 25's user folder. Start the game once, then try again.".to_string())?;
    let db = db_path(&app).ok();
    tauri::async_runtime::spawn_blocking(move || -> Result<logscan::LogReport, error::AppError> {
        if !path.exists() {
            // The game hasn't been played yet: an empty state, not a failure.
            return Err(error::AppError::new(
                error::code::NO_LOG,
                "No game log yet. Play Farming Simulator 25 once, then come back here.",
            ));
        }
        // Logs are read as lossy UTF-8; FS writes mostly ASCII but paths can carry stray bytes.
        // A runaway log is read as its start and end only, so it can't exhaust memory (#351).
        let file = std::fs::File::open(&path)?;
        // Best effort: without the database, the folders in use right now still count.
        let conn = db.and_then(|d| db::open(&d).ok());
        let silo = silo_folders_in_mods(conn.as_ref());
        Ok(logscan::scan_with(
            file,
            logscan::LOG_HEAD_BYTES,
            logscan::LOG_TAIL_BYTES,
            &silo,
        )?)
    })
    .await
    .map_err(|e| error::AppError::from(e.to_string()))?
}

/// Crash detection's cheap check (Diagnose, #260): where log.txt is, when it last changed,
/// whether the game is running, and the change time Silo last looked at. A stat and a process
/// list, off the main thread; the frontend calls it on focus and every 5 s after a launch.
#[tauri::command]
async fn log_status(app: tauri::AppHandle) -> Result<logscan::LogStatus, String> {
    let db = db_path(&app)?;
    off_main(move || {
        // One read of the profile: the path, its change time and its last-seen marker all
        // describe the same log.
        let main = fsgame::user_dir();
        let path =
            gameprofile::resolve(gameprofile::Place::Log, main.clone(), gameprofile::active())
                .map(|d| d.join("log.txt"));
        let modified_ms = path.as_deref().and_then(logscan::modified_ms);
        let conn = db::open(&db)?;
        let key = log_seen_key_for(path.as_deref(), main.as_deref());
        let last_seen_ms = logscan::parse_last_seen(db::get_app_setting(&conn, &key).as_deref());
        Ok(logscan::LogStatus {
            path: path.map(|p| p.to_string_lossy().into_owned()),
            modified_ms,
            // Any FS25 process counts, whichever profile it runs on (#387). A game on another
            // profile only delays the look until it exits. Telling profiles apart by command line
            // isn't reliable (the launcher starts `x64\FarmingSimulator2025Game.exe`, and whether
            // that keeps `-profile` is unverified), and a wrong "not running" would read a
            // running game's log, which has no exit marker yet, as a crash.
            game_running: gamelaunch::fs25_running(),
            last_seen_ms,
        })
    })
    .await
}

/// Remember that Silo looked at the log as of `modified_ms`, so the same run isn't announced
/// again (on the next focus or the next start). `path`: the log a `log_status` named, so the
/// time lands on that log's marker, never another profile's (#387); `None`: the current log.
#[tauri::command]
async fn mark_log_seen(
    app: tauri::AppHandle,
    modified_ms: i64,
    path: Option<String>,
) -> Result<(), String> {
    let db = db_path(&app)?;
    off_main(move || {
        let main = fsgame::user_dir();
        let log = log_target(path.as_deref(), main.clone(), gameprofile::published())?;
        let conn = db::open(&db)?;
        let key = log_seen_key_for(log.as_deref(), main.as_deref());
        db::set_app_setting(&conn, &key, Some(&modified_ms.to_string()))
    })
    .await
}

/// The log a command works on: `requested` (a `log_status` path) when it's `log.txt` in the main
/// FS25 user folder `main` or in the current folder's FS25 profile (`profile`, launch on or off);
/// anything else is refused. `None` requested: the current log (the profile's when its launch is
/// on, `gameprofile::Place::Log`). `Ok(None)`: no user folder is known.
fn log_target(
    requested: Option<&str>,
    main: Option<PathBuf>,
    profile: Option<gameprofile::Published>,
) -> Result<Option<PathBuf>, String> {
    let Some(req) = requested else {
        let active = profile.filter(|p| p.launch).map(|p| p.dir);
        return Ok(
            gameprofile::resolve(gameprofile::Place::Log, main, active).map(|d| d.join("log.txt"))
        );
    };
    let req = std::path::Path::new(req);
    main.into_iter()
        .chain(profile.map(|p| p.dir))
        .map(|d| d.join("log.txt"))
        .find(|l| modsroot::same_folder(l, req))
        .map(Some)
        .ok_or_else(|| "That isn't a Farming Simulator 25 log Silo reads.".to_string())
}

/// Where the log change time Silo last looked at is kept, for the log at `log`: per FS25
/// profile, so a profile's old crash isn't compared with the main log's time (#387). The main
/// user folder `main` keeps the original key (an upgrade doesn't re-announce an old crash).
fn log_seen_key_for(log: Option<&std::path::Path>, main: Option<&std::path::Path>) -> String {
    let profile = log
        .and_then(std::path::Path::parent)
        .filter(|d| !main.is_some_and(|m| modsroot::same_folder(m, d)))
        .map(modsroot::canonical_key);
    gameprofile::log_seen_key(logscan::LAST_SEEN_KEY, profile.as_deref())
}

/// Parse inputBinding.xml into a per-device binding map (read-only).
#[tauri::command]
async fn scan_bindings() -> Result<bindings::BindingReport, error::AppError> {
    let path = gameprofile::user_dir(gameprofile::Place::Bindings)
        .map(|d| d.join("inputBinding.xml"))
        .ok_or_else(|| "Silo couldn't find Farming Simulator 25's user folder. Start the game once, then try again.".to_string())?;
    tauri::async_runtime::spawn_blocking(
        move || -> Result<bindings::BindingReport, error::AppError> {
            if !path.exists() {
                // The game writes it on first launch: an empty state, not a failure.
                return Err(error::AppError::new(
                    error::code::NO_LOG,
                    "No control settings yet. Play Farming Simulator 25 once, then come back here.",
                ));
            }
            let bytes = std::fs::read(&path)?;
            Ok(bindings::parse(&String::from_utf8_lossy(&bytes)))
        },
    )
    .await
    .map_err(|e| error::AppError::from(e.to_string()))?
}

// ── Guided bisection ──

/// Decide the next split for the current suspect pool (pure; see bisect.rs).
#[tauri::command]
fn bisect_plan(pool: Vec<String>) -> bisect::BisectStep {
    bisect::plan(&pool)
}

/// Narrow the pool by the last round's verdict — kept in one tested place so the
/// test/rest halves can't get swapped by the caller.
#[tauri::command]
fn bisect_narrow(test: Vec<String>, rest: Vec<String>, still_broken: bool) -> Vec<String> {
    bisect::narrow(test, rest, still_broken)
}

/// Persist the user's real active set before bisection perturbs it, so even an app
/// crash mid-diagnosis can restore it (reversibility is non-negotiable — CLAUDE.md).
#[tauri::command]
async fn bisect_snapshot_save(app: tauri::AppHandle, active: Vec<String>) -> Result<(), String> {
    let db = db_path(&app)?;
    off_main(move || {
        let conn = db::open(&db)?;
        let json = serde_json::to_string(&active).map_err(|e| e.to_string())?;
        db::set_app_setting(&conn, "bisect_snapshot", Some(&json))
    })
    .await
}

/// The saved pre-bisection active set, if a bisection is (or was) in progress.
#[tauri::command]
async fn bisect_snapshot_get(app: tauri::AppHandle) -> Result<Option<Vec<String>>, String> {
    let db = db_path(&app)?;
    off_main(move || {
        let conn = db::open(&db)?;
        match db::get_app_setting(&conn, "bisect_snapshot") {
            Some(json) => Ok(Some(
                serde_json::from_str(&json).map_err(|e| e.to_string())?,
            )),
            None => Ok(None),
        }
    })
    .await
}

/// Clear the snapshot once the original set has been restored.
#[tauri::command]
async fn bisect_snapshot_clear(app: tauri::AppHandle) -> Result<(), String> {
    let db = db_path(&app)?;
    off_main(move || db::set_app_setting(&db::open(&db)?, "bisect_snapshot", None)).await
}

/// Read specific values from a config XML by path.
#[tauri::command]
async fn get_config(
    path: String,
    paths: Vec<String>,
) -> Result<std::collections::HashMap<String, String>, String> {
    off_main(move || {
        let user = gameprofile::user_dir(gameprofile::Place::GameConfig).ok_or_else(|| "Silo couldn't find Farming Simulator 25's user folder. Start the game once, then try again.".to_string())?;
        let xml = crate::paths::guarded_xml_read(&[user], std::path::Path::new(&path))?;
        Ok(xmlconfig::get_values(&xml, &paths))
    })
    .await
}

/// Apply value edits to a config XML. The write is confined to the FS25 user dir, backs up
/// the original first (required), and swaps the new file in atomically (see
/// `paths::guarded_xml_write`).
#[tauri::command]
async fn set_config(path: String, edits: Vec<xmlconfig::Edit>) -> Result<(), String> {
    off_main(move || {
        let _profile = game_files_guard();
        // Checked under the guard, which a launch holds while it starts the game: a write never
        // lands just after a launch it waited behind (#387).
        config_write_refusal(gamelaunch::fs25_running() || gamelaunch::profile_launch_pending())?;
        let user = gameprofile::user_dir(gameprofile::Place::GameConfig).ok_or_else(|| "Silo couldn't find Farming Simulator 25's user folder. Start the game once, then try again.".to_string())?;
        let roots = [user];
        let p = std::path::Path::new(&path);
        let xml = paths::guarded_xml_read(&roots, p)?;
        let out = xmlconfig::set_values(&xml, &edits)?;
        ensure_override_kept(&roots[0], p, Some(&xml), &out)?;
        paths::guarded_xml_write(&roots, p, &out)
    })
    .await
}

// ── Mod settings form ──
#[tauri::command]
async fn mods_with_settings() -> Result<Vec<String>, String> {
    off_main(|| {
        Ok(gameprofile::user_dir(gameprofile::Place::ModSettings)
            .map(|d| settings_form::mods_with_settings(&d))
            .unwrap_or_default())
    })
    .await
}

#[tauri::command]
async fn get_mod_settings(mod_name: String) -> Result<Vec<settings_form::SettingsFile>, String> {
    off_main(move || {
        let dir = gameprofile::user_dir(gameprofile::Place::ModSettings).ok_or_else(|| "Silo couldn't find Farming Simulator 25's user folder. Start the game once, then try again.".to_string())?;
        let mut files = Vec::new();
        for path in settings_form::find_files(&dir, &mod_name) {
            files.push(settings_form::load_file(&path)?);
        }
        Ok(files)
    })
    .await
}

#[tauri::command]
async fn save_mod_settings(path: String, edits: Vec<settings_form::Edit>) -> Result<(), String> {
    off_main(move || {
        let _profile = game_files_guard();
        // Checked under the guard, which a launch holds while it starts the game: a write never
        // lands just after a launch it waited behind (#387).
        config_write_refusal(gamelaunch::fs25_running() || gamelaunch::profile_launch_pending())?;
        let user = gameprofile::user_dir(gameprofile::Place::ModSettings).ok_or_else(|| "Silo couldn't find Farming Simulator 25's user folder. Start the game once, then try again.".to_string())?;
        let roots = [user];
        let p = std::path::Path::new(&path);
        let raw = paths::guarded_xml_read(&roots, p)?;
        let updated = settings_form::apply_edits(&raw, &edits)?;
        ensure_override_kept(&roots[0], p, Some(&raw), &updated)?;
        paths::guarded_xml_write(&roots, p, &updated)
    })
    .await
}

#[tauri::command]
async fn save_mod_settings_raw(path: String, content: String) -> Result<(), String> {
    off_main(move || {
        ensure_well_formed(&content)?;
        let _profile = game_files_guard();
        // Checked under the guard, which a launch holds while it starts the game: a write never
        // lands just after a launch it waited behind (#387).
        config_write_refusal(gamelaunch::fs25_running() || gamelaunch::profile_launch_pending())?;
        let user = gameprofile::user_dir(gameprofile::Place::ModSettings).ok_or_else(|| "Silo couldn't find Farming Simulator 25's user folder. Start the game once, then try again.".to_string())?;
        let p = std::path::Path::new(&path);
        ensure_override_kept(&user, p, None, &content)?;
        paths::guarded_xml_write(&[user], p, &content)
    })
    .await
}

/// Refuse text that isn't one well-formed XML document (parsed to the end: every tag closed
/// and matched, one root element, nothing but whitespace or comments around it; valid
/// names; every attribute quoted and named once; only the five predefined entities and
/// character references; an XML declaration only at the very start and a DOCTYPE only
/// before the root). The raw editor writes what the user typed, and a broken settings file
/// makes the mod fall back to its defaults or fail to load.
fn ensure_well_formed(xml: &str) -> Result<(), String> {
    use quick_xml::events::Event;
    let refuse = |why: String| {
        Err(format!(
            "That isn't valid XML, so Silo didn't save it ({why}). Fix it and save again."
        ))
    };
    if let Err(why) = xmltext::check_chars(xml) {
        return refuse(why);
    }
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().check_comments = true; // no `--` inside a comment
    let mut depth = 0usize;
    let mut roots = 0usize;
    let mut first = true;
    let mut doctype = false;
    loop {
        let event = reader.read_event();
        if let Ok(Event::Start(e) | Event::Empty(e)) = &event {
            if let Err(why) =
                xmltext::check_name(e.name().as_ref()).and_then(|()| xmltext::check_attributes(e))
            {
                return refuse(format!("before byte {}: {why}", reader.buffer_position()));
            }
        }
        let at_start = std::mem::replace(&mut first, false);
        match event {
            Err(e) => {
                return refuse(format!("at byte {}: {e}", reader.error_position()));
            }
            Ok(Event::Eof) => break,
            Ok(Event::Decl(_)) if !at_start => {
                return refuse("the <?xml ...?> declaration must come first".into());
            }
            Ok(Event::Decl(d)) => {
                if let Err(why) = xmltext::check_decl(&d) {
                    return refuse(why);
                }
            }
            Ok(Event::PI(p)) => {
                let target = p.target();
                if let Err(why) = xmltext::check_name(target) {
                    return refuse(format!("before byte {}: {why}", reader.buffer_position()));
                }
                if target.eq_ignore_ascii_case(b"xml") {
                    return refuse("the <?xml ...?> declaration must come first".into());
                }
            }
            Ok(Event::DocType(_)) if doctype || depth > 0 || roots > 0 => {
                return refuse("a <!DOCTYPE> can only come once, before the root element".into());
            }
            // Settings files have no DTD: only a bare `<!DOCTYPE name>` passes, since Silo
            // can't check an internal subset or an external one.
            Ok(Event::DocType(d)) => {
                if xmltext::check_name(String::from_utf8_lossy(&d).trim().as_bytes()).is_err() {
                    return refuse("a <!DOCTYPE> can only name the root element".into());
                }
                doctype = true;
            }
            Ok(Event::Start(_)) => {
                if depth == 0 {
                    roots += 1;
                }
                depth += 1;
            }
            Ok(Event::Empty(_)) if depth == 0 => roots += 1,
            Ok(Event::End(_)) => depth = depth.saturating_sub(1),
            Ok(Event::Text(t)) if depth == 0 && !t.iter().all(u8::is_ascii_whitespace) => {
                return refuse("text outside the root element".into());
            }
            Ok(Event::GeneralRef(_) | Event::CData(_)) if depth == 0 => {
                return refuse("text outside the root element".into());
            }
            // Only a CDATA section may end with `]]>`; in text it must be `]]&gt;`.
            Ok(Event::Text(t)) if t.windows(3).any(|w| w == b"]]>") => {
                return refuse("text contains ]]> outside a CDATA section".into());
            }
            Ok(Event::GeneralRef(r)) => {
                if let Err(why) = xmltext::check_reference(&String::from_utf8_lossy(&r)) {
                    return refuse(format!("before byte {}: {why}", reader.buffer_position()));
                }
            }
            Ok(_) => {}
        }
    }
    match (depth, roots) {
        (0, 1) => Ok(()),
        (d, _) if d > 0 => refuse(format!("{d} unclosed tag(s)")),
        (_, 0) => refuse("no root element".into()),
        _ => refuse("more than one root element".into()),
    }
}

// ── Savegames ──

/// What the app shows from the game's user folder, read together (`game_files`).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct GameFiles {
    /// The user folder the config editors open (`gameprofile::Place::GameConfig`).
    user_dir: Option<String>,
    savegames: Vec<savegame::Savegame>,
    mods_with_settings: Vec<String>,
    /// The FS25 profile these were read from (`None`: the main user folder), so the app labels
    /// them by where they came from, not by the status it holds.
    profile: Option<String>,
}

/// Savegames, the user folder and the mods with settings from one read of the FS25 profile in
/// use (#387), so a save listed from one profile is never shown beside another one's folder
/// (the config editors build a save's path from that folder).
#[tauri::command]
async fn game_files() -> Result<GameFiles, String> {
    off_main(|| {
        let main = fsgame::user_dir();
        let profile = gameprofile::active();
        let at = |place| gameprofile::resolve(place, main.clone(), profile.clone());
        Ok(GameFiles {
            user_dir: at(gameprofile::Place::GameConfig).map(|p| p.to_string_lossy().into_owned()),
            savegames: at(gameprofile::Place::Savegames)
                .map(|d| savegame::list_savegames(&d))
                .unwrap_or_default(),
            mods_with_settings: at(gameprofile::Place::ModSettings)
                .map(|d| settings_form::mods_with_settings(&d))
                .unwrap_or_default(),
            profile: profile.as_ref().map(|p| p.to_string_lossy().into_owned()),
        })
    })
    .await
}

#[tauri::command]
async fn get_savegames() -> Result<Vec<savegame::Savegame>, String> {
    off_main(|| {
        Ok(gameprofile::user_dir(gameprofile::Place::Savegames)
            .map(|dir| savegame::list_savegames(&dir))
            .unwrap_or_default())
    })
    .await
}

#[tauri::command]
async fn backup_savegame(folder: String, user_dir: Option<String>) -> Result<String, String> {
    off_main(move || {
        // Held through the backup: the profile can't be switched while a save is copied.
        let _profile = game_files_guard();
        let dir = gameprofile::user_dir(gameprofile::Place::Savegames).ok_or_else(|| "Silo couldn't find Farming Simulator 25's user folder. Start the game once, then try again.".to_string())?;
        // `user_dir`: the folder the save was listed from. A save named in one FS25 profile is
        // never backed up from another one's same-named folder (#387).
        if user_dir.is_some_and(|u| !modsroot::same_folder(std::path::Path::new(&u), &dir)) {
            return Err(SAVES_CHANGED.into());
        }
        savegame::backup(&dir, &folder)
    })
    .await
}

/// The app version from Cargo/tauri.conf package info — the single source of truth
/// the UI reads so a build always shows the number it was cut from.
#[tauri::command]
fn app_version(app: tauri::AppHandle) -> String {
    app.package_info().version.to_string()
}

/// One frontend error (or warning / note) into the persistent log, as a single line prefixed
/// `[frontend]` (`error::format_frontend_log`). Async so the file write never runs on the
/// main thread; never fails. Callers must not pass secrets: the log is shared via diagnostics.
#[tauri::command]
async fn log_frontend_error(level: String, message: String, context: Option<String>) {
    let (level, line) = error::format_frontend_log(&level, &message, context.as_deref());
    log::log!(level, "{line}");
}

/// Notices from startup work, each returned once (see `notices`).
#[tauri::command]
async fn take_startup_notices() -> Vec<String> {
    notices::take()
}

/// True when this machine can store account tokens in the OS keychain. When false, the UI
/// warns that a connected token would be kept in the local DB unencrypted.
#[tauri::command]
async fn secret_storage_secure() -> Result<bool, String> {
    off_main(|| Ok(secrets::keychain_available())).await
}

/// Files in the flat mods root that occupy an organized mod's name but aren't Silo's projection
/// (a build the user swapped in, a leftover, etc.). Silo never deletes them, but surfaces them
/// so the user knows their intended mod isn't what will load from that name.
// MUST be async + spawn_blocking: it hashes any flat file that isn't provably Silo's hardlink
// (copy-projections force a full content hash), which on a large library is gigabytes of I/O.
// A synchronous command runs on the main thread and would freeze the window ("Not Responding").
#[tauri::command]
async fn detect_foreign_files(
    app: tauri::AppHandle,
    root: Option<String>,
) -> Result<Vec<organize::ForeignFile>, String> {
    let db = db_path(&app)?;
    let layout = primary_layout(root)?;
    tauri::async_runtime::spawn_blocking(move || -> Result<Vec<organize::ForeignFile>, String> {
        let conn = db::open(&db)?;
        Ok(organize::detect_foreign_projections(&conn, &layout))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The log: `app_log_dir()/silo.log` when `to_file` (Windows `%LOCALAPPDATA%\com.hllmr.silo\logs`,
/// macOS `~/Library/Logs/com.hllmr.silo`, Linux `~/.local/share/com.hllmr.silo/logs`), and
/// stderr. Release builds have no console, so the file is the only record of what Silo did. It
/// holds full paths; "Copy diagnostics" removes the home folder and user name (`diagnostics`).
/// Never log secrets (tokens, gist contents) through it.
/// Why the log file couldn't be opened this session (`None` when it was), for diagnostics.
struct LogFileError(Option<String>);

fn log_plugin<R: tauri::Runtime>(to_file: bool) -> tauri::plugin::TauriPlugin<R> {
    use tauri_plugin_log::{RotationStrategy, Target, TargetKind, TimezoneStrategy};
    let mut targets = vec![Target::new(TargetKind::Stderr)];
    if to_file {
        let file_name = Some(diagnostics::LOG_NAME.into());
        targets.push(Target::new(match profile::dir() {
            Some(p) => TargetKind::Folder {
                path: p.join("logs"),
                file_name,
            },
            None => TargetKind::LogDir { file_name },
        }));
    }
    tauri_plugin_log::Builder::new()
        .clear_targets()
        .targets(targets)
        .level(log::LevelFilter::Info)
        .level_for("tao", log::LevelFilter::Warn)
        .level_for("wry", log::LevelFilter::Warn)
        .level_for("ureq", log::LevelFilter::Warn)
        .level_for("ureq_proto", log::LevelFilter::Warn)
        .level_for("rustls", log::LevelFilter::Warn)
        .max_file_size(2_000_000)
        // KeepSome(n) keeps n rotated files besides the live one: five files in all.
        .rotation_strategy(RotationStrategy::KeepSome(4))
        .timezone_strategy(TimezoneStrategy::UseLocal)
        .build()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut builder = tauri::Builder::default();
    // A debug build pointed at a demo profile (docs/SHOWCASE.md) panics here on a bad path
    // rather than ever falling back to the real one.
    let profile = profile::dir();
    let mut context = tauri::generate_context!();
    if profile.is_some() {
        // Its own identifier keeps the single-instance lock apart from an installed Silo, and
        // the window is built in `setup` so its webview storage lives in the profile too.
        context.config_mut().identifier = profile::IDENTIFIER.into();
        for w in &mut context.config_mut().app.windows {
            w.create = false;
        }
    }

    // single-instance MUST be registered first. Its `deep-link` feature forwards a silo://
    // URL to the running instance (rather than spawning a second one); the callback fires on
    // that second launch, so we just surface the existing window — the deep-link plugin has
    // already re-emitted the URL to the frontend's onOpenUrl listener.
    #[cfg(desktop)]
    if profile.is_none() {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            use tauri::Manager;
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_focus();
                let _ = w.unminimize();
            }
        }));
    }

    // A profile never checks for app updates: the frontend's check fails quietly instead.
    if profile.is_none() {
        builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    }
    let handler = typed_handler(tauri::generate_handler![
        scan_mods,
        firstrun::first_run_state,
        firstrun::set_first_run_flag,
        get_mod_icon,
        get_curation,
        set_curation,
        get_tags,
        set_tags,
        get_mod_repos,
        set_mod_repo,
        get_mod_sources,
        set_mod_source,
        mark_source_checked,
        guess_repo,
        check_mod_update,
        gh_status,
        gh_set_client_id,
        gh_device_start,
        gh_device_poll,
        gh_device_cancel,
        gh_sign_in_begin,
        gh_set_pat,
        gh_repo_stats,
        gh_star,
        gh_watch,
        gh_logout,
        download_update,
        siloapi_status,
        siloapi_set_base,
        browse_mods,
        siloapi_facets,
        siloapi_stats,
        siloapi_categories,
        siloapi_mod_detail,
        catalog_detail_by_tech,
        catalog_image,
        install_remote_mod,
        catalog_check_updates,
        verify_mod,
        get_overrides,
        set_override,
        plan_organize,
        apply_organize,
        set_active,
        flatten,
        mods_root_status,
        set_mods_root,
        clear_mods_root,
        point_game_at_mods_root,
        set_game_profile,
        create_mods_folder,
        folder_looks_empty,
        pick_folder,
        open_known_folder,
        archive_status,
        archive_move_preflight,
        archive_move_start,
        archive_move_resume,
        archive_move_pause,
        archive_move_cancel,
        archive_use_default,
        archive_repoint,
        archive_adopt_preview,
        archive_adopt_apply,
        get_loadouts,
        save_loadout,
        replace_loadout_mods,
        delete_loadout,
        save_auto_snapshot,
        export_set_file,
        shared_file_kind,
        get_savegames,
        game_files,
        backup_savegame,
        detect_conflicts,
        detect_game,
        launch_game,
        save_text,
        diagnostics_bundle,
        copy_text,
        user_dir_path,
        clear_scan_cache,
        scan_log,
        log_status,
        mark_log_seen,
        scan_bindings,
        mp_export,
        mp_verify_file,
        collection_export,
        collection_update,
        collection_import_preview,
        collection_apply,
        collections_list,
        collection_delete,
        generate_bridge,
        bisect_plan,
        bisect_narrow,
        bisect_snapshot_save,
        bisect_snapshot_get,
        bisect_snapshot_clear,
        get_config,
        set_config,
        mods_with_settings,
        get_mod_settings,
        save_mod_settings,
        save_mod_settings_raw,
        app_version,
        log_frontend_error,
        take_startup_notices,
        secret_storage_secure,
        detect_foreign_files,
        adopt_foreign_file,
        restore_foreign_file,
        relink_projections,
        hardlink_support,
        app_busy,
        force_quit,
        set_self_updating
    ]);

    builder
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_process::init())
        // Used by `copy_text` only; no clipboard permission is granted to the webview.
        .plugin(tauri_plugin_clipboard_manager::init())
        .setup(|app| {
            // Started here, not on the builder: a plugin that fails on the builder stops the app,
            // and a log folder Silo can't write (or an old log another program holds open) must
            // never keep Silo from starting. Then it logs to stderr only.
            let log_error = app.handle().plugin(log_plugin(true)).err().map(|e| {
                let _ = app.handle().plugin(log_plugin(false));
                log::warn!("couldn't open the log file: {e}");
                e.to_string()
            });
            app.manage(LogFileError(log_error));
            // A Disconnect (or a half-saved sign-in) in an earlier run that couldn't clean up
            // storage left this marker: stay signed out until a sign-in succeeds.
            if let Ok(p) = db_path(app.handle()) {
                GH_SESSION.use_marker(p.with_file_name("github-signed-out"));
                let _ = APP_DB.set(p);
            }
            log::info!(
                "Silo {} starting on {}",
                app.package_info().version,
                std::env::consts::OS
            );
            if let Some(p) = profile::dir() {
                log::warn!("running in the demo profile at {}", p.display());
                let stubbed = profile::stubs().as_object().map_or(0, |o| o.len());
                log::warn!(
                    "demo profile: {stubbed} commands answered from {}",
                    profile::STUBS
                );
                build_profile_windows(app, p)?;
            }
            // Resolve and publish the active layout (mods folder, archive, any unfinished move)
            // before the UI asks, so the write guards never act on an unknown layout. Off the
            // main thread: it reads gameSettings.xml and the DB.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn_blocking(move || {
                match db_path(&handle).and_then(|p| db::open(&p)) {
                    Ok(conn) => {
                        // A daily copy of the database to fall back on if it's ever damaged.
                        if let Ok(p) = db_path(&handle) {
                            match db::snapshot(
                                &conn,
                                &db::snapshot_dir(&p),
                                5,
                                std::time::Duration::from_secs(20 * 3600),
                            ) {
                                Ok(Some(s)) => {
                                    log::info!("database snapshot saved: {}", s.display())
                                }
                                Ok(None) => {}
                                Err(e) => log::warn!("database snapshot failed: {e}"),
                            }
                        }
                        // Under the library lock too: a status refresh may already have resolved
                        // it and a write be under way.
                        let _ = refresh_mods_root(&conn);
                        log::info!("library layout resolved at startup");
                    }
                    Err(e) => {
                        log::warn!("couldn't resolve the library layout at startup: {e}")
                    }
                }
                // Icon thumbnails from the old (unstable) cache naming; `icons-v2` replaced it.
                if let Ok(c) = profile::app_cache_dir(&handle) {
                    let _ = std::fs::remove_dir_all(c.join("icons"));
                    // Catalog thumbnails cached before they were checked whole (#357).
                    let _ = std::fs::remove_dir_all(c.join("catalog_images"));
                }
            });
            Ok(())
        })
        // In a demo profile, commands named in its stubs.json get their canned answer instead.
        .invoke_handler(move |invoke| {
            if profile::active() {
                let args = match invoke.message.payload() {
                    tauri::ipc::InvokeBody::Json(v) => v.clone(),
                    _ => serde_json::Value::Null,
                };
                let stub = profile::answer(profile::stubs(), invoke.message.command(), &args);
                if let Some(answer) = stub {
                    match answer {
                        Ok(v) => invoke.resolver.resolve(v),
                        Err(e) => invoke.resolver.reject(e),
                    }
                    return true;
                }
            }
            handler(invoke)
        })
        // Closing the main window while Silo is changing files could leave a mod half-moved:
        // keep it open and let the frontend ask (it calls `force_quit` on "Quit anyway").
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if hold_quit() {
                    api.prevent_close();
                    let _ = window.emit("close-blocked", ());
                }
            }
        })
        .build(context)
        .expect("error while building Silo")
        // Quitting the app another way (the macOS Quit menu, Cmd+Q) is held the same way.
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                if hold_quit() {
                    api.prevent_exit();
                    if app.get_webview_window("main").is_some() {
                        let _ = app.emit("close-blocked", ());
                    } else {
                        // No window left to ask in: quit as soon as the change is done.
                        exit_when_idle(app.clone());
                    }
                }
            }
        });
}

/// Gives `generate_handler!`'s closure its argument type, so it can be wrapped (see `run`).
fn typed_handler<F: Fn(tauri::ipc::Invoke) -> bool>(f: F) -> F {
    f
}

/// The configured windows, built by hand for a demo profile: same settings, but webview storage
/// in the profile and, when asked for, the DevTools protocol on a local port for a capture script.
fn build_profile_windows(app: &tauri::App, profile_dir: &std::path::Path) -> tauri::Result<()> {
    let cdp = profile::cdp_arg(std::env::var(profile::CDP_PORT_ENV).ok().as_deref());
    for cfg in app.config().app.windows.clone() {
        let mut w = tauri::WebviewWindowBuilder::from_config(app, &cfg)?
            .data_directory(profile::webview_dir(profile_dir));
        if let Some(arg) = &cdp {
            // Giving WebView2 any arguments replaces its defaults, so keep wry's.
            w = w.additional_browser_args(&format!(
                "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection {arg}"
            ));
        }
        w.build()?;
    }
    Ok(())
}

/// Set while a thread waits to quit once Silo stops changing files (one at a time).
static EXIT_PENDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Quit once `library_busy()` clears, from a thread of its own.
fn exit_when_idle(app: tauri::AppHandle) {
    if EXIT_PENDING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        while library_busy() {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        FORCE_QUIT.store(true, std::sync::atomic::Ordering::SeqCst);
        app.exit(0);
    });
}

/// Hold a quit: Silo is changing files and the user hasn't chosen "Quit anyway".
fn hold_quit() -> bool {
    !FORCE_QUIT.load(std::sync::atomic::Ordering::SeqCst) && library_busy()
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_backfill_tells_the_window_each_linked_mod_once() {
        use crate::aliasbackfill::{LibraryZip, Outcome};
        let zip = |p: &str, t: &str| LibraryZip {
            path: PathBuf::from(p),
            tech_name: t.into(),
            size: 1,
            mtime_ms: 1,
        };
        let zips = [
            zip("a/FS25_X.zip", "FS25_X"),
            zip("b/FS25_X.zip", "FS25_X"),
            zip("a/FS25_Y.zip", "FS25_Y"),
        ];
        let out = [
            (PathBuf::from("a/FS25_X.zip"), Outcome::Linked("c".into())),
            (PathBuf::from("b/FS25_X.zip"), Outcome::Linked("c".into())),
            (
                PathBuf::from("a/FS25_Y.zip"),
                Outcome::Retry("later".into()),
            ),
        ];
        assert_eq!(super::linked_tech_names(&zips, &out), ["FS25_X"]);
        assert!(super::linked_tech_names(&zips, &out[2..]).is_empty());
    }

    #[test]
    fn an_update_check_hands_the_backfill_the_file_the_scan_read() {
        let m = |size, mtime_ms| super::LocalMod {
            tech_name: "FS25_FieldToDoList".into(),
            version: Some("1.0".into()),
            path: Some("C:/mods/FS25_FieldToDoList.zip".into()),
            size,
            mtime_ms,
        };
        let zips = super::local_zips(&[m(Some(9), Some(77)), m(None, Some(77)), m(Some(9), None)]);
        // Only a mod with the scan's size and time is taken, keyed on them (never on what the
        // file has now): a file changed since the scan waits for the next scan's look.
        assert_eq!(zips.len(), 1);
        assert_eq!((zips[0].size, zips[0].mtime_ms), (9, 77));
        let none = super::LocalMod {
            path: None,
            ..m(Some(9), Some(77))
        };
        assert!(super::local_zips(&[none]).is_empty());
    }

    use super::{
        archive_holds_mods, build_gh_scope, cancel_falls_back, fence_problems,
        foreign_mods_problem, move_outcome, move_outcome_level, move_problems, overlap_problems,
        repoint_refusal, scan_archives, scope_grants, select_adopt, split_adopt, Fences, MoveFacts,
        Space,
    };
    use crate::{
        organize::{Layout, ModInput},
        relocate::MoveMarker,
        siloapi,
    };
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    /// An unreachable catalog fails the preview with its cause, instead of listing every mod
    /// as "not in the catalog".
    #[test]
    fn a_catalog_outage_is_an_error_not_an_empty_answer() {
        let coll =
            crate::collection::parse_set_file(r#"{ "silo": 1, "name": "W", "mods": ["FS25_A"] }"#)
                .unwrap();
        // Nothing listens on port 9 of loopback: the lookup fails at once.
        let err = super::resolve_collection("http://127.0.0.1:9", &coll, "{}").unwrap_err();
        assert!(err.contains("couldn't look up this set's mods"), "{err}");
        assert_eq!(
            crate::error::classify(&err),
            crate::error::code::NET_OFFLINE,
            "{err}"
        );
    }

    #[test]
    fn raw_settings_must_be_one_well_formed_document() {
        for good in [
            r#"<?xml version="1.0" encoding="utf-8"?><settings a="1"><x>2</x></settings>"#,
            "<!-- c --><settings/>\n",
            "  <settings>\n  <x>a &amp; b</x>\n</settings>  ",
            r#"<settings><a v='say "hi"' w="&lt;&#10;&#x9;&quot;"/>&apos;&#233;</settings>"#,
            "\u{feff}<?xml version=\"1.0\" encoding=\"utf-8\" standalone=\"no\"?>\n<!DOCTYPE settings>\n<settings xsi:x=\"1\"><x-y.z/></settings>\n",
            "<settings><?app-hint keep this?><!-- a - b --></settings>",
            "<settings><v>hello]]&gt;</v><v><![CDATA[hello]]></v><v>a]]b ]> ]]</v></settings>",
        ] {
            assert!(super::ensure_well_formed(good).is_ok(), "{good}");
        }
        for bad in [
            "<settings><x>1</settings>",
            "<settings><x>1</x>",
            "<settings></settings><other/>",
            "",
            "just text",
            "<settings>a & b</settings>",
            "<settings/>trailing",
            // #354: unquoted or duplicated attributes, and entities nothing declares.
            "<settings><a value=5/></settings>",
            r#"<settings><a value="5" value="6"/></settings>"#,
            r#"<settings value="5" value="6"></settings>"#,
            "<settings>&missing;</settings>",
            r#"<settings><a v="&missing;"/></settings>"#,
            r#"<settings><a v="a & b"/></settings>"#,
            r#"<settings><a v="<"/></settings>"#,
            "<settings>&#1;</settings>",
            r#"<settings><a v="&#0;"/></settings>"#,
            "<settings value=\"\u{1}\"/>",
            "<settings>a\u{1}b</settings>",
            "<settings><![CDATA[\u{FFFF}]]></settings>",
            r#"<settings 1value="5"/>"#,
            r#"<1settings/>"#,
            r#"<settings><?xml version="1.0"?></settings>"#,
            r#"<settings/><?xml version="1.0"?>"#,
            "<!-- c --><?xml version=\"1.0\"?><settings/>",
            "<settings><!DOCTYPE settings></settings>",
            "<!DOCTYPE a><!DOCTYPE b><settings/>",
            "<?xml version=1.0?><settings/>",
            r#"<?xml version="1.0" version="1.0"?><settings/>"#,
            "<settings><!-- a--b --></settings>",
            "<settings><!-- a---></settings>",
            "<settings><?1bad data?></settings>",
            "<settings><?XML data?></settings>",
            "<settings><value>hello]]></value></settings>",
            "<!DOCTYPE settings [garbage]>\n<settings/>",
            "<!DOCTYPE settings SYSTEM \"x.dtd\"><settings/>",
        ] {
            let Err(e) = super::ensure_well_formed(bad) else {
                panic!("accepted {bad:?}");
            };
            assert!(e.contains("didn't save it"), "{bad}: {e}");
        }
    }

    #[test]
    fn a_blank_version_is_not_looked_up() {
        assert_eq!(super::verifiable_version(Some("1.2.0.0")), Some("1.2.0.0"));
        assert_eq!(super::verifiable_version(Some(" v1 ")), Some("v1"));
        assert_eq!(super::verifiable_version(Some("  ")), None);
        assert_eq!(super::verifiable_version(Some("")), None);
        assert_eq!(super::verifiable_version(None), None);
    }

    #[test]
    fn a_shared_set_file_is_read_only_from_a_plain_silo_or_json_path() {
        let dir = std::env::temp_dir().join(format!("silo_set_source_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let good = dir.join("weekend.silo");
        std::fs::write(
            &good,
            r#"{ "silo": 1, "name": "Weekend", "mods": ["FS25_A"] }"#,
        )
        .unwrap();
        let (c, json) =
            super::read_set_source("ignored", Some(good.to_str().unwrap()), None).unwrap();
        assert_eq!(c.name, "Weekend");
        assert!(
            json.contains("FS25_A"),
            "the fingerprint covers the file's text"
        );
        let txt = dir.join("weekend.txt");
        std::fs::write(&txt, "{}").unwrap();
        assert!(super::read_set_source("", Some(txt.to_str().unwrap()), None).is_err());
        let up = format!(
            "{}/../{}/weekend.silo",
            dir.display(),
            dir.file_name().unwrap().to_string_lossy()
        );
        assert!(
            super::read_set_source("", Some(&up), None).is_err(),
            "no .. segments"
        );
        let missing = dir.join("missing.silo");
        assert!(super::read_set_source("", Some(missing.to_str().unwrap()), None).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn fences() -> Fences {
        Fences {
            mods: PathBuf::from("/home/me/fs25/mods"),
            current_archive: PathBuf::from("/hdd/arch"),
            other_mods: vec![
                PathBuf::from("/other/mods"),
                PathBuf::from("/home/me/fs25/mods2"),
            ],
            other_archives: vec![PathBuf::from("/other/mods/archive"), PathBuf::from("/x/a2")],
            user_dir: Some(PathBuf::from("/home/me/fs25")),
            profile_dirs: vec![PathBuf::from("/f/prof")],
            app_data: Some(PathBuf::from("/home/me/.silo")),
        }
    }

    fn has(v: &[String], needle: &str) -> bool {
        v.iter().any(|p| p.contains(needle))
    }

    #[test]
    fn the_mods_folder_changes_only_to_a_picked_known_or_default_folder() {
        let a = std::path::Path::new("/x/mods-a");
        let known = vec![PathBuf::from("/x/mods-known")];
        let d = std::path::Path::new("/x/default");
        let default = Some(d);
        assert!(super::mods_root_choice_allowed(a, true, &known, default));
        assert!(super::mods_root_choice_allowed(
            &known[0], false, &known, default
        ));
        assert!(super::mods_root_choice_allowed(d, false, &known, default));
        assert!(!super::mods_root_choice_allowed(a, false, &known, default));
    }

    #[test]
    fn overlap_allows_a_clear_folder() {
        assert!(overlap_problems(Path::new("/ssd/new-archive"), &fences()).is_empty());
        // The default archive inside the FS25 user dir is fine (only the dir itself, or a
        // folder containing it, is refused).
        assert!(overlap_problems(Path::new("/home/me/fs25/mods/archive"), &fences()).is_empty());
    }

    #[test]
    fn overlap_refuses_an_fs25_profile_folder_like_the_main_user_folder() {
        // #387: a profile folder, a folder inside it, or one holding it is refused...
        let f = fences();
        for p in ["/f/prof", "/f/prof/archive", "/f"] {
            assert!(
                has(&overlap_problems(Path::new(p), &f), "profile folders"),
                "{p}"
            );
        }
        // ...but not the default archive inside a mods folder, nor a folder beside it.
        let mut in_mods = fences();
        in_mods.other_mods.push(PathBuf::from("/f/prof/mods"));
        assert!(!has(
            &overlap_problems(Path::new("/f/prof/mods/archive"), &in_mods),
            "profile folders"
        ));
        assert!(overlap_problems(Path::new("/f/profile-archive"), &f).is_empty());
    }

    #[test]
    fn fences_cover_the_profile_folders_of_profile_mods_folders() {
        let base = temp_base("gameprofile_fences");
        let prof = base.join("prof");
        let pmods = prof.join("mods");
        std::fs::create_dir_all(&pmods).unwrap();
        std::fs::write(prof.join("gameSettings.xml"), "<gameSettings/>").unwrap();
        // A plain folder named `mods`, never a profile: not fenced as one.
        let plain = base.join("plain").join("mods");
        std::fs::create_dir_all(&plain).unwrap();
        let conn = crate::db::open(&base.join("silo.db")).unwrap();
        let known = crate::modsroot::add_known(None, &pmods.to_string_lossy());
        let known = crate::modsroot::add_known(Some(&known), &plain.to_string_lossy());
        crate::db::set_app_setting(&conn, crate::modsroot::KNOWN_KEY, Some(&known)).unwrap();
        // Another folder is the active one; the profile is only a known folder.
        let mods = base.join("mods");
        let layout = Layout::default_for(&mods);
        let f = super::fences_with(&conn, &layout, &[], None, None).unwrap();
        assert_eq!(f.profile_dirs, vec![super::key_path(&prof)]);
        // An archive inside the profile folder is refused; beside it, fine.
        assert!(has(
            &fence_problems(&layout, &prof.join("library"), &f),
            "profile folders"
        ));
        assert!(!has(
            &fence_problems(&layout, &base.join("library"), &f),
            "profile folders"
        ));
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn overlap_refuses_a_drive_root_alone() {
        assert_eq!(
            overlap_problems(Path::new("/"), &fences()),
            vec!["Pick a folder, not a whole drive.".to_string()]
        );
    }

    #[test]
    fn overlap_refuses_nesting_with_the_current_archive() {
        let f = fences();
        assert!(has(
            &overlap_problems(Path::new("/hdd/arch/sub"), &f),
            "current archive"
        ));
        assert!(has(
            &overlap_problems(Path::new("/hdd"), &f),
            "current archive"
        ));
        // Equality is path_problems' message, not repeated here.
        assert!(overlap_problems(Path::new("/hdd/arch"), &f).is_empty());
    }

    #[test]
    fn overlap_refuses_other_mods_folders_equal_inside_or_containing() {
        let f = fences();
        for p in ["/other/mods", "/other/mods/sub", "/other"] {
            assert!(
                has(
                    &overlap_problems(Path::new(p), &f),
                    "another mods folder Silo knows"
                ),
                "{p}"
            );
        }
    }

    #[test]
    fn overlap_refuses_nesting_with_other_archives() {
        let f = fences();
        assert!(has(
            &overlap_problems(Path::new("/x/a2/deeper"), &f),
            "archive"
        ));
        assert!(has(&overlap_problems(Path::new("/x"), &f), "archive"));
        // Equal: path_problems' "Another mods folder already uses that archive".
        assert!(!has(&overlap_problems(Path::new("/x/a2"), &f), "archive"));
    }

    #[test]
    fn overlap_refuses_the_user_dir_and_app_data() {
        let f = fences();
        assert!(has(
            &overlap_problems(Path::new("/home/me/fs25"), &f),
            "Farming Simulator 25"
        ));
        assert!(has(
            &overlap_problems(Path::new("/home/me"), &f),
            "Farming Simulator 25"
        ));
        assert!(has(
            &overlap_problems(Path::new("/home/me/.silo"), &f),
            "Silo's own data"
        ));
        assert!(has(
            &overlap_problems(Path::new("/home/me/.silo/a"), &f),
            "Silo's own data"
        ));
    }

    #[test]
    fn overlap_refuses_inside_the_user_dir_except_inside_a_mods_folder() {
        let f = fences();
        for p in ["/home/me/fs25/savegame1", "/home/me/fs25/a/b"] {
            assert!(
                has(&overlap_problems(Path::new(p), &f), "Farming Simulator 25"),
                "{p}"
            );
        }
        // Inside the active mods folder: the default archive is fine (path_problems judges
        // anything else there), and the user-dir message isn't added.
        assert!(overlap_problems(Path::new("/home/me/fs25/mods/archive"), &f).is_empty());
        assert!(!has(
            &overlap_problems(Path::new("/home/me/fs25/mods/other"), &f),
            "Farming Simulator 25"
        ));
        // Inside another known mods folder: refused for that, not repeated as the user dir.
        let p = overlap_problems(Path::new("/home/me/fs25/mods2/sub"), &f);
        assert!(has(&p, "another mods folder Silo knows"));
        assert!(!has(&p, "Farming Simulator 25"));
    }

    #[test]
    fn a_mods_folder_clear_of_every_archive_is_fine() {
        let archives = vec![
            PathBuf::from("/hdd/lib"),
            PathBuf::from("/other/mods/archive"),
        ];
        assert_eq!(
            super::mods_folder_problem(
                Path::new("/ssd/mods"),
                Path::new("/ssd/mods/archive"),
                &archives,
                &[PathBuf::from("/other/mods")]
            ),
            None
        );
        // A sibling that only shares a name prefix isn't inside.
        assert_eq!(
            super::mods_folder_problem(
                Path::new("/hdd/lib2"),
                Path::new("/hdd/lib2/archive"),
                &archives,
                &[]
            ),
            None
        );
    }

    #[test]
    fn a_mods_folder_that_is_in_or_holds_another_archive_is_refused() {
        let archives = vec![PathBuf::from("/hdd/lib")];
        // The archive itself, a category inside it, or a folder holding it.
        for p in ["/hdd/lib", "/hdd/lib/Tools", "/hdd"] {
            let own = PathBuf::from(p).join("archive");
            let why = super::mods_folder_problem(Path::new(p), &own, &archives, &[]);
            assert_eq!(
                why,
                Some(super::FolderProblem {
                    message:
                        "That folder overlaps another mods folder's archive. Pick a folder outside it."
                            .into(),
                    conflict: PathBuf::from("/hdd/lib"),
                }),
                "{p}"
            );
        }
    }

    #[test]
    fn a_mods_folder_whose_own_archive_overlaps_another_mods_folder_is_refused() {
        let other_mods = vec![PathBuf::from("/hdd/m2")];
        let want =
            Some("That folder's archive would overlap another mods folder. Pick another folder.");
        // Its chosen archive inside another mods folder, or holding one.
        for own in ["/hdd/m2/lib", "/hdd/m2", "/hdd"] {
            let why = super::mods_folder_problem(
                Path::new("/ssd/mods"),
                Path::new(own),
                &[],
                &other_mods,
            );
            assert_eq!(why.map(|w| w.message).as_deref(), want, "{own}");
        }
        assert_eq!(
            super::mods_folder_problem(
                Path::new("/ssd/mods"),
                Path::new("/ssd/mods/archive"),
                &[],
                &other_mods
            ),
            None
        );
    }

    #[test]
    fn the_mods_folder_check_reads_the_other_layouts_from_the_database() {
        let base = temp_base("modscheck");
        let mine = base.join("mine");
        let other = base.join("other");
        for d in [&mine, &other] {
            std::fs::create_dir_all(d).unwrap();
        }
        let conn = crate::db::open(&base.join("silo.db")).unwrap();
        let known = vec![mine.clone(), other.clone()];
        assert_eq!(super::mods_folder_check(&conn, &mine, &known), Ok(None));
        // Its own archive chosen inside the other mods folder.
        crate::db::set_app_setting(
            &conn,
            &crate::relocate::archive_setting_key(&crate::modsroot::canonical_key(&mine)),
            Some(&other.join("lib").to_string_lossy()),
        )
        .unwrap();
        let p = super::mods_folder_check(&conn, &mine, &known)
            .unwrap()
            .unwrap();
        assert!(p.message.contains("overlap another mods folder"));
        // Named as Silo knows it, not as an identity key.
        assert_eq!(p.conflict, other);
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn switching_back_to_a_known_folder_with_a_custom_archive_still_works() {
        let base = temp_base("switchback");
        let (a, b, lib) = (base.join("a"), base.join("b"), base.join("lib-a"));
        for d in [&a, &b, &lib.join("Tools")] {
            std::fs::create_dir_all(d).unwrap();
        }
        let conn = crate::db::open(&base.join("silo.db")).unwrap();
        let default = base.join("mods");
        let s = |p: &Path| p.to_string_lossy().into_owned();
        let choose = |p: &Path, picked: bool| {
            super::apply_mods_root_choice(&conn, &s(p), picked, Some(&default), &[])
        };
        let choice = || crate::db::get_app_setting(&conn, crate::modsroot::CHOICE_KEY);
        choose(&a, true).unwrap();
        // A's archive moved to a folder of its own (what a finished move stores).
        crate::db::set_app_setting(
            &conn,
            &crate::relocate::archive_setting_key(&crate::modsroot::canonical_key(&a)),
            Some(&s(&lib)),
        )
        .unwrap();
        choose(&b, true).unwrap();
        assert_eq!(choice(), Some(s(&b)));
        // Back to A from the known list (not picked again): its own archive is no obstacle.
        choose(&a, false).unwrap();
        assert_eq!(choice(), Some(s(&a)));
        // A category folder inside A's archive is refused, and nothing changes.
        assert_eq!(
            choose(&lib.join("Tools"), true),
            Err(super::OVERLAPS_AN_ARCHIVE.to_string())
        );
        assert_eq!(choice(), Some(s(&a)));
        // An unknown folder that wasn't picked is refused.
        assert_eq!(
            choose(&base.join("c"), false),
            Err(super::picked::NOT_PICKED.to_string())
        );
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A database Silo can't read refuses the switch in plain words, never lets it through.
    #[test]
    fn a_mods_folder_choice_is_refused_when_the_check_cant_read() {
        let base = temp_base("choiceunreadable");
        let a = base.join("a");
        std::fs::create_dir_all(&a).unwrap();
        let conn = crate::db::open(&base.join("silo.db")).unwrap();
        conn.execute_batch("DROP TABLE app_setting").unwrap();
        let e = super::apply_mods_root_choice(&conn, &a.to_string_lossy(), true, None, &[])
            .unwrap_err();
        assert_eq!(
            e,
            "Silo couldn't check this folder against your other mods folders. Try again."
        );
        assert_eq!(
            crate::db::get_app_setting(&conn, crate::modsroot::CHOICE_KEY),
            None,
            "nothing stored"
        );
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_folder_fs25_reads_counts_as_a_known_mods_folder() {
        let base = temp_base("overridepick");
        let o = base.join("override");
        std::fs::create_dir_all(o.join("archive/Maps")).unwrap();
        let conn = crate::db::open(&base.join("silo.db")).unwrap();
        // O is only FS25's override (never picked, so not in the known list).
        let maps = o.join("archive/Maps");
        assert_eq!(
            super::apply_mods_root_choice(
                &conn,
                &maps.to_string_lossy(),
                true,
                None,
                std::slice::from_ref(&o)
            ),
            Err(super::OVERLAPS_AN_ARCHIVE.to_string())
        );
        assert_eq!(
            crate::db::get_app_setting(&conn, crate::modsroot::CHOICE_KEY),
            None
        );
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn an_override_into_another_layouts_archive_is_reported_and_blocks_writes() {
        let base = temp_base("overrideblock");
        let mods = base.join("mods");
        let lib = base.join("lib");
        std::fs::create_dir_all(lib.join("Maps")).unwrap();
        let conn = crate::db::open(&base.join("silo.db")).unwrap();
        crate::db::set_app_setting(
            &conn,
            &crate::relocate::archive_setting_key(&crate::modsroot::canonical_key(&mods)),
            Some(&lib.to_string_lossy()),
        )
        .unwrap();
        // FS25 reads a category folder of that archive.
        let b = super::resolve_layout(&conn, &lib.join("Maps"), Some(&mods), None, None, false)
            .unwrap_err();
        assert_eq!(b.reason, super::OVERLAPS_AN_ARCHIVE);
        assert_eq!(b.cause, crate::modsroot::BlockCause::Overlap);
        assert_eq!(b.conflict, Some(lib.clone()));
        let why = b.reason;
        // Published as "no writable layout, because…": nothing is a write root, and organize
        // and the other library writes are refused with the reason.
        assert!(super::trusted_roots(None, true, true).is_empty());
        assert!(super::trusted_roots(None, true, false).is_empty());
        assert_eq!(
            super::blocked_first(Some(why.clone()), || Ok::<_, String>(())),
            Err(why.clone())
        );
        assert_eq!(super::blocked_first(None, || Ok::<_, String>(1)), Ok(1));
        assert_eq!(
            super::archive_availability(None, true, Some(&why)),
            Err(why)
        );
        // The default layout of a clear folder resolves.
        let clear = base.join("clear");
        std::fs::create_dir_all(&clear).unwrap();
        let a = super::resolve_layout(&conn, &clear, Some(&mods), None, None, false).unwrap();
        assert_eq!(a.layout, Layout::default_for(&clear));
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn when_only_fs25s_setting_clashes_pointing_fs25_here_clears_it() {
        let base = temp_base("gameclash");
        let o = base.join("override");
        let chosen = o.join("archive/Maps");
        std::fs::create_dir_all(&chosen).unwrap();
        let conn = crate::db::open(&base.join("silo.db")).unwrap();
        // The user chose a folder inside O's archive; FS25's setting points at O.
        let b = super::resolve_layout(&conn, &chosen, None, Some(&o), None, true).unwrap_err();
        assert_eq!(b.cause, crate::modsroot::BlockCause::GameSetting);
        assert!(
            b.reason.starts_with(&format!(
                "Farming Simulator 25's mods folder setting points at {}",
                o.display()
            )),
            "{}",
            b.reason
        );
        // "Point FS25 here" targets the chosen folder, though nothing is published.
        assert_eq!(super::game_point_target(None, Some(&b)), Ok(chosen.clone()));
        // Once FS25 reads the chosen folder, it resolves.
        assert!(super::resolve_layout(&conn, &chosen, None, Some(&chosen), None, true).is_ok());
        // Not the user's choice (FS25's override itself picked it): a plain overlap, and
        // "Point FS25 here" has nothing to point at.
        let b = super::resolve_layout(&conn, &chosen, None, Some(&o), None, false).unwrap_err();
        assert_eq!(b.cause, crate::modsroot::BlockCause::Overlap);
        assert_eq!(
            super::game_point_target(None, Some(&b)),
            Err(b.reason.clone())
        );
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_folder_silo_never_managed_counts_only_as_itself() {
        let base = temp_base("unmanaged");
        let (me, o) = (base.join("me"), base.join("override"));
        for d in [&me, &o] {
            std::fs::create_dir_all(d).unwrap();
        }
        let conn = crate::db::open(&base.join("silo.db")).unwrap();
        let known = vec![o.clone()];
        let archives = || super::other_archives(&conn, &me, &known).unwrap();
        // No stored archive, no organized rows, no archive folder: no archive to fence.
        assert!(archives().is_empty());
        // Its archive folder exists: it counts.
        std::fs::create_dir_all(o.join("archive")).unwrap();
        assert_eq!(archives(), vec![o.join("archive")]);
        // Or Silo has organized it (the archive folder may be gone).
        std::fs::remove_dir(o.join("archive")).unwrap();
        let row = crate::db::OrganizedRow {
            tech_name: "FS25_A".into(),
            file_name: "FS25_A.zip".into(),
            kind: "zip".into(),
            category: "Tools".into(),
            subcategory: None,
            active: false,
        };
        crate::db::upsert_organized(&conn, &crate::modsroot::canonical_key(&o), &row).unwrap();
        assert_eq!(archives(), vec![o.join("archive")]);
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_blocked_folders_own_move_can_still_be_resumed_or_cancelled() {
        let base = temp_base("blockedmove");
        let mods = base.join("mods");
        let hdd = base.join("hdd");
        std::fs::create_dir_all(&hdd).unwrap();
        let conn = crate::db::open(&base.join("silo.db")).unwrap();
        assert_eq!(super::unfinished_move(&conn, &mods), None);
        let m = MoveMarker::new(mods.join("archive"), hdd.clone());
        crate::relocate::save_marker(&conn, &crate::modsroot::canonical_key(&mods), &m).unwrap();
        let moving = super::unfinished_move(&conn, &mods).unwrap();
        assert_eq!(moving.layout, Layout::default_for(&mods));
        assert_eq!(moving.move_to, Some(hdd));
        let blocked = |moving| crate::modsroot::Blocked {
            reason: "blocked".into(),
            cause: crate::modsroot::BlockCause::Overlap,
            mods: mods.clone(),
            conflict: None,
            moving,
        };
        // Resume / cancel run in the blocked folder's move; without one, the reason.
        assert_eq!(
            super::resumable_layout(None, Some(blocked(Some(Box::new(moving.clone())))), true),
            Ok(Layout::default_for(&mods))
        );
        assert_eq!(
            super::resumable_layout(None, Some(blocked(None)), true),
            Err("blocked".to_string())
        );
        // Everything else waits for that move, as for any unfinished move.
        assert_eq!(
            super::no_move_refusal(None, Some(&blocked(Some(Box::new(moving)))), true),
            Err("Finish or cancel the archive move first.".to_string())
        );
        assert_eq!(
            super::no_move_refusal(None, Some(&blocked(None)), true),
            Ok(())
        );
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn once_resolved_no_layout_means_no_write_roots_at_all() {
        // An unavailable chosen folder (not blocked) never falls back to FS25's default.
        assert!(super::trusted_roots(None, true, true).is_empty());
        assert!(super::trusted_roots(None, true, false).is_empty());
        // Before anything is resolved, the auto-detected default is all there is.
        assert_eq!(
            super::trusted_roots(None, false, false),
            crate::fsgame::default_mods_paths()
        );
        let a = crate::modsroot::Active {
            layout: Layout::default_for(Path::new("/m")),
            move_to: Some(PathBuf::from("/hdd")),
        };
        assert_eq!(
            super::trusted_roots(Some(a.clone()), true, true),
            vec![
                PathBuf::from("/m"),
                PathBuf::from("/m").join("archive"),
                PathBuf::from("/hdd")
            ]
        );
        assert_eq!(
            super::trusted_roots(Some(a), true, false),
            vec![PathBuf::from("/m")]
        );
    }

    #[test]
    fn a_refresh_publishes_only_under_the_lock_or_before_the_first_resolution() {
        assert!(super::may_publish(true, true));
        assert!(super::may_publish(true, false));
        // Nothing can write before the first resolution, so startup always publishes.
        assert!(super::may_publish(false, false));
        // A write is under way: describe only, don't move the trusted roots under it.
        assert!(!super::may_publish(false, true));
    }

    #[test]
    fn the_trusted_folder_and_its_layout_come_from_one_snapshot() {
        let base = temp_base("one_snapshot");
        let a = base.join("mods");
        let b = base.join("other");
        let custom = base.join("hdd");
        for d in [&b, &custom] {
            std::fs::create_dir_all(d).unwrap();
        }
        let active = |mods: &Path, archive: &Path| crate::modsroot::Active {
            layout: Layout::new(mods.to_path_buf(), archive.to_path_buf()).unwrap(),
            move_to: None,
        };
        let on_a = active(&a, &custom);
        // The folder the snapshot trusts, with its own custom archive.
        let mods = super::root_in(Some(on_a.clone()), None, true, None).unwrap();
        assert_eq!(mods, a);
        assert_eq!(super::layout_for(Some(on_a.clone()), &mods).archive, custom);
        let named = Some(a.to_string_lossy().into_owned());
        assert_eq!(
            super::root_in(Some(on_a.clone()), None, true, named.clone()),
            Ok(a.clone())
        );
        // A snapshot taken after a switch to B refuses A outright...
        let on_b = active(&b, &b.join("archive"));
        assert!(super::root_in(Some(on_b.clone()), None, true, named).is_err());
        // ...and never lends A's archive to another folder (or B's to A).
        assert_eq!(super::layout_for(Some(on_b), &a), Layout::default_for(&a));
        // A blocked folder answers with its reason.
        let blocked = crate::modsroot::Blocked {
            reason: "shares files".into(),
            cause: crate::modsroot::BlockCause::Overlap,
            mods: a.clone(),
            conflict: None,
            moving: None,
        };
        assert_eq!(
            super::root_in(None, Some(blocked), true, None),
            Err("shares files".to_string())
        );
        assert!(super::root_in(None, None, true, None).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A small real image encoded as `format`.
    fn image_bytes(format: image::ImageFormat) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(8, 8, image::Rgb([200, 30, 30]));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, format).unwrap();
        out.into_inner()
    }

    #[test]
    fn only_a_real_image_data_url_is_a_usable_cached_thumbnail() {
        use base64::Engine;
        let enc = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
        let png = image_bytes(image::ImageFormat::Png);
        let jpeg = image_bytes(image::ImageFormat::Jpeg);
        let b64 = enc(&png);
        assert!(super::cached_image_ok(&format!(
            "data:image/png;base64,{b64}"
        )));
        assert!(super::cached_image_ok(&format!(
            "data:image/jpeg;base64,{}",
            enc(&jpeg)
        )));
        // GIF, WebP and the rest can't be told whole from a cut-off copy: never cached.
        let mut gif = b"GIF89a".to_vec();
        gif.extend([0u8; 20]);
        gif.push(0x3B);
        let mut webp = b"RIFF".to_vec();
        webp.extend(24u32.to_le_bytes());
        webp.extend(b"WEBP");
        webp.extend([0u8; 20]);
        for other in [&gif, &webp] {
            assert_eq!(super::whole_image_format(other), None);
        }
        // A PNG signature straight followed by its end chunk has no header: not an image.
        let hollow = [&png[..8], &png[png.len() - 12..]].concat();
        assert_eq!(super::whole_image_format(&hollow), None);
        for bad in [
            String::new(),
            "data:image/png;base64,".into(),
            // Cut off mid-write: not valid base64 any more.
            format!("data:image/png;base64,{}", &b64[..b64.len() - 3]),
            // Cut off on a four-character boundary: valid base64 of part of the file.
            format!("data:image/png;base64,{}", &b64[..b64.len() / 8 * 4]),
            format!("data:image/jpeg;base64,{}", enc(&jpeg[..jpeg.len() - 30])),
            // A JPEG cut just after an embedded thumbnail's end marker.
            format!(
                "data:image/jpeg;base64,{}",
                enc(&[&jpeg[..20], &[0xFF, 0xD9][..]].concat())
            ),
            // Text labelled as an image.
            format!(
                "data:image/png;base64,{}",
                enc(b"<html>not found, but long enough</html>")
            ),
            // Too small to be an image.
            "data:image/png;base64,AAAA".into(),
            format!("data:text/html;base64,{b64}"),
            format!("data:image/png,{b64}"),
        ] {
            assert!(!super::cached_image_ok(&bad), "{bad}");
        }
    }

    #[test]
    fn a_damaged_fetched_image_is_shown_but_never_cached() {
        use base64::Engine;
        let mut png = image_bytes(image::ImageFormat::Png);
        let idat = png.windows(4).position(|w| w == b"IDAT").unwrap();
        png[idat + 6] ^= 0xFF;
        let damaged = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&png)
        );
        // Whole as far as a cache read can tell (header and end intact)...
        assert!(super::cached_image_ok(&damaged));
        // ...but it doesn't decode, so it isn't kept.
        assert!(!super::cacheable_image(&damaged));
        let base = temp_base("thumbs_damaged");
        let file = base.join("catalog_images").join("abc");
        super::cache_image(&file, &damaged);
        assert!(!file.exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn thumbnails_are_cached_whole_and_a_broken_entry_is_replaced() {
        use base64::Engine;
        let base = temp_base("thumbs");
        let dir = base.join("catalog_images");
        let file = dir.join("abc");
        let good = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(image_bytes(image::ImageFormat::Png))
        );
        // Nothing cached yet; an empty fetch isn't cached at all.
        assert_eq!(super::read_cached_image(&file), None);
        super::cache_image(&file, "data:image/jpeg;base64,");
        assert!(!file.exists());
        super::cache_image(&file, &good);
        assert_eq!(super::read_cached_image(&file), Some(good.clone()));
        // Replacing an entry works too, and no temp file is left behind.
        super::cache_image(&file, &good);
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["abc"]);
        // A broken entry (an older Silo's cut-off write) is a miss, left for the refetch to
        // replace (never deleted on read: that could delete another request's good copy).
        std::fs::write(&file, "data:image/jpeg;base64,AAA").unwrap();
        assert_eq!(super::read_cached_image(&file), None);
        super::cache_image(&file, &good);
        assert_eq!(super::read_cached_image(&file), Some(good.clone()));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A zip mod with a minimal modDesc at `path`.
    fn mod_zip(path: &Path) {
        use std::io::Write;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut zw = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zw.start_file("modDesc.xml", opts).unwrap();
        zw.write_all(b"<modDesc descVersion=\"100\"><title><en>A</en></title></modDesc>")
            .unwrap();
        zw.finish().unwrap();
    }

    #[test]
    fn a_scan_of_a_blocked_folder_writes_no_organized_rows() {
        static L: std::sync::Mutex<()> = std::sync::Mutex::new(());
        static E: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let base = temp_base("scanblocked");
        let mods = base.join("mods");
        mod_zip(&mods.join("archive/Tools/FS25_A.zip"));
        let key = crate::modsroot::canonical_key(&mods);
        let mut conn = crate::db::open(&base.join("silo.db")).unwrap();
        let organized = |c: &rusqlite::Connection| crate::db::load_organized(c, &key);
        let scan = |conn: &mut rusqlite::Connection, plan: &super::ScanPlan| {
            let start = super::quiet_epoch(&L, &E);
            super::scan_library(conn, plan, None, &L, &E, start, |_, _| {})
        };

        // Blocked: no trusted roots, so the folder the webview names is neither read nor
        // reconciled.
        let plan = super::scan_plan(vec![mods.clone()], &[], None);
        assert_eq!(plan, super::ScanPlan::default());
        scan(&mut conn, &plan);
        assert!(organized(&conn).is_empty());

        // A trusted root that isn't the active layout's folder is read, never reconciled.
        let other = crate::modsroot::Active {
            layout: Layout::default_for(&base.join("other")),
            move_to: None,
        };
        let plan = super::scan_plan(
            vec![mods.clone()],
            std::slice::from_ref(&mods),
            Some(&other),
        );
        assert_eq!(plan.reconcile_key, None);
        let found = scan(&mut conn, &plan);
        assert_eq!(found.mods.len(), 1);
        assert!(organized(&conn).is_empty());

        // Control: the active folder's manifest follows its archive.
        let active = crate::modsroot::Active {
            layout: Layout::default_for(&mods),
            move_to: None,
        };
        let plan = super::scan_plan(Vec::new(), std::slice::from_ref(&mods), Some(&active));
        assert_eq!(plan.reconcile_key.as_deref(), Some(key.as_str()));
        scan(&mut conn, &plan);
        assert_eq!(organized(&conn).len(), 1);
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// #353: a different build swapped in at an archived mod's name is listed as the file the
    /// game loads, while the manifest keeps naming the archived copy (category, file, kind).
    #[test]
    fn a_swapped_in_zip_is_listed_but_the_manifest_keeps_the_archive() {
        static L: std::sync::Mutex<()> = std::sync::Mutex::new(());
        static E: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let base = temp_base("scanstandin");
        let mods = base.join("mods");
        mod_zip(&mods.join("archive/Tools/FS25_A.zip"));
        std::fs::write(mods.join("FS25_A.zip"), b"not the archived build").unwrap();
        let key = crate::modsroot::canonical_key(&mods);
        let mut conn = crate::db::open(&base.join("silo.db")).unwrap();
        let active = crate::modsroot::Active {
            layout: Layout::default_for(&mods),
            move_to: None,
        };
        let plan = super::scan_plan(Vec::new(), std::slice::from_ref(&mods), Some(&active));
        let start = super::quiet_epoch(&L, &E);
        let found = super::scan_library(&mut conn, &plan, None, &L, &E, start, |_, _| {});

        assert_eq!(found.mods.len(), 1);
        let m = &found.mods[0];
        assert_eq!(Path::new(&m.path), mods.join("FS25_A.zip"));
        assert!(m.organized && m.active);
        assert!(m.error.is_some(), "described from the swapped-in file");
        let rows = crate::db::load_organized(&conn, &key);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (rows[0].category.as_str(), rows[0].file_name.as_str()),
            ("Tools", "FS25_A.zip")
        );
        assert!(rows[0].active);
        assert!(crate::db::load_cache(&conn).contains_key(m.path.as_str()));
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn overlap_messages_are_plain_sentences() {
        let f = fences();
        assert!(overlap_problems(Path::new("/other/mods/sub"), &f).contains(
            &"That folder overlaps another mods folder Silo knows. Pick a folder outside it."
                .to_string()
        ));
        assert!(overlap_problems(Path::new("/x/a2/deeper"), &f).contains(
            &"That folder overlaps another mods folder's archive. Pick a folder outside it."
                .to_string()
        ));
    }

    #[test]
    fn a_mods_folder_whose_own_archive_is_another_folders_is_refused() {
        // The default `<X>/archive` is another folder's chosen archive...
        let archives = vec![PathBuf::from("/x/archive")];
        assert!(super::mods_folder_problem(
            Path::new("/x"),
            Path::new("/x/archive"),
            &archives,
            &[]
        )
        .is_some());
        // ...or its own chosen archive sits inside another folder's (the folder itself is
        // clear, so this is the own-archive check speaking).
        let archives = vec![PathBuf::from("/hdd/lib")];
        let why = super::mods_folder_problem(
            Path::new("/ssd/mods"),
            Path::new("/hdd/lib/a"),
            &archives,
            &[],
        );
        assert_eq!(
            why.map(|w| w.message).as_deref(),
            Some("That folder's archive would be another mods folder's archive. Pick another folder.")
        );
    }

    /// The re-point from #188: folder X was only ever followed through FS25's override (never
    /// in the known list) and its archive was moved to B. Once Silo follows D again, D's
    /// archive may not be re-pointed (or moved) at B, nor a folder inside B or holding it —
    /// two layouts would share one archive. D's own stored archive isn't a fence.
    #[test]
    fn fences_include_every_stored_archive_but_the_folders_own() {
        let base = temp_base("fences_stored");
        let d = base.join("d-mods");
        let x = base.join("x-mods");
        let b = base.join("b-archive");
        let mine = base.join("d-archive");
        for p in [&d, &x, &b, &mine] {
            std::fs::create_dir_all(p).unwrap();
        }
        let conn = crate::db::open(&base.join("silo.db")).unwrap();
        let set = |mods: &Path, archive: &Path| {
            crate::db::set_app_setting(
                &conn,
                &crate::relocate::archive_setting_key(&crate::modsroot::canonical_key(mods)),
                Some(&archive.to_string_lossy()),
            )
            .unwrap();
        };
        set(&x, &b);
        set(&d, &mine);
        let layout = Layout {
            mods: d.clone(),
            archive: mine.clone(),
        };
        // X isn't known and isn't in use: only its stored archive choice names it — both as a
        // mods folder and by its archive.
        let f = super::fences_with(&conn, &layout, &[], None, None).unwrap();
        assert_eq!(f.other_mods, vec![super::key_path(&x)]);
        assert_eq!(f.other_archives, vec![super::key_path(&b)]);
        assert!(
            has(
                &fence_problems(&layout, &x, &f),
                "overlaps another mods folder Silo knows"
            ),
            "X itself can't become D's archive"
        );
        let problems = fence_problems(&layout, &b, &f);
        assert!(
            has(&problems, "Another mods folder already uses that archive."),
            "{problems:?}"
        );
        assert!(!fence_problems(&layout, &b.join("Tools"), &f).is_empty());
        let clear = fence_problems(&layout, &base.join("clear"), &f);
        assert!(clear.is_empty(), "{clear:?}");
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The other half of the #188 re-point: X was followed only through FS25's override and
    /// organized into its default `X/archive` (no stored setting, never known). Once Silo
    /// follows D again, D's archive may not be re-pointed at `X/archive`, and `X/archive` (or a
    /// category in it) can't be chosen as a mods folder either: X's organized rows name it.
    #[test]
    fn an_override_only_folders_default_archive_is_fenced() {
        let base = temp_base("fences_override_default");
        let d = base.join("d-mods");
        let x = base.join("x-mods");
        let x_archive = crate::organize::Layout::default_for(&x).archive;
        std::fs::create_dir_all(&d).unwrap();
        std::fs::create_dir_all(x_archive.join("Tools")).unwrap();
        let conn = crate::db::open(&base.join("silo.db")).unwrap();
        let row = crate::db::OrganizedRow {
            tech_name: "FS25_A".into(),
            file_name: "FS25_A.zip".into(),
            kind: "zip".into(),
            category: "Tools".into(),
            subcategory: None,
            active: false,
        };
        crate::db::upsert_organized(&conn, &crate::modsroot::canonical_key(&x), &row).unwrap();
        let layout = Layout::default_for(&d);
        let f = super::fences_with(&conn, &layout, &[], None, None).unwrap();
        assert_eq!(f.other_archives, vec![super::key_path(&x_archive)]);
        let problems = fence_problems(&layout, &x_archive, &f);
        assert!(
            has(&problems, "Another mods folder already uses that archive."),
            "{problems:?}"
        );
        assert_eq!(
            super::apply_mods_root_choice(
                &conn,
                &x_archive.join("Tools").to_string_lossy(),
                true,
                None,
                &[]
            ),
            Err(super::OVERLAPS_AN_ARCHIVE.to_string())
        );
        assert_eq!(
            crate::db::get_app_setting(&conn, crate::modsroot::CHOICE_KEY),
            None
        );
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// An override-only folder X (organized rows, never known) is a mods folder to keep clear
    /// of, not just its archive: D's archive can't be re-pointed at an empty folder inside X.
    #[test]
    fn an_override_only_folder_is_fenced_as_a_mods_folder() {
        let base = temp_base("fences_override_mods");
        let d = base.join("d-mods");
        let x = base.join("x-mods");
        let inside = x.join("OtherLibrary");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::create_dir_all(&inside).unwrap();
        let conn = crate::db::open(&base.join("silo.db")).unwrap();
        let row = crate::db::OrganizedRow {
            tech_name: "FS25_A".into(),
            file_name: "FS25_A.zip".into(),
            kind: "zip".into(),
            category: "Tools".into(),
            subcategory: None,
            active: false,
        };
        crate::db::upsert_organized(&conn, &crate::modsroot::canonical_key(&x), &row).unwrap();
        let layout = Layout::default_for(&d);
        let f = super::fences_with(&conn, &layout, &[], None, None).unwrap();
        assert_eq!(f.other_mods, vec![super::key_path(&x)]);
        let problems = fence_problems(&layout, &inside, &f);
        assert!(
            has(&problems, "overlaps another mods folder Silo knows"),
            "{problems:?}"
        );
        // The library list can't be read: refused, never passed.
        conn.execute_batch("DROP TABLE organized").unwrap();
        assert_eq!(
            super::fences_with(&conn, &layout, &[], None, None).err().as_deref(),
            Some("Silo couldn't read its list of archives, so it can't check this folder. Try again.")
        );
        assert!(super::mods_folder_check(&conn, &d, &[]).is_err());
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The stored archives can't be read: the fences refuse in plain words instead of passing,
    /// and so does every check built on them (a move, a re-point, a found-again archive).
    #[test]
    fn fences_refuse_when_the_stored_archives_cant_be_read() {
        let base = temp_base("fences_unreadable");
        let d = base.join("d-mods");
        let new = base.join("new-archive");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        let conn = crate::db::open(&base.join("silo.db")).unwrap();
        conn.execute_batch("DROP TABLE app_setting").unwrap();
        let layout = Layout::default_for(&d);
        let e = super::fences_with(&conn, &layout, &[], None, None)
            .err()
            .expect("an unreadable list is never a pass");
        assert_eq!(
            e,
            "Silo couldn't read its list of archives, so it can't check this folder. Try again."
        );
        let problems = super::target_problems(&conn, &layout, &new, None);
        assert!(
            has(&problems, "couldn't read its list of archives"),
            "{problems:?}"
        );
        assert!(!super::rediscovered_archive_fits(&conn, &d, &new));
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn other_archives_lists_stored_and_default_archives_but_not_its_own() {
        let base = temp_base("otherarchives");
        let mine = base.join("mine");
        let other = base.join("other");
        let unlisted = base.join("unlisted");
        let lib = base.join("lib");
        let mine_lib = base.join("mine-lib");
        let stale = base.join("stale-lib");
        for d in [
            &mine,
            &other.join("archive"),
            &unlisted,
            &lib,
            &mine_lib,
            &stale,
        ] {
            std::fs::create_dir_all(d).unwrap();
        }
        let conn = crate::db::open(&base.join("silo.db")).unwrap();
        let set = |mods: &Path, archive: &Path| {
            crate::db::set_app_setting(
                &conn,
                &crate::relocate::archive_setting_key(&crate::modsroot::canonical_key(mods)),
                Some(&archive.to_string_lossy()),
            )
            .unwrap();
        };
        set(&mine, &mine_lib);
        set(&unlisted, &stale); // a mods folder no longer in the known list
        let known = vec![mine.clone(), other.clone(), base.join("mods")];
        set(&base.join("mods"), &lib);
        let got: HashSet<PathBuf> = super::other_archives(&conn, &mine, &known)
            .unwrap()
            .iter()
            .map(|p| super::key_path(p))
            .collect();
        let want: HashSet<PathBuf> = [
            super::key_path(&other.join("archive")),
            super::key_path(&lib),
            super::key_path(&stale),
        ]
        .into_iter()
        .collect();
        assert_eq!(got, want);
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn cancel_falls_back_only_when_a_move_holds_the_lock() {
        assert!(cancel_falls_back(super::MOVE_LOCK_HELD));
        // relocate's own guard refusal is a different case.
        assert!(!cancel_falls_back("An archive move is already running."));
        assert!(!cancel_falls_back(super::LIBRARY_BUSY));
        assert!(!cancel_falls_back("There's no archive move to cancel."));
    }

    #[test]
    fn foreign_mods_problem_pluralizes() {
        assert!(foreign_mods_problem(1).contains("already has 1 mod in subfolders"));
        assert!(foreign_mods_problem(1).contains("move it to"));
        assert!(foreign_mods_problem(4).contains("already has 4 mods in subfolders"));
        assert!(foreign_mods_problem(4).contains("move them to"));
    }

    /// Finishing, or pausing because the user asked, is routine; a pause for a reason (the game
    /// started), a pause on errors, or a refusal is worth a warning.
    #[test]
    fn move_outcome_level_warns_only_when_something_went_wrong() {
        let m = MoveMarker::new("/a".into(), "/b".into());
        assert_eq!(move_outcome_level(&Ok(None)), log::Level::Info);
        assert_eq!(move_outcome_level(&Ok(Some(m.clone()))), log::Level::Info);
        let mut e = m.clone();
        e.errors.push("x".into());
        assert_eq!(move_outcome_level(&Ok(Some(e))), log::Level::Warn);
        let mut g = m;
        g.pause_reason = Some("Farming Simulator 25 started".into());
        assert_eq!(move_outcome_level(&Ok(Some(g))), log::Level::Warn);
        assert_eq!(move_outcome_level(&Err("nope".into())), log::Level::Warn);
    }

    #[test]
    fn move_outcome_names_finished_paused_and_refused() {
        let m = MoveMarker::new("/a".into(), "/b".into());
        assert_eq!(
            move_outcome("start", &Ok(None)),
            "archive move start: finished"
        );
        assert!(move_outcome("resume", &Ok(Some(m.clone())))
            .contains("paused (on request; 0 error(s))"));
        let mut e = m.clone();
        e.errors.push("x".into());
        assert!(
            move_outcome("resume", &Ok(Some(e))).contains("paused (stopped on errors; 1 error(s))")
        );
        let mut g = m;
        g.pause_reason = Some("Farming Simulator 25 started".into());
        assert!(move_outcome("start", &Ok(Some(g))).contains("(Farming Simulator 25 started; 0"));
        assert_eq!(
            move_outcome("cancel", &Err("nope".into())),
            "archive move cancel: refused: nope"
        );
    }

    fn input(tech: &str, file: &str) -> ModInput {
        ModInput {
            tech_name: tech.into(),
            file_name: file.into(),
            kind: "zip".into(),
            category: "Tractors".into(),
            subcategory: None,
        }
    }

    #[test]
    fn split_adopt_drops_known_and_repeated_tech_names() {
        let known: HashSet<String> = ["FS25_Known".to_string()].into_iter().collect();
        let (inputs, dups) = split_adopt(
            vec![
                input("FS25_B", "FS25_B.zip"),
                input("FS25_Known", "FS25_Known.zip"),
                // Same techName twice: the first by file name wins.
                input("FS25_A", "FS25_A.zip"),
                input("FS25_A", "FS25_A"),
            ],
            &known,
        );
        let files: Vec<&str> = inputs.iter().map(|m| m.file_name.as_str()).collect();
        assert_eq!(files, ["FS25_A", "FS25_B.zip"]);
        assert_eq!(dups, ["FS25_A.zip", "FS25_Known.zip"]);
    }

    #[test]
    fn select_adopt_keeps_only_server_candidates() {
        let cands = vec![input("FS25_A", "FS25_A.zip"), input("FS25_B", "FS25_B.zip")];
        let (picked, missing) = select_adopt(
            cands,
            &["FS25_B.zip".to_string(), "..\\evil.zip".to_string()],
        );
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].tech_name, "FS25_B");
        assert_eq!(missing, ["..\\evil.zip"]);
    }

    fn space(archive_same_drive: bool, free: Option<u64>) -> Space {
        Space {
            archive_same_drive,
            renames: false,
            bytes: 10 << 30,
            largest: 2 << 30,
            free,
            largest_unkept: 2 << 30,
            relink_kept: 0,
            relink_copied: 0,
            relink_largest_copied: 0,
            mods_same_drive: true,
            old_on_mods_drive: false,
            mods_free: free,
        }
    }

    fn facts(space: Space) -> MoveFacts {
        MoveFacts {
            target: vec![],
            unknown: vec![],
            missing: 0,
            collisions: vec![],
            space,
        }
    }

    #[test]
    fn move_problems_lists_everything_in_order() {
        let p = move_problems(MoveFacts {
            target: vec!["path".into()],
            unknown: vec!["FS25_B.zip is filed twice".into(), "3 mods unknown".into()],
            missing: 2,
            collisions: vec!["FS25_A: x already exists".into()],
            space: space(false, Some(1 << 30)),
        });
        assert_eq!(p.len(), 6, "{p:?}");
        assert_eq!(p[0], "path");
        assert_eq!(p[1], "FS25_B.zip is filed twice");
        assert_eq!(p[2], "3 mods unknown");
        assert!(p[3].starts_with("2 mods in your library are missing from the archive"));
        assert!(p[3].contains("Rescan first"));
        assert_eq!(p[4], "FS25_A: x already exists");
        assert!(p[5].starts_with("Not enough free space"));
    }

    #[test]
    fn missing_problem_pluralizes() {
        let p = move_problems(MoveFacts {
            missing: 1,
            ..facts(space(false, None))
        });
        assert!(
            p[0].starts_with("1 mod in your library is missing"),
            "{p:?}"
        );
    }

    #[test]
    fn move_problems_on_the_same_drive_need_room_for_one_mod_at_a_time() {
        // Each mod's old copy (and any hardlink to it, re-linked first) is gone before the next
        // is copied, so the whole archive never needs to fit twice — only the largest mod.
        assert!(move_problems(facts(space(true, Some(4 << 30)))).is_empty());
        let p = move_problems(facts(space(true, Some(1 << 30))));
        assert!(p[0].starts_with("Not enough free space"), "{p:?}");
        // Unknown free space never blocks.
        assert!(move_problems(facts(space(false, None))).is_empty());
        assert!(move_problems(facts(space(true, None))).is_empty());
    }

    /// Renamed on one volume (#401): the archive needs no room, and kept hardlinks cost
    /// nothing (the renamed file is still theirs). A copy into the mods folder still counts.
    #[test]
    fn move_problems_need_no_archive_room_when_mods_are_renamed() {
        let renamed = |relink_kept, relink_copied, free| Space {
            renames: true,
            relink_kept,
            relink_copied,
            ..space(true, free)
        };
        assert!(move_problems(facts(renamed(5 << 30, 0, Some(0)))).is_empty());
        let p = move_problems(facts(renamed(0, 3 << 30, Some(1 << 30))));
        assert!(
            p[0].starts_with("Not enough free space: needs 4.3 GB"),
            "{p:?}"
        );
    }

    /// The mods folder rejects hardlinks (a cloud-synced folder, #135) or the new archive's
    /// drive can't be identified, so re-links can only copy (#188). On one drive, each kept
    /// hardlink's old bytes stay used and each missing or symlinked active mod is copied into
    /// the mods folder: both count on top of the largest mod.
    #[test]
    fn move_problems_count_re_link_copies_where_links_cant_be_made() {
        let gb = |n: u64| n << 30;
        let with = |kept: u64, copied: u64, mods_same_drive: bool, free, mods_free| Space {
            relink_kept: kept,
            relink_copied: copied,
            mods_same_drive,
            mods_free,
            ..space(true, free)
        };
        // Largest (2) + margin (1) fits in 4 GB, but not with a 3 GB kept hardlink as well.
        assert!(move_problems(facts(with(0, 0, true, Some(gb(4)), None))).is_empty());
        let p = move_problems(facts(with(gb(3), 0, true, Some(gb(4)), None)));
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(
            p[0].starts_with("Not enough free space: needs 6.4 GB"),
            "{p:?}"
        );
        // One kept 10 GB mod and nothing else: only its new copy is new, so 12 GB is enough.
        assert!(move_problems(facts(Space {
            largest: gb(10),
            largest_unkept: 0,
            ..with(gb(10), 0, true, Some(gb(12)), None)
        }))
        .is_empty());
        // A copy into a mods folder on the same drive counts on that drive too.
        let p = move_problems(facts(with(0, gb(3), true, Some(gb(4)), None)));
        assert!(
            p[0].starts_with("Not enough free space: needs 6.4 GB"),
            "{p:?}"
        );
        // Across drives the whole archive must fit; kept hardlinks stay on the old drive.
        assert!(move_problems(facts(Space {
            archive_same_drive: false,
            ..with(gb(3), 0, true, Some(gb(12)), None)
        }))
        .is_empty());
        // The mods folder on its own drive: its copies are checked against that drive.
        let p = move_problems(facts(with(0, gb(3), false, Some(gb(4)), Some(gb(2)))));
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(
            p[0].starts_with("Not enough free space on your mods folder's drive"),
            "{p:?}"
        );
        assert!(move_problems(facts(with(0, gb(3), false, Some(gb(4)), Some(gb(5))))).is_empty());
        // The old archive shares the mods folder's drive: each mod's old copy there is deleted
        // right after its copy is made in the mods folder, so 100 such 1 GB mods need room for
        // one, not all 100.
        let shared_old = Space {
            relink_largest_copied: gb(1),
            old_on_mods_drive: true,
            archive_same_drive: false,
            ..with(0, gb(100), false, Some(gb(200)), Some(gb(3)))
        };
        assert!(move_problems(facts(shared_old)).is_empty());
        let p = move_problems(facts(Space {
            mods_free: Some(gb(1)),
            ..shared_old
        }));
        assert!(
            p[0].starts_with("Not enough free space on your mods folder's drive"),
            "{p:?}"
        );
        // Unknown free space on the mods folder's drive never blocks.
        assert!(move_problems(facts(with(0, gb(3), false, Some(gb(4)), None))).is_empty());
    }

    // Against a lock of its own: other tests hold `LIBRARY_WRITE`, so asserting "not busy" on
    // the real one would race them.
    #[test]
    fn library_busy_reflects_the_write_lock() {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        assert!(!super::busy_with(&LOCK, false));
        let g = LOCK.lock().unwrap();
        assert!(super::busy_with(&LOCK, false));
        drop(g);
        assert!(!super::busy_with(&LOCK, false));
        assert!(super::busy_with(&LOCK, true), "a running move counts");
    }

    #[test]
    fn a_poisoned_but_free_lock_is_not_busy() {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _ = std::thread::spawn(|| {
            let _g = LOCK.lock().unwrap();
            panic!("poison the lock (expected in this test)");
        })
        .join();
        assert!(LOCK.is_poisoned());
        assert!(!super::busy_with(&LOCK, false));
    }

    #[test]
    fn ensure_no_move_refuses_while_a_move_holds_the_lock() {
        use std::sync::atomic::Ordering::SeqCst;
        super::MOVE_HOLDS_LOCK.store(true, SeqCst);
        let r = super::ensure_no_move();
        super::MOVE_HOLDS_LOCK.store(false, SeqCst);
        assert_eq!(
            r,
            Err("Finish or cancel the archive move first.".to_string())
        );
    }

    #[test]
    fn a_finished_update_download_is_not_placed_while_a_move_is_unfinished() {
        use std::sync::atomic::Ordering::SeqCst;
        super::MOVE_HOLDS_LOCK.store(true, SeqCst);
        let r = super::update_recheck(Path::new("/nowhere/mods/FS25_A.zip"));
        super::MOVE_HOLDS_LOCK.store(false, SeqCst);
        assert_eq!(
            r.err(),
            Some("Finish or cancel the archive move first.".to_string())
        );
    }

    #[test]
    fn a_scan_reconciles_only_when_no_write_overlapped_it() {
        assert!(super::scan_may_reconcile(Some(3), Some(3)));
        assert!(
            !super::scan_may_reconcile(Some(3), Some(4)),
            "a write ran during the scan"
        );
        assert!(
            !super::scan_may_reconcile(None, Some(3)),
            "a write was running when it began"
        );
        assert!(
            !super::scan_may_reconcile(Some(3), None),
            "a write is running now"
        );
    }

    #[test]
    fn quiet_epoch_is_none_while_a_writer_holds_the_lock() {
        static L: std::sync::Mutex<()> = std::sync::Mutex::new(());
        static E: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(7);
        assert_eq!(super::quiet_epoch(&L, &E), Some(7));
        let _held = L.lock().unwrap();
        let seen = std::thread::spawn(|| super::quiet_epoch(&L, &E))
            .join()
            .unwrap();
        assert_eq!(seen, None);
    }

    #[test]
    fn a_write_during_a_scan_keeps_that_scan_from_overwriting_it() {
        use crate::db;
        static L: std::sync::Mutex<()> = std::sync::Mutex::new(());
        static E: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let base = temp_base("scan_race");
        let mut conn = db::open(&base.join("silo.db")).unwrap();
        let row = |tech: &str| db::OrganizedRow {
            tech_name: tech.into(),
            file_name: format!("{tech}.zip"),
            kind: "zip".into(),
            category: "Vehicles".into(),
            subcategory: None,
            active: false,
        };
        let names = |conn: &rusqlite::Connection| {
            let mut v: Vec<String> = db::load_organized(conn, "r")
                .into_iter()
                .map(|r| r.tech_name)
                .collect();
            v.sort();
            v
        };

        // The scan starts on a quiet library and finds only FS25_A on disk...
        let start = super::quiet_epoch(&L, &E);
        let seen_on_disk = vec![row("FS25_A")];
        // ...while an organize files FS25_B (taking the lock the way every write does).
        {
            let _w = super::take_for_write(&L, &E).unwrap();
            db::upsert_organized(&conn, "r", &row("FS25_B")).unwrap();
        }
        assert!(!super::reconcile_if_quiet(
            &L,
            &E,
            start,
            &mut conn,
            "r",
            &seen_on_disk
        ));
        assert_eq!(names(&conn), ["FS25_B"], "the organize's record is kept");

        // A scan that began after the write may bring the record in line with disk.
        let start = super::quiet_epoch(&L, &E);
        let seen_on_disk = vec![row("FS25_A"), row("FS25_B")];
        assert!(super::reconcile_if_quiet(
            &L,
            &E,
            start,
            &mut conn,
            "r",
            &seen_on_disk
        ));
        assert_eq!(names(&conn), ["FS25_A", "FS25_B"]);
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_scan_never_reconciles_while_a_write_holds_the_lock() {
        static L: std::sync::Mutex<()> = std::sync::Mutex::new(());
        static E: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let base = temp_base("scan_held");
        let mut conn = crate::db::open(&base.join("silo.db")).unwrap();
        let start = super::quiet_epoch(&L, &E);
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        // Held without moving the epoch on, so only the "held right now" check can refuse.
        let writer = std::thread::spawn(move || {
            let _w = L.lock().unwrap();
            held_tx.send(()).unwrap();
            let _ = done_rx.recv_timeout(std::time::Duration::from_secs(5));
        });
        held_rx.recv().unwrap();
        let did = super::reconcile_if_quiet(&L, &E, start, &mut conn, "r", &[]);
        done_tx.send(()).unwrap();
        writer.join().unwrap();
        assert!(!did);
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_failed_reconcile_is_logged_and_still_counts_as_a_quiet_scan() {
        static L: std::sync::Mutex<()> = std::sync::Mutex::new(());
        static E: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let base = temp_base("scan_reconcile_fails");
        let mut conn = crate::db::open(&base.join("silo.db")).unwrap();
        conn.execute_batch("DROP TABLE organized").unwrap();
        let row = crate::db::OrganizedRow {
            tech_name: "FS25_A".into(),
            file_name: "FS25_A.zip".into(),
            kind: "zip".into(),
            category: "Tractors".into(),
            subcategory: None,
            active: false,
        };
        assert!(
            crate::db::reconcile_organized(&mut conn, "r", std::slice::from_ref(&row)).is_err()
        );
        // The library was quiet: the manifest wasn't skipped for a write, it failed (logged),
        // and the next scan retries it.
        let start = super::quiet_epoch(&L, &E);
        assert!(super::reconcile_if_quiet(
            &L,
            &E,
            start,
            &mut conn,
            "r",
            &[row]
        ));
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn every_library_write_moves_the_epoch_on() {
        use std::sync::atomic::Ordering::SeqCst;
        let before = super::LIBRARY_EPOCH.load(SeqCst);
        // Checks stubbed out, as in the deadline test below: they read global state.
        let guard = super::wait_for_library(std::time::Duration::from_secs(20), || Ok(()))
            .expect("the library lock");
        assert!(super::LIBRARY_EPOCH.load(SeqCst) > before);
        drop(guard);
    }

    #[test]
    fn the_library_lock_gives_up_instead_of_waiting_forever() {
        use std::time::Duration;
        let (tx, rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        // Hold the lock briefly from another thread (poison-tolerant, never panics holding it).
        let holder = std::thread::spawn(move || {
            let _g = super::LIBRARY_WRITE
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            tx.send(()).unwrap();
            let _ = done_rx.recv_timeout(Duration::from_secs(5));
        });
        rx.recv().unwrap();
        // The wait loop itself, with the move/loading checks stubbed out: `library_write`'s
        // checks read global state other tests flip in parallel, so asserting through it would
        // test those checks (often "still loading"), not the deadline.
        let start = std::time::Instant::now();
        let waited = super::wait_for_library(Duration::from_millis(300), || Ok(()));
        let waited_for = start.elapsed();
        done_tx.send(()).unwrap();
        holder.join().unwrap();
        assert_eq!(waited.err(), Some(super::LIBRARY_BUSY.to_string()));
        assert!(waited_for >= Duration::from_millis(300), "{waited_for:?}");
        assert!(waited_for < Duration::from_secs(2), "{waited_for:?}");
    }

    #[test]
    fn placing_falls_back_to_rename_only_without_hardlink_support() {
        use std::io::{Error, ErrorKind};
        let base = temp_base("place_fallback");
        let mods = base.join("mods");
        let no_links = |_: &Path, _: &Path| Err(Error::from(ErrorKind::Unsupported));
        // No hardlinks, destination free: renamed into place.
        let dest = mods.join("FS25_A.zip");
        let part = dest.with_extension("zip.part");
        std::fs::write(&part, b"a").unwrap();
        super::place_part_with(&part, &dest, no_links).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"a");
        assert!(!part.exists());
        // No hardlinks, destination taken: refused, theirs untouched, the part left to its
        // claim (#400).
        std::fs::write(&part, b"b").unwrap();
        let err = super::place_part_with(&part, &dest, no_links).unwrap_err();
        assert_eq!(err, "FS25_A.zip is already in your library");
        assert_eq!(std::fs::read(&dest).unwrap(), b"a");
        assert_eq!(std::fs::read(&part).unwrap(), b"b");
        // A real link failure is reported, never papered over with a rename.
        let free = mods.join("FS25_B.zip");
        let part = free.with_extension("zip.part");
        std::fs::write(&part, b"c").unwrap();
        let broken = |_: &Path, _: &Path| Err(Error::other("disk on fire"));
        let err = super::place_part_with(&part, &free, broken).unwrap_err();
        assert_eq!(err, "disk on fire");
        assert!(!free.exists());
        // The part vanished: a plain message.
        let gone = |_: &Path, _: &Path| Err(Error::from(ErrorKind::NotFound));
        let err = super::place_part_with(&part, &free, gone).unwrap_err();
        assert_eq!(err, super::DOWNLOAD_GONE);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_rename_fallback_never_replaces_a_file_that_appears_after_the_link_attempt() {
        use std::io::{Error, ErrorKind};
        let base = temp_base("place_race");
        let dest = base.join("mods").join("FS25_Race.zip");
        let part = dest.with_extension("zip.part");
        std::fs::write(&part, b"download").unwrap();
        // The link fails for lack of support, and meanwhile another program creates `dest`.
        let racer = |_: &Path, d: &Path| {
            std::fs::write(d, b"user's only copy").unwrap();
            Err(Error::from(ErrorKind::Unsupported))
        };
        let err = super::place_part_with(&part, &dest, racer).unwrap_err();
        assert_eq!(err, "FS25_Race.zip is already in your library");
        assert_eq!(std::fs::read(&dest).unwrap(), b"user's only copy");
        assert_eq!(
            std::fs::read(&part).unwrap(),
            b"download",
            "left to its claim"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_busy_library_fails_the_pending_downloads_and_removes_their_parts() {
        let base = temp_base("place_pending_busy");
        let dest = base.join("mods").join("FS25_Wait.zip");
        let part = super::claim_part(&dest, None).unwrap();
        std::io::Write::write_all(&mut part.create().unwrap(), b"done").unwrap();
        let part_path = part.path().to_path_buf();
        let mut rows = vec![super::ApplyRow {
            tech_name: "FS25_Wait".into(),
            filename: Some("FS25_Wait.zip".into()),
            status: "installed".into(),
            verdict: None,
            detail: None,
        }];
        let pending = vec![super::PendingPlace {
            row: 0,
            part,
            dest: dest.clone(),
            verdict: "verified".into(),
        }];
        let (installed, failed) = super::place_pending(
            &mut rows,
            pending,
            Err(super::LIBRARY_BUSY.to_string()),
            |_| Ok(()),
        );
        assert_eq!((installed, failed), (0, 1));
        assert_eq!(rows[0].status, "failed");
        assert_eq!(rows[0].detail.as_deref(), Some(super::LIBRARY_BUSY));
        assert!(!dest.exists());
        assert!(!part_path.exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn claims_ignore_case_on_every_system() {
        // macOS (APFS) ignores case as Windows does; on Linux this only over-claims.
        let base = temp_base("claim_case");
        let mods = base.join("mods");
        let _first = super::claim_part(&mods.join("FS25_Foo.zip"), None).unwrap();
        assert!(super::claim_part(&mods.join("fs25_foo.zip"), None).is_err());
        assert!(super::claim_part_at(
            &mods.join("FS25_FOO.zip"),
            &mods.join("FS25_FOO.zip.part"),
            super::INSTALL_PART_SUFFIX,
            None
        )
        .is_err());
        // NTFS uppercases, so both lowercase sigmas are one name.
        let _sigma = super::claim_part(&mods.join("FS25_\u{3c3}.zip"), None).unwrap();
        assert!(super::claim_part(&mods.join("FS25_\u{3c2}.zip"), None).is_err());
        assert!(super::claim_part(&mods.join("FS25_\u{3a3}.zip"), None).is_err());
        // Two spellings of one folder share keys: another case on Windows, a link elsewhere.
        #[cfg(windows)]
        let alias = base.join("MODS");
        #[cfg(unix)]
        let alias = {
            let a = base.join("mods-link");
            std::os::unix::fs::symlink(&mods, &a).unwrap();
            a
        };
        assert_ne!(alias, mods);
        assert!(super::claim_part(&alias.join("FS25_Foo.zip"), None).is_err());
        // NTFS compares names by uppercasing each character, without reordering accents.
        let _ypo = super::claim_part(&mods.join("FS25_A\u{345}\u{301}.zip"), None).unwrap();
        assert!(super::claim_part(&mods.join("FS25_A\u{399}\u{301}.zip"), None).is_err());
        // APFS also treats the two ways of writing an accent as one name.
        let _cafe = super::claim_part(&mods.join("FS25_Caf\u{e9}.zip"), None).unwrap();
        assert!(super::claim_part(&mods.join("FS25_Cafe\u{301}.zip"), None).is_err());
        assert!(super::claim_part(&mods.join("fs25_CAFE\u{301}.zip"), None).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn collection_downloads_are_not_placed_once_their_mods_folder_was_switched_away() {
        let base = temp_base("place_pending_switched");
        let dest = base.join("mods").join("FS25_Moved.zip");
        let part = super::claim_part(&dest, None).unwrap();
        std::io::Write::write_all(&mut part.create().unwrap(), b"done").unwrap();
        let part_path = part.path().to_path_buf();
        let mut rows = vec![super::ApplyRow {
            tech_name: "FS25_Moved".into(),
            filename: Some("FS25_Moved.zip".into()),
            status: "installed".into(),
            verdict: None,
            detail: None,
        }];
        let pending = vec![super::PendingPlace {
            row: 0,
            part,
            dest: dest.clone(),
            verdict: "verified".into(),
        }];
        let (installed, failed) = super::place_pending(&mut rows, pending, Ok(()), |_| {
            Err(super::MODS_FOLDER_CHANGED.to_string())
        });
        assert_eq!((installed, failed), (0, 1));
        assert_eq!(rows[0].status, "failed");
        assert_eq!(rows[0].detail.as_deref(), Some(super::MODS_FOLDER_CHANGED));
        assert!(!dest.exists());
        assert!(!part_path.exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_set_naming_one_file_twice_reports_both_rows_as_that_download_ended() {
        let row = |status: &str, detail: Option<&str>| super::ApplyRow {
            tech_name: "FS25_Dup".into(),
            filename: Some("FS25_Dup.zip".into()),
            status: status.into(),
            verdict: None,
            detail: detail.map(str::to_string),
        };
        let mut rows = vec![
            row("failed", Some(super::LIBRARY_BUSY)),
            row("present", None),
            row("installed", None),
            row("present", None),
        ];
        let failed = super::settle_duplicates(&mut rows, &[(1, 0), (3, 2)]);
        assert_eq!(failed, 1);
        assert_eq!(rows[1].status, "failed");
        assert_eq!(rows[1].detail.as_deref(), Some(super::LIBRARY_BUSY));
        assert_eq!(rows[3].status, "present");
    }

    #[test]
    fn browse_installs_a_differently_named_file_under_its_own_name_and_notes_it() {
        // Browse: allowed, only noted (the catalog's `FS25_ToDo` ships `FS25_FieldToDoList.zip`).
        let note = super::browse_name_note("FS25_FieldToDoList.zip", Some("FS25_ToDo")).unwrap();
        assert!(
            note.contains("FS25_ToDo") && note.contains("FS25_FieldToDoList.zip"),
            "{note}"
        );
        assert_eq!(
            super::browse_name_note("FS25_ToDo.zip", Some("fs25_todo")),
            None
        );
        assert_eq!(super::browse_name_note("FS25_X.zip", None), None);
        assert_eq!(super::browse_name_note("FS25_X.zip", Some("  ")), None);
        // A shared set: the same file is refused, since the saved set could never turn it on.
        assert!(
            crate::siloapi::check_download_name("FS25_FieldToDoList.zip", "FS25_ToDo").is_err()
        );
    }

    fn lookup_row(tech: Option<&str>, id: &str) -> crate::siloapi::LookupResult {
        serde_json::from_value(serde_json::json!({
            "techName": tech, "id": id, "title": "t", "latestVersion": "1.0.0.0"
        }))
        .unwrap()
    }

    fn todo_link() -> crate::catalogalias::Link {
        crate::catalogalias::Link {
            catalog_id: "cat-todo".into(),
            catalog_tech_name: Some("FS25_ToDo".into()),
            download_url: None,
        }
    }

    #[test]
    fn an_aliased_mod_is_looked_up_as_its_catalog_entry() {
        // #386: FS25_FieldToDoList.zip came from the catalog's FS25_ToDo.
        let asked = vec![
            ("FS25_FieldToDoList".to_string(), Some(todo_link())),
            ("FS25_Plain".to_string(), None),
            // An entry with no techName can only be fetched by id.
            (
                "FS25_Nameless".to_string(),
                Some(crate::catalogalias::Link {
                    catalog_id: "cat-nameless".into(),
                    catalog_tech_name: None,
                    download_url: None,
                }),
            ),
        ];
        assert_eq!(
            super::catalog_lookup_names(&asked),
            ["FS25_ToDo", "FS25_Plain"]
        );

        let results = vec![
            lookup_row(Some("FS25_ToDo"), "cat-todo"),
            lookup_row(Some("FS25_Plain"), "cat-plain"),
        ];
        let link = todo_link();
        let hit = super::catalog_hit("FS25_FieldToDoList", Some(&link), &results).unwrap();
        assert_eq!(hit.id, "cat-todo");
        let hit = super::catalog_hit("FS25_Plain", None, &results).unwrap();
        assert_eq!(hit.id, "cat-plain");
        // Without its alias the file isn't in the catalog, as before.
        assert!(super::catalog_hit("FS25_FieldToDoList", None, &results).is_none());
    }

    #[test]
    fn an_alias_never_falls_back_to_another_entry_with_the_file_name() {
        let link = todo_link();
        let results = vec![
            lookup_row(Some("FS25_FieldToDoList"), "someone-else"),
            lookup_row(Some("FS25_ToDo"), "cat-todo"),
        ];
        let hit = super::catalog_hit("FS25_FieldToDoList", Some(&link), &results).unwrap();
        assert_eq!(hit.id, "cat-todo");
        // The entry was renamed in the catalog and another entry has the file's name: not that
        // one (`resolve_catalog` then fetches the linked entry by its id).
        let results = vec![lookup_row(Some("FS25_FieldToDoList"), "someone-else")];
        assert!(super::catalog_hit("FS25_FieldToDoList", Some(&link), &results).is_none());
    }

    #[test]
    fn a_linked_entry_never_offers_an_older_build_as_its_latest() {
        let detail: crate::siloapi::BrowseMod = serde_json::from_value(serde_json::json!({
            "id": "cat-todo", "techName": "FS25_ToDo", "title": "To Do",
            "latestVersion": "2.0", "pageUrl": "https://modhub.example/x",
            "sources": [
                {"source": "modhub", "sourceUrl": "https://modhub.example/x", "version": "2.0",
                 "installable": false},
                {"source": "github", "sourceUrl": "https://github.com/k/FS25_ToDo", "version": "1.0",
                 "installable": true,
                 "downloadUrl": "https://github.com/k/FS25_ToDo/releases/download/1.0/FS25_FieldToDoList.zip"}
            ]
        }))
        .unwrap();
        let row = super::lookup_from_detail(detail);
        assert_eq!(row.latest_version.as_deref(), Some("2.0"));
        assert!(row.download.is_none(), "the page, not the 1.0 download");
        assert_eq!(row.page_url.as_deref(), Some("https://modhub.example/x"));
        assert!(super::same_version(Some("v1.2.0"), Some("1.2.0")));
        assert!(
            !super::same_version(None, Some("1.2.0")),
            "unknown isn't the latest"
        );
        assert!(!super::same_version(Some(""), Some("")));
        assert!(!super::same_version(Some("1.0"), Some("2.0")));

        // An older first source and a current second one: the current one.
        let detail: crate::siloapi::BrowseMod = serde_json::from_value(serde_json::json!({
            "id": "x", "title": "X", "latestVersion": "2.0",
            "sources": [
                {"source": "github", "sourceUrl": "https://github.com/k/a", "version": "1.0",
                 "installable": true, "downloadUrl": "https://github.com/k/a/releases/download/1.0/X.zip"},
                {"source": "github", "sourceUrl": "https://github.com/k/a", "installable": true,
                 "downloadUrl": "https://github.com/k/a/releases/download/unknown/X.zip"},
                {"source": "github", "sourceUrl": "https://github.com/k/b", "version": "v2.0",
                 "installable": true, "downloadUrl": "https://github.com/k/b/releases/download/2.0/X.zip"}
            ]
        }))
        .unwrap();
        let dl = super::lookup_from_detail(detail).download.unwrap();
        assert!(dl.url.contains("/k/b/"), "{}", dl.url);
    }

    #[test]
    fn a_failed_update_undoes_its_record_only_when_the_new_bytes_are_provably_not_there() {
        let dir = std::env::temp_dir().join(format!("silo_absent_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("FS25_FieldToDoList.zip");
        let ok =
            |h: &'static str| move |_: &std::path::Path| -> Result<String, String> { Ok(h.into()) };
        // No file: absent.
        assert!(super::new_bytes_absent_with(&dest, "aa", ok("aa")));
        std::fs::write(&dest, b"x").unwrap();
        // The new bytes are there (any case of hex): kept. Other bytes: absent.
        assert!(!super::new_bytes_absent_with(&dest, "AA", ok("aa")));
        assert!(super::new_bytes_absent_with(&dest, "aa", ok("bb")));
        // Unreadable: proves nothing, so the record is kept.
        let locked = |_: &std::path::Path| -> Result<String, String> { Err("in use".into()) };
        assert!(!super::new_bytes_absent_with(&dest, "aa", locked));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_a_404_counts_as_a_linked_entry_gone() {
        assert!(super::catalog_not_found(
            "Silo's catalog isn't answering right now (error 404). Try again in a few minutes."
        ));
        assert!(!super::catalog_not_found(
            "Silo's catalog isn't answering right now (error 500). Try again in a few minutes."
        ));
        assert!(!super::catalog_not_found(
            "Silo can't reach its catalog right now: timed out"
        ));
    }

    #[test]
    fn a_linked_entry_fetched_by_id_reads_like_a_lookup_row() {
        let detail: crate::siloapi::BrowseMod = serde_json::from_value(serde_json::json!({
            "id": "cat-todo", "techName": "FS25_ToDo", "title": "To Do",
            "latestVersion": "v0.2", "pageUrl": "https://github.com/k/FS25_ToDo",
            "sources": [
                {"source": "modhub", "sourceUrl": "https://modhub.example/x", "installable": false},
                {"source": "github", "sourceUrl": "https://github.com/k/FS25_ToDo",
                 "installable": true, "version": "v0.2",
                 "downloadUrl": "https://github.com/k/FS25_ToDo/releases/download/v0.2/FS25_FieldToDoList.zip"}
            ]
        }))
        .unwrap();
        let row = super::lookup_from_detail(detail);
        assert_eq!(row.id, "cat-todo");
        assert_eq!(row.latest_version.as_deref(), Some("v0.2"));
        let dl = row.download.unwrap();
        assert_eq!(dl.source, "github");
        assert!(dl.url.ends_with("/FS25_FieldToDoList.zip"));
        assert_eq!(
            row.page_url.as_deref(),
            Some("https://github.com/k/FS25_ToDo")
        );
    }

    #[test]
    fn an_update_keeps_the_alias_only_for_a_download_its_entry_lists() {
        let detail: crate::siloapi::BrowseMod = serde_json::from_value(serde_json::json!({
            "id": "cat-todo", "techName": "FS25_ToDo", "title": "To Do",
            "sources": [
                {"source": "github", "sourceUrl": "https://github.com/k/FS25_ToDo",
                 "installable": true,
                 "downloadUrl": "https://github.com/k/FS25_ToDo/releases/download/v2/FS25_FieldToDoList.zip"},
                {"source": "modhub", "sourceUrl": "https://modhub.example/x",
                 "installable": false,
                 "downloadUrl": "https://modhub.example/FS25_FieldToDoList.zip"}
            ]
        }))
        .unwrap();
        assert!(super::is_catalog_download(
            &detail,
            "https://github.com/k/FS25_ToDo/releases/download/v2/FS25_FieldToDoList.zip"
        ));
        assert!(!super::is_catalog_download(
            &detail,
            "https://github.com/someone/fork/releases/download/v2/FS25_FieldToDoList.zip"
        ));
        assert!(!super::is_catalog_download(
            &detail,
            "https://modhub.example/FS25_FieldToDoList.zip"
        ));
    }

    #[test]
    fn an_install_is_not_placed_once_its_mods_folder_was_switched_away() {
        let base = temp_base("install_recheck");
        let (a, b) = (base.join("A"), base.join("B"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        assert_eq!(
            super::still_a_mods_folder(&a, std::slice::from_ref(&a)),
            Ok(())
        );
        assert_eq!(
            super::still_a_mods_folder(&a, std::slice::from_ref(&b)),
            Err(super::MODS_FOLDER_CHANGED.to_string())
        );
        assert_eq!(
            super::still_a_mods_folder(&a, &[]),
            Err(super::MODS_FOLDER_CHANGED.to_string())
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn placing_a_download_never_overwrites_the_library() {
        let base = temp_base("place_part");
        let mods = base.join("mods");
        // Destination free: the part becomes the mod.
        let dest = mods.join("FS25_New.zip");
        let part = dest.with_extension("zip.part");
        std::fs::write(&part, b"new").unwrap();
        super::place_part(&part, &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"new");
        assert!(!part.exists());
        // Destination taken: refused, the existing file untouched. The claim then removes the
        // part it made.
        let taken = mods.join("FS25_Have.zip");
        std::fs::write(&taken, b"mine").unwrap();
        let claim = super::claim_part(&taken, None).unwrap();
        std::io::Write::write_all(&mut claim.create().unwrap(), b"theirs").unwrap();
        let part = claim.path().to_path_buf();
        let err = super::place_part(&part, &taken).unwrap_err();
        assert_eq!(err, "FS25_Have.zip is already in your library");
        assert_eq!(std::fs::read(&taken).unwrap(), b"mine");
        drop(claim);
        assert!(!part.exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_claim_removes_its_part_cut_off_inside_a_write() {
        // A write that stopped part-way (a full disk): the part's length lies inside the last
        // chunk and its proven prefix is intact, so dropping the claim still removes it.
        let base = temp_base("claim_mid_chunk");
        let dest = base.join("mods").join("FS25_Cut.zip");
        let claim = super::claim_part(&dest, None).unwrap();
        std::io::Write::write_all(&mut claim.create().unwrap(), b"0123456789abcdef").unwrap();
        let at = claim.path().to_path_buf();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&at)
            .and_then(|f| f.set_len(12))
            .unwrap();
        drop(claim);
        assert!(!at.exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_claim_never_removes_its_part_once_its_bytes_changed() {
        // #400: dropping a claim removes its part only while it holds exactly what the
        // download wrote; changed in place (same length) it stays.
        let base = temp_base("claim_changed");
        let dest = base.join("mods").join("FS25_Edit.zip");
        let claim = super::claim_part(&dest, None).unwrap();
        std::io::Write::write_all(&mut claim.create().unwrap(), b"download bytes").unwrap();
        let at = claim.path().to_path_buf();
        let mut f = std::fs::OpenOptions::new().write(true).open(&at).unwrap();
        std::io::Write::write_all(&mut f, b"USER").unwrap();
        drop(f);
        drop(claim);
        assert_eq!(std::fs::read(&at).unwrap(), b"USERload bytes");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_refused_placement_never_removes_a_file_put_at_the_parts_name() {
        // #400: the part was moved aside and a user's file put at its name; the destination
        // is taken, so placing is refused. Neither the refusal nor the claim removes it.
        let base = temp_base("place_refused_replaced");
        let mods = base.join("mods");
        let taken = mods.join("FS25_Have.zip");
        std::fs::write(&taken, b"mine").unwrap();
        let claim = super::claim_part(&taken, None).unwrap();
        std::io::Write::write_all(&mut claim.create().unwrap(), b"download").unwrap();
        let part = claim.path().to_path_buf();
        std::fs::rename(&part, base.join("aside")).unwrap();
        std::fs::write(&part, b"user's file").unwrap();
        assert!(super::place_part(&part, &taken).is_err());
        assert_eq!(std::fs::read(&part).unwrap(), b"user's file");
        drop(claim);
        assert_eq!(std::fs::read(&part).unwrap(), b"user's file");
        assert_eq!(std::fs::read(&taken).unwrap(), b"mine");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_download_claims_its_part_file_until_done() {
        let base = temp_base("claim_part");
        let dest = base.join("mods").join("FS25_Busy.zip");
        let db = base.join("silo.db");
        let claim = super::claim_part(&dest, Some(&db)).unwrap();
        // A name of its own beside the mod (#400), recorded before it's made.
        let name = claim
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            name.starts_with("FS25_Busy.zip.") && name.ends_with(".part"),
            "{name}"
        );
        assert_ne!(claim.path(), dest.with_extension("zip.part").as_path());
        let conn = super::db::open(&db).unwrap();
        let journal = super::scratch::DbJournal(&conn);
        let recorded = super::scratch::Journal::entries(&journal);
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].path, claim.path());
        assert_eq!(recorded[0].role, super::scratch::Role::Part);
        std::io::Write::write_all(&mut claim.create().unwrap(), b"half").unwrap();
        // Made once: a second create never opens what's there.
        assert!(claim.create().is_err());
        assert_eq!(std::fs::read(claim.path()).unwrap(), b"half");
        // A second download of the same file can't share (and corrupt) the part.
        assert!(super::claim_part(&dest, None).is_err());
        let part = claim.path().to_path_buf();
        drop(claim);
        // Dropping the claim removes the part it made, drops its record and frees the name.
        assert!(!part.exists());
        assert!(super::scratch::Journal::entries(&journal).is_empty());
        assert!(super::claim_part(&dest, None).is_ok());
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_claim_never_removes_a_file_it_didnt_make() {
        // #400: an old fixed-name part (or the user's own `.zip.part`, e.g. a browser's
        // partial download) is never replaced, written into or removed by a new download.
        let base = temp_base("claim_not_made");
        let dest = base.join("mods").join("FS25_Keep.zip");
        let db = base.join("silo.db");
        let legacy = dest.with_extension("zip.part");
        std::fs::write(&legacy, b"the user's partial download").unwrap();
        let claim = super::claim_part(&dest, Some(&db)).unwrap();
        // Something appears at the claim's own name before the download makes it.
        std::fs::write(claim.path(), b"not Silo's").unwrap();
        assert!(claim.create().is_err(), "never opens what's already there");
        let at = claim.path().to_path_buf();
        drop(claim);
        assert_eq!(std::fs::read(&at).unwrap(), b"not Silo's");
        assert_eq!(
            std::fs::read(&legacy).unwrap(),
            b"the user's partial download"
        );
        // The failed create leaves no record that a later sweep could act on.
        let conn = super::db::open(&db).unwrap();
        let journal = super::scratch::DbJournal(&conn);
        assert!(super::scratch::Journal::entries(&journal).is_empty());
        super::scratch::sweep_where(&journal, &|_| true);
        assert_eq!(std::fs::read(&at).unwrap(), b"not Silo's");
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_claim_never_removes_a_file_put_at_its_name_after_it_made_its_part() {
        // #400: dropping a claim removes only the very file it made.
        let base = temp_base("claim_replaced");
        let dest = base.join("mods").join("FS25_Swap.zip");
        let db = base.join("silo.db");
        let claim = super::claim_part(&dest, Some(&db)).unwrap();
        std::io::Write::write_all(&mut claim.create().unwrap(), b"silo's part").unwrap();
        let at = claim.path().to_path_buf();
        let moved = base.join("moved.zip");
        std::fs::rename(&at, &moved).unwrap();
        std::fs::write(&at, b"user's file").unwrap(); // not even a different size
        drop(claim);
        assert_eq!(std::fs::read(&at).unwrap(), b"user's file");
        assert_eq!(std::fs::read(&moved).unwrap(), b"silo's part");
        let conn = super::db::open(&db).unwrap();
        let journal = super::scratch::DbJournal(&conn);
        assert!(
            super::scratch::Journal::entries(&journal).is_empty(),
            "no record stands for the user's file"
        );
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A journal that keeps every entry ever recorded, to see what an operation recorded.
    #[derive(Default)]
    struct SpyJournal {
        now: crate::scratch::tests::MemJournal,
        seen: std::cell::RefCell<Vec<crate::scratch::Entry>>,
    }

    impl crate::scratch::Journal for SpyJournal {
        fn record(&self, e: &crate::scratch::Entry) -> Result<(), String> {
            self.seen.borrow_mut().push(e.clone());
            self.now.record(e)
        }
        fn forget(&self, path: &Path) {
            self.now.forget(path)
        }
        fn entries(&self) -> Vec<crate::scratch::Entry> {
            self.now.entries()
        }
    }

    #[test]
    fn background_recovery_records_its_relink_temp_and_a_later_one_sweeps_it() {
        // #400: an interrupted update whose projection still links the old file. Recovery
        // re-points it through a relink temp, recorded first.
        use crate::scratch::{Journal, Role};
        let base = temp_base("settle_relink");
        let mods = base.join("mods");
        let layout = crate::organize::Layout::default_for(&mods);
        let cat = layout.archive.join("Misc");
        std::fs::create_dir_all(&cat).unwrap();
        let dest = cat.join("FS25_A.zip");
        let old = crate::update::old_path(&dest);
        std::fs::write(&old, b"PK old version").unwrap();
        let flat = mods.join("FS25_A.zip");
        std::fs::hard_link(&old, &flat).unwrap();
        std::fs::write(&dest, b"PK new version").unwrap();
        let journal = SpyJournal::default();

        super::settle_layout_with(&layout, Some(&journal));

        assert_eq!(std::fs::read(&flat).unwrap(), b"PK new version");
        let temps: Vec<_> = journal
            .seen
            .borrow()
            .iter()
            .filter(|e| e.role == Role::RelinkTmp)
            .cloned()
            .collect();
        assert_eq!(temps.len(), 1, "the relink temp was recorded");
        assert_eq!(temps[0].to, flat);
        assert!(
            !journal.entries().iter().any(|e| e.role == Role::RelinkTmp),
            "placed, so its record went"
        );

        // A crash between making the temp and renaming it in leaves it there, recorded.
        let tmp = temps[0].path.clone();
        std::fs::hard_link(&dest, &tmp).unwrap();
        journal.record(&temps[0]).unwrap();
        // An unrecorded look-alike beside it is never touched.
        let lookalike = crate::scratch::unique_beside(&flat, crate::scratch::RELINK_TMP);
        std::fs::write(&lookalike, b"not recorded").unwrap();

        super::settle_layout_with(&layout, Some(&journal));

        assert!(
            tmp.symlink_metadata().is_err(),
            "the next recovery sweeps it"
        );
        // (Recovery's backup isn't recorded either: its old version was known only by name.)
        assert!(journal.entries().is_empty(), "{:?}", journal.entries());
        assert_eq!(std::fs::read(&flat).unwrap(), b"PK new version");
        assert_eq!(std::fs::read(&dest).unwrap(), b"PK new version");
        assert_eq!(std::fs::read(&lookalike).unwrap(), b"not recorded");
        let _ = std::fs::remove_dir_all(&base);
    }

    fn temp_base(tag: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!("silo_lib_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("mods")).unwrap();
        base
    }

    fn game_settings(active: &str, dir: &str, port: &str) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\" standalone=\"no\" ?>\n<gameSettings \
             revision=\"15\">\n    <modsDirectoryOverride active=\"{active}\" \
             directory=\"{dir}\"/>\n    <defaultMultiplayerPort>{port}</defaultMultiplayerPort>\n\
             </gameSettings>\n"
        )
    }

    #[test]
    fn a_profile_folder_is_detected_and_its_choice_is_kept_per_folder() {
        let base = temp_base("gameprofile");
        let prof = base.join("FS25 Profile");
        let mods = prof.join("mods");
        std::fs::create_dir_all(&mods).unwrap();
        let conn = crate::db::open(&base.join("silo.db")).unwrap();
        let main = base.join("main");

        // A bare folder named `mods` can be a profile's, but isn't detected yet: off.
        let st = super::game_profile_for(&conn, &mods, Some(&main)).unwrap();
        assert_eq!(PathBuf::from(&st.dir), prof);
        assert!(!st.detected && !st.enabled && !st.chosen);

        // FS25 has used it (gameSettings.xml): on by itself where supported.
        std::fs::write(
            prof.join("gameSettings.xml"),
            game_settings("false", "", "1"),
        )
        .unwrap();
        let st = super::game_profile_for(&conn, &mods, Some(&main)).unwrap();
        assert!(st.detected && !st.chosen);
        assert_eq!(st.enabled, crate::gameprofile::SUPPORTED);
        // Where it isn't supported, the status says why (the Settings note shows it).
        assert_eq!(st.supported, crate::gameprofile::SUPPORTED);
        assert_eq!(
            st.unsupported_reason.as_deref(),
            crate::gameprofile::UNSUPPORTED
        );

        // Turned off for this folder: stays off though it's detected.
        let key = crate::gameprofile::setting_key(&crate::modsroot::canonical_key(&mods));
        crate::db::set_app_setting(&conn, &key, Some(crate::gameprofile::OFF)).unwrap();
        let st = super::game_profile_for(&conn, &mods, Some(&main)).unwrap();
        assert!(st.detected && st.chosen && !st.enabled);

        // The main user folder's own mods folder is never a profile's.
        assert_eq!(super::game_profile_for(&conn, &mods, Some(&prof)), None);
        // Nor a folder not named `mods`.
        let other = prof.join("more mods");
        std::fs::create_dir_all(&other).unwrap();
        assert_eq!(super::game_profile_for(&conn, &other, Some(&main)), None);
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_profile_reads_its_own_mods_folder_unless_its_setting_points_elsewhere() {
        let base = temp_base("gameprofile_reads");
        let prof = base.join("prof");
        std::fs::create_dir_all(prof.join("mods")).unwrap();
        // No settings yet, or the override is off: `<profile>/mods`.
        assert_eq!(super::profile_game_reads(&prof), Ok(prof.join("mods")));
        let elsewhere = base.join("elsewhere");
        let dir = elsewhere.to_string_lossy().replace('\\', "/");
        std::fs::write(
            prof.join("gameSettings.xml"),
            game_settings("false", &dir, "1"),
        )
        .unwrap();
        assert_eq!(super::profile_game_reads(&prof), Ok(prof.join("mods")));
        // The profile's own override is on: that folder.
        std::fs::write(
            prof.join("gameSettings.xml"),
            game_settings("true", &dir, "1"),
        )
        .unwrap();
        assert!(crate::modsroot::same_folder(
            &super::profile_game_reads(&prof).unwrap(),
            &elsewhere
        ));
        // An override that isn't a full path can't be placed: unknown.
        for odd in ["", "mods", "../other", "C:mods"] {
            std::fs::write(
                prof.join("gameSettings.xml"),
                game_settings("true", odd, "1"),
            )
            .unwrap();
            assert!(super::profile_game_reads(&prof).is_err(), "{odd:?}");
        }
        // Turned on without naming a folder: unknown too. Off without one: the default.
        let half = |active: &str| {
            format!("<gameSettings><modsDirectoryOverride active=\"{active}\"/></gameSettings>")
        };
        std::fs::write(prof.join("gameSettings.xml"), half("true")).unwrap();
        assert!(super::profile_game_reads(&prof).is_err());
        std::fs::write(prof.join("gameSettings.xml"), half("false")).unwrap();
        assert_eq!(super::profile_game_reads(&prof), Ok(prof.join("mods")));
        // Empty, or cut off before or after the override: damaged, so unknown.
        let whole = game_settings("true", &dir, "1");
        let cut = whole.find("<modsDirectoryOverride").unwrap();
        for damaged in [
            String::new(),
            whole[..cut].to_string(),
            whole[..whole.len() - 20].to_string(),
        ] {
            std::fs::write(prof.join("gameSettings.xml"), &damaged).unwrap();
            assert!(super::profile_game_reads(&prof).is_err(), "{damaged:?}");
        }
        // Settings that are there but can't be read: unknown, not "its own mods folder".
        std::fs::remove_file(prof.join("gameSettings.xml")).unwrap();
        std::fs::create_dir_all(prof.join("gameSettings.xml")).unwrap();
        assert!(super::profile_game_reads(&prof).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_profile_whose_settings_cant_be_read_blocks_the_folder() {
        // Where FS25 reads can't be told, so it can't be fenced off: nothing is written there.
        let base = temp_base("gameprofile_unreadable");
        let prof = base.join("prof");
        let mods = prof.join("mods");
        std::fs::create_dir_all(&mods).unwrap();
        std::fs::write(prof.join("game.xml"), "<game/>").unwrap();
        std::fs::create_dir_all(prof.join("gameSettings.xml")).unwrap();
        let conn = crate::db::open(&base.join("silo.db")).unwrap();
        let path = mods.to_string_lossy().into_owned();
        crate::db::set_app_setting(&conn, crate::modsroot::CHOICE_KEY, Some(&path)).unwrap();
        // Described only (no publish): reads the main user folder, writes nothing there.
        let st = super::resolve_mods_root(&conn, false);
        // Only judged where Silo launches profiles; elsewhere the folder is managed as before.
        let want = crate::gameprofile::SUPPORTED.then_some(crate::modsroot::BlockCause::ReadError);
        assert_eq!(st.layout_problem_cause, want);
        // Still a profile's folder everywhere: the main game setting is left alone.
        assert!(!st.can_write_game_setting);
        assert!(st.game_profile.is_some_and(|g| g.detected));
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_fresh_profile_turned_on_then_off_keeps_the_main_settings_protected() {
        // What `set_game_profile` stores: on (remembered as a profile), later off.
        let base = temp_base("gameprofile_fresh");
        let mods = base.join("prof").join("mods");
        std::fs::create_dir_all(&mods).unwrap();
        let conn = crate::db::open(&base.join("silo.db")).unwrap();
        let key = crate::modsroot::canonical_key(&mods);
        crate::db::set_app_setting(&conn, &crate::gameprofile::seen_key(&key), Some("on")).unwrap();
        crate::db::set_app_setting(&conn, &crate::gameprofile::setting_key(&key), Some("off"))
            .unwrap();
        let st = super::game_profile_for(&conn, &mods, Some(&base.join("main"))).unwrap();
        assert!(!st.enabled && st.chosen);
        assert!(crate::gameprofile::belongs(&st));
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The profile was used by its real path first (mark and an explicit "off" stored there),
    /// then picked through a junction to it, then the drive goes: the junction spelling got both.
    #[cfg(windows)]
    #[test]
    fn a_profile_seen_by_its_real_path_first_stays_one_through_a_junction() {
        let base = temp_base("gameprofile_junction_order");
        let target = base.join("drive").join("Farm");
        std::fs::create_dir_all(target.join("mods")).unwrap();
        std::fs::write(target.join("gameSettings.xml"), "<gameSettings/>").unwrap();
        let conn = crate::db::open(&base.join("silo.db")).unwrap();
        let main = base.join("main");
        let real = target.join("mods");
        assert!(super::game_profile_for(&conn, &real, Some(&main)).is_some_and(|g| g.detected));
        for k in super::profile_setting_keys(&real) {
            crate::db::set_app_setting(&conn, &crate::gameprofile::setting_key(&k), Some("off"))
                .unwrap();
        }
        let link = base.join("Farm");
        if crate::organize::junction_to(&target, &link).is_err() {
            eprintln!("no junctions here (not NTFS?): skipped");
            let _ = std::fs::remove_dir_all(&base);
            return;
        }
        let mods = link.join("mods");
        // Seen through the junction once: the mark and the choice are copied to its spelling.
        let st = super::game_profile_for(&conn, &mods, Some(&main)).unwrap();
        assert!(st.detected && st.chosen && !st.enabled);
        std::fs::remove_dir_all(base.join("drive")).unwrap();
        let st = super::game_profile_for(&conn, &mods, Some(&main)).unwrap();
        assert!(st.detected && st.chosen && !st.enabled, "{st:?}");
        drop(conn);
        let _ = std::fs::remove_dir(&link);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_relative_mods_folder_is_never_a_profile() {
        let conn = crate::db::open(&temp_base("gameprofile_relative").join("silo.db")).unwrap();
        assert_eq!(
            super::game_profile_for(&conn, std::path::Path::new("profiles/Farm/mods"), None),
            None
        );
    }

    #[test]
    fn a_busy_refresh_only_describes_the_folder_and_profile_in_use() {
        use std::path::Path;
        let a = Path::new("/f/a");
        let am = "/f/a/mods";
        // Same folder and profile: fine.
        assert!(!super::status_differs(
            Some(am),
            Some(Path::new(am)),
            Some(a),
            Some(a)
        ));
        // FS25's setting moved Silo to another folder meanwhile: refused.
        assert!(super::status_differs(
            Some("/f/b/mods"),
            Some(Path::new(am)),
            Some(Path::new("/f/b")),
            Some(a)
        ));
        // Same folder, but the published profile isn't this one's: refused.
        assert!(super::status_differs(
            Some(am),
            Some(Path::new(am)),
            None,
            Some(a)
        ));
        // Nothing published to differ from (no usable folder, no profile): fine.
        assert!(!super::status_differs(Some(am), None, Some(a), None));
    }

    /// The profile is reached through a junction whose target goes away (a removable drive
    /// behind `C:\Profiles\Farm`): its identity key changes, but its choice and mark are still
    /// found under the folder's own spelling.
    #[cfg(windows)]
    #[test]
    fn a_profile_behind_a_junction_whose_target_is_gone_stays_a_profile() {
        let base = temp_base("gameprofile_junction");
        let target = base.join("drive").join("Farm");
        std::fs::create_dir_all(target.join("mods")).unwrap();
        std::fs::write(target.join("gameSettings.xml"), "<gameSettings/>").unwrap();
        let link = base.join("Farm");
        if crate::organize::junction_to(&target, &link).is_err() {
            eprintln!("no junctions here (not NTFS?): skipped");
            let _ = std::fs::remove_dir_all(&base);
            return;
        }
        let mods = link.join("mods");
        let conn = crate::db::open(&base.join("silo.db")).unwrap();
        let main = base.join("main");
        assert!(super::game_profile_for(&conn, &mods, Some(&main)).is_some_and(|g| g.detected));
        // The user turns the launch off (stored under every key), then the drive goes.
        for k in super::profile_setting_keys(&mods) {
            crate::db::set_app_setting(&conn, &crate::gameprofile::setting_key(&k), Some("off"))
                .unwrap();
        }
        std::fs::remove_dir_all(base.join("drive")).unwrap();
        let st = super::game_profile_for(&conn, &mods, Some(&main)).unwrap();
        assert!(st.detected && st.chosen && !st.enabled, "{st:?}");
        assert!(crate::gameprofile::belongs(&st));
        drop(conn);
        let _ = std::fs::remove_dir(&link);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_profile_on_an_unplugged_drive_stays_a_profile() {
        let base = temp_base("gameprofile_unplugged");
        let prof = base.join("prof");
        let mods = prof.join("mods");
        std::fs::create_dir_all(&mods).unwrap();
        std::fs::write(
            prof.join("gameSettings.xml"),
            game_settings("false", "", "1"),
        )
        .unwrap();
        let conn = crate::db::open(&base.join("silo.db")).unwrap();
        let main = base.join("main");
        assert!(super::game_profile_for(&conn, &mods, Some(&main)).is_some_and(|g| g.detected));
        // The drive goes away: still the profile (Play refuses, nothing falls back to main).
        std::fs::remove_dir_all(&prof).unwrap();
        let st = super::game_profile_for(&conn, &mods, Some(&main)).unwrap();
        assert!(st.detected);
        assert_eq!(st.enabled, crate::gameprofile::SUPPORTED);
        // Back, but its settings files are gone (or can't be seen right now): still a profile.
        std::fs::create_dir_all(&mods).unwrap();
        assert!(super::game_profile_for(&conn, &mods, Some(&main)).is_some_and(|g| g.detected));
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn marker_folders_named_like_the_settings_files_dont_make_a_profile() {
        let base = temp_base("gameprofile_dirs");
        let prof = base.join("prof");
        std::fs::create_dir_all(prof.join("mods")).unwrap();
        std::fs::create_dir_all(prof.join("game.xml")).unwrap();
        std::fs::create_dir_all(prof.join("gameSettings.xml")).unwrap();
        let conn = crate::db::open(&base.join("silo.db")).unwrap();
        let st =
            super::game_profile_for(&conn, &prof.join("mods"), Some(&base.join("main"))).unwrap();
        assert!(!st.detected && !st.enabled);
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_profile_overriding_into_another_folders_archive_blocks_the_layout() {
        // The folder FS25 reads on the profile counts as a mods folder for the overlap checks.
        let base = temp_base("gameprofile_overlap");
        let mods = base.join("prof").join("mods");
        std::fs::create_dir_all(&mods).unwrap();
        let conn = crate::db::open(&base.join("silo.db")).unwrap();
        let into_archive = mods.join("archive");
        assert!(super::resolve_layout(&conn, &mods, None, None, None, true).is_ok());
        // The usual case: the profile reads this very folder.
        assert!(super::resolve_layout(&conn, &mods, None, None, Some(&mods), true).is_ok());
        let b =
            super::resolve_layout(&conn, &mods, None, None, Some(&into_archive), true).unwrap_err();
        assert_eq!(b.cause, crate::modsroot::BlockCause::Overlap);
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_log_request_names_the_main_log_or_the_profiles_and_nothing_else() {
        let main = PathBuf::from("/docs/FarmingSimulator2025");
        let prof = |launch| crate::gameprofile::Published {
            dir: PathBuf::from("/f/prof"),
            launch,
            reads: None,
        };
        let t = |req: Option<&str>, p: Option<crate::gameprofile::Published>| {
            super::log_target(req, Some(main.clone()), p)
        };
        // Current log: the profile's while its launch is on, else the main one.
        assert_eq!(
            t(None, Some(prof(true))),
            Ok(Some(PathBuf::from("/f/prof/log.txt")))
        );
        assert_eq!(t(None, Some(prof(false))), Ok(Some(main.join("log.txt"))));
        assert_eq!(t(None, None), Ok(Some(main.join("log.txt"))));
        assert_eq!(super::log_target(None, None, None), Ok(None));
        // A named log is kept as named, even after the profile changed.
        let main_log = main.join("log.txt");
        let main_log = main_log.to_string_lossy();
        assert_eq!(
            t(Some(&main_log), Some(prof(true))),
            Ok(Some(main.join("log.txt")))
        );
        assert_eq!(
            t(Some("/f/prof/log.txt"), Some(prof(false))),
            Ok(Some(PathBuf::from("/f/prof/log.txt")))
        );
        // Anything else is refused.
        assert!(t(Some("/f/prof/gameSettings.xml"), Some(prof(true))).is_err());
        assert!(t(Some("/other/log.txt"), Some(prof(true))).is_err());
        assert!(t(Some("/f/prof/log.txt"), None).is_err());
    }

    #[test]
    fn each_log_has_its_own_last_seen_marker() {
        let main = std::path::Path::new("/docs/FarmingSimulator2025");
        assert_eq!(
            super::log_seen_key_for(Some(&main.join("log.txt")), Some(main)),
            crate::logscan::LAST_SEEN_KEY
        );
        assert_eq!(
            super::log_seen_key_for(None, Some(main)),
            crate::logscan::LAST_SEEN_KEY
        );
        let prof =
            super::log_seen_key_for(Some(std::path::Path::new("/f/prof/log.txt")), Some(main));
        assert_ne!(prof, crate::logscan::LAST_SEEN_KEY);
        assert!(prof.starts_with(crate::logscan::LAST_SEEN_KEY));
    }

    /// Play looks at the published mods folder's move (a blocked folder's too), from the DB.
    #[test]
    fn plays_check_reads_the_mods_folders_own_move_from_the_db() {
        let base = std::env::temp_dir().join(format!("silo_play_gate_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let mods = base.join("mods");
        std::fs::create_dir_all(&mods).unwrap();
        let active = crate::modsroot::Active {
            layout: crate::organize::Layout {
                mods: mods.clone(),
                archive: mods.join("archive"),
            },
            move_to: None,
        };
        let blocked = crate::modsroot::Blocked {
            reason: "blocked".into(),
            cause: crate::modsroot::BlockCause::Overlap,
            mods: base.join("other"),
            conflict: None,
            moving: None,
        };
        assert_eq!(super::launch_mods_folder(None, None), None);
        assert_eq!(
            super::launch_mods_folder(Some(active.clone()), Some(blocked.clone())),
            Some(mods.clone())
        );
        assert_eq!(
            super::launch_mods_folder(None, Some(blocked)),
            Some(base.join("other"))
        );
        let conn = crate::db::open(&base.join("silo.db")).unwrap();
        let key = crate::modsroot::canonical_key(&mods);
        let row = crate::db::OrganizedRow {
            tech_name: "FS25_A".into(),
            file_name: "FS25_A.zip".into(),
            kind: "zip".into(),
            category: "Tools".into(),
            subcategory: None,
            active: true,
        };
        crate::db::upsert_organized(&conn, &key, &row).unwrap();
        // No move: nothing to name, even with the entry missing.
        assert_eq!(super::unreachable_in(&conn, &mods), Ok(vec![]));
        let mut m = crate::relocate::MoveMarker::new(mods.join("archive"), base.join("hdd"));
        m.paused = true;
        m.moved.push("FS25_A".into());
        crate::relocate::save_marker(&conn, &key, &m).unwrap();
        assert_eq!(
            super::unreachable_in(&conn, &mods),
            Ok(vec!["FS25_A".to_string()])
        );
        std::fs::write(mods.join("FS25_A.zip"), b"PK").unwrap();
        assert_eq!(super::unreachable_in(&conn, &mods), Ok(vec![]));
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A move record or library row Silo can't read is "couldn't check", never "no move".
    #[test]
    fn plays_check_reports_a_record_it_cant_read() {
        let base = std::env::temp_dir().join(format!("silo_play_gate_bad_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let mods = base.join("mods");
        std::fs::create_dir_all(&mods).unwrap();
        let conn = crate::db::open(&base.join("silo.db")).unwrap();
        let key = crate::modsroot::canonical_key(&mods);
        crate::db::set_app_setting(&conn, &format!("archive_move:{key}"), Some("{not json"))
            .unwrap();
        assert!(super::unreachable_in(&conn, &mods).is_err());
        let m = crate::relocate::MoveMarker::new(mods.join("archive"), base.join("hdd"));
        crate::relocate::save_marker(&conn, &key, &m).unwrap();
        assert_eq!(super::unreachable_in(&conn, &mods), Ok(vec![]));
        conn.execute(
            "INSERT INTO organized(root, tech_name, file_name, kind, category, subcategory, active)
             VALUES(?1, 'FS25_A', 'FS25_A.zip', 'zip', 'Tools', NULL, 'not a number')",
            [&key],
        )
        .unwrap();
        assert!(super::unreachable_in(&conn, &mods).is_err());
        drop(conn);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The folder checked is the one the started game reads: a launching profile's, else the
    /// main profile's; only when Silo couldn't read that, the folder it manages.
    #[test]
    fn plays_check_looks_at_the_folder_the_game_will_read() {
        let p = |launch, reads: Option<&str>| crate::gameprofile::Published {
            dir: PathBuf::from("/prof"),
            launch,
            reads: reads.map(PathBuf::from),
        };
        let (main, managed) = (Some(PathBuf::from("/main/mods")), Some(PathBuf::from("/b")));
        let reads = super::launch_reads;
        assert_eq!(
            reads(
                Some(p(true, Some("/prof/mods"))),
                main.clone(),
                managed.clone()
            ),
            Some(PathBuf::from("/prof/mods"))
        );
        assert_eq!(
            reads(Some(p(true, None)), main.clone(), managed.clone()),
            managed
        );
        assert_eq!(
            reads(
                Some(p(false, Some("/prof/mods"))),
                main.clone(),
                managed.clone()
            ),
            main
        );
        assert_eq!(reads(None, main.clone(), managed.clone()), main);
        assert_eq!(reads(None, None, managed.clone()), managed);
        assert_eq!(reads(None, None, None), None);
    }

    /// The launch gate (#415): a Play and a move never overlap, whatever the order.
    #[test]
    fn a_play_and_an_archive_move_are_never_admitted_together() {
        use super::{LaunchGate, LAUNCH_GRACE_MS as G};
        let mut g = LaunchGate::default();
        assert!(!g.launching(0));
        // A move admitted first: Play is refused and publishes nothing a move could see.
        assert!(!g.begin_launch(true));
        assert_eq!(g, LaunchGate::default());
        // Play in flight: no move, however long its checks take (no deadline yet).
        assert!(g.begin_launch(false));
        assert!(g.launching(10 * G));
        // Refused (nothing started): moves are admitted again at once.
        g.end_launch(false, 10 * G);
        assert!(!g.launching(10 * G));
        // Started: no move for the grace from the start, not from when Play was pressed.
        assert!(g.begin_launch(false));
        g.end_launch(true, 100);
        assert!(g.launching(100 + G - 1));
        assert!(!g.launching(100 + G));
        // Play again moments later, refused (the game is still starting): the first start's
        // deadline stands.
        assert!(g.begin_launch(false));
        g.end_launch(false, 200);
        assert!(g.launching(100 + G - 1), "{g:?}");
        assert!(!g.launching(100 + G));
        // A clock read from another thread a moment before the start never underflows.
        assert!(g.launching(50));
    }

    #[test]
    fn play_starts_only_the_profile_the_app_shows() {
        let p = std::path::Path::new("/f/prof");
        assert!(!super::launch_profile_mismatch(None, None));
        assert!(!super::launch_profile_mismatch(Some(p), Some("/f/prof")));
        // Shown main, would start a profile; shown a profile, would start main or another one.
        assert!(super::launch_profile_mismatch(Some(p), None));
        assert!(super::launch_profile_mismatch(None, Some("/f/prof")));
        assert!(super::launch_profile_mismatch(Some(p), Some("/f/other")));
    }

    /// Play's archive-move gate (#415): a running move refuses even "Play anyway"; a paused
    /// move that left mods FS25 can't open asks first, naming them, unless "Play anyway"; a
    /// paused move with nothing listed (or no move) plays as before.
    #[test]
    fn play_asks_first_while_a_paused_move_left_mods_unreachable() {
        use super::move_play_gate as gate;
        let none: Result<Vec<String>, String> = Ok(vec![]);
        let one = || Ok(vec!["FS25_A".to_string()]);
        for anyway in [false, true] {
            let e = gate(true, one(), anyway).unwrap_err();
            assert_eq!(e.code, super::error::code::MOVE_RUNNING, "{anyway}");
            assert_eq!(
                gate(true, none.clone(), anyway).unwrap_err().code,
                "MOVE_RUNNING"
            );
            assert_eq!(gate(false, none.clone(), anyway), Ok(()));
        }
        let e = gate(false, one(), false).unwrap_err();
        assert_eq!(e.code, super::error::code::MOVE_LINKS_BROKEN);
        assert!(e.message.contains("FS25_A"), "{}", e.message);
        assert!(e.message.contains("Resume"), "{}", e.message);
        assert_eq!(gate(false, one(), true), Ok(()));
        // Silo couldn't look: it asks too, rather than starting FS25 blind.
        let e = gate(false, Err("database is locked".into()), false).unwrap_err();
        assert_eq!(e.code, super::error::code::MOVE_LINKS_BROKEN);
        assert!(e.message.contains("database is locked"), "{}", e.message);
        assert_eq!(gate(false, Err("x".into()), true), Ok(()));
    }

    #[test]
    fn the_unreachable_mods_message_names_them_and_counts_the_rest() {
        let names = |n: usize| (1..=n).map(|i| format!("FS25_M{i:02}")).collect::<Vec<_>>();
        let one = super::unreachable_message(&names(1));
        assert!(one.contains("can't open FS25_M01 from"), "{one}");
        assert!(one.contains("without it"), "{one}");
        let two = super::unreachable_message(&names(2));
        assert!(two.contains("FS25_M01 and FS25_M02"), "{two}");
        assert!(two.contains("without them"), "{two}");
        let many = super::unreachable_message(&names(12));
        assert!(many.contains("12 mods"), "{many}");
        assert!(many.contains("FS25_M08"), "{many}");
        assert!(!many.contains("FS25_M09"), "{many}");
        assert!(many.contains("and 4 more"), "{many}");
    }

    #[test]
    fn pointing_fs25_at_a_folder_is_refused_while_a_profile_is_in_use() {
        assert_eq!(
            super::main_settings_refusal(Some(std::path::Path::new("/f/prof"))),
            Err(super::PROFILE_KEEPS_MAIN_SETTINGS.to_string())
        );
        assert_eq!(super::main_settings_refusal(None), Ok(()));
    }

    #[test]
    fn a_config_edit_can_not_move_the_mods_folder() {
        let old = game_settings("false", "C:/Temp", "10823");
        assert_eq!(
            super::override_edit_refusal(&old, &game_settings("false", "D:/Elsewhere", "10823")),
            Err(super::OVERRIDE_IS_SILOS.to_string()),
            "changed directory"
        );
        assert_eq!(
            super::override_edit_refusal(&old, &game_settings("true", "C:/Temp", "10823")),
            Err(super::OVERRIDE_IS_SILOS.to_string()),
            "changed active"
        );
        assert_eq!(
            super::override_edit_refusal(&old, &game_settings("false", "C:/Temp", "10900")),
            Ok(()),
            "an unrelated edit"
        );
        let plain =
            "<gameSettings><defaultMultiplayerPort>1</defaultMultiplayerPort></gameSettings>";
        let plain2 =
            "<gameSettings><defaultMultiplayerPort>2</defaultMultiplayerPort></gameSettings>";
        assert_eq!(
            super::override_edit_refusal(plain, plain2),
            Ok(()),
            "no override element"
        );
        assert_eq!(
            super::override_edit_refusal(plain, &old),
            Err(super::OVERRIDE_IS_SILOS.to_string()),
            "adding the element"
        );
    }

    #[test]
    fn only_game_settings_writes_are_checked_for_the_override() {
        let base = temp_base("override_kept");
        let gs = base.join("gameSettings.xml");
        std::fs::write(&gs, game_settings("false", "C:/Temp", "1")).unwrap();
        let moved = game_settings("true", "D:/Elsewhere", "1");
        assert_eq!(
            super::ensure_override_kept(&base, &gs, None, &moved),
            Err(super::OVERRIDE_IS_SILOS.to_string()),
            "the file is read when the caller hasn't"
        );
        let other = base.join("modSettings").join("FS25_X.xml");
        assert_eq!(
            super::ensure_override_kept(&base, &other, None, &moved),
            Ok(())
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn only_known_folders_that_exist_can_be_opened() {
        let base = temp_base("known_folder");
        std::fs::create_dir_all(base.join("mods")).unwrap();
        assert_eq!(
            super::existing_dir(Some(base.join("mods"))).unwrap(),
            base.join("mods")
        );
        assert_eq!(
            super::existing_dir(Some(base.join("gone"))).unwrap_err(),
            super::FOLDER_MISSING
        );
        assert_eq!(
            super::existing_dir(None).unwrap_err(),
            super::FOLDER_MISSING
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_logs_folder_is_the_app_log_dir_when_it_exists() {
        let base = temp_base("known_folder_logs");
        let logs = base.join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        assert_eq!(
            super::known_folder_path(super::KnownFolder::Logs, Some(logs.clone())).unwrap(),
            logs
        );
        assert_eq!(
            super::known_folder_path(super::KnownFolder::Logs, Some(base.join("none")))
                .unwrap_err(),
            super::FOLDER_MISSING
        );
        assert_eq!(
            super::known_folder_path(super::KnownFolder::Logs, None).unwrap_err(),
            super::FOLDER_MISSING
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn an_update_only_replaces_a_mod_zip_in_the_library() {
        let base = temp_base("update_target");
        let mods = base.join("mods");
        let layout = Layout::default_for(&mods);
        let cat = layout.archive.join("Vehicles");
        for d in [&mods, &cat, &mods.join("backups"), &mods.join("sub")] {
            std::fs::create_dir_all(d).unwrap();
        }
        let ok = [mods.join("FS25_A.zip"), cat.join("FS25_B.zip")];
        let bad = [
            mods.join("backups").join("FS25_A.zip"),
            mods.join("FS25_A.txt"),
            layout.archive.join("FS25_C.zip"),
            mods.join("sub").join("FS25_D.zip"),
        ];
        for p in ok.iter().chain(&bad) {
            std::fs::write(p, b"zip").unwrap();
        }
        for p in &ok {
            assert_eq!(super::update_target_problem(p, &layout), None, "{p:?}");
        }
        for p in &bad {
            assert!(super::update_target_problem(p, &layout).is_some(), "{p:?}");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn an_update_can_not_reach_a_backup_through_a_linked_category() {
        let base = temp_base("update_target_link");
        let mods = base.join("mods");
        let layout = Layout::default_for(&mods);
        let backups = mods.join("backups");
        std::fs::create_dir_all(&backups).unwrap();
        std::fs::create_dir_all(&layout.archive).unwrap();
        std::fs::write(backups.join("FS25_A.zip"), b"zip").unwrap();
        // A "category" that is a junction (Windows) or symlink (unix) into `mods/backups`.
        let linked = layout.archive.join("Linked");
        crate::organize::link_dir_native(&backups, &linked).unwrap();
        let dest = linked.join("FS25_A.zip");
        assert!(dest.is_file());
        assert!(
            super::update_target_problem(&dest, &layout).is_some(),
            "the category leads into backups"
        );
        let _ = std::fs::remove_dir(&linked);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_scan_never_forgets_the_library_while_a_custom_archive_is_missing() {
        let base = temp_base("scanmissing");
        let conn = crate::db::open(&base.join("silo.db")).unwrap();
        let mods = base.join("mods");
        let gone = base.join("unplugged");
        let active = crate::modsroot::Active {
            layout: Layout {
                mods: mods.clone(),
                archive: gone.clone(),
            },
            move_to: None,
        };
        let (archives, reconcile) = scan_archives(&conn, &mods, Some(&active));
        assert_eq!(archives, vec![gone.clone()]);
        assert!(
            !reconcile,
            "a missing custom archive must not drop organized rows"
        );
        // Plugged back in: reconcile as usual.
        std::fs::create_dir_all(&gone).unwrap();
        assert!(scan_archives(&conn, &mods, Some(&active)).1);
        // The default archive may simply not exist yet — that's an empty library, not a
        // missing drive.
        let default = crate::modsroot::Active {
            layout: Layout::default_for(&mods),
            move_to: None,
        };
        assert!(scan_archives(&conn, &mods, Some(&default)).1);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_lost_database_finds_a_custom_archive_from_its_notes() {
        let base = temp_base("rediscover");
        let (mods, hdd) = (base.join("mods"), base.join("hdd"));
        std::fs::create_dir_all(&hdd).unwrap();
        crate::relocate::note_archive(&mods, Some(&hdd));
        // A brand-new database (in its own data folder): it knows nothing about the archive.
        let conn = crate::db::open(&base.join("appdata").join("silo.db")).unwrap();
        assert_eq!(super::resolve_active(&conn, &mods).layout.archive, hdd);
        let key = crate::modsroot::canonical_key(&mods);
        assert_eq!(
            crate::db::get_app_setting(&conn, &crate::relocate::archive_setting_key(&key)),
            Some(hdd.to_string_lossy().into_owned()),
            "stored again, so the next start doesn't need the notes"
        );
        let notices = crate::notices::take_mentioning(&hdd.display().to_string());
        assert!(
            notices
                .iter()
                .any(|n| n.contains("found your library archive")),
            "{notices:?}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_rediscovered_archive_must_clear_the_fences() {
        let base = temp_base("rediscover_fenced");
        let mods = base.join("mods");
        let app_data = base.join("appdata");
        let inside_app_data = app_data.join("archive");
        std::fs::create_dir_all(&inside_app_data).unwrap();
        crate::relocate::note_archive(&mods, Some(&inside_app_data));
        let conn = crate::db::open(&app_data.join("silo.db")).unwrap();
        assert_eq!(
            super::resolve_active(&conn, &mods).layout.archive,
            crate::organize::Layout::default_for(&mods).archive,
            "an archive inside Silo's own data folder isn't taken up"
        );
        let key = crate::modsroot::canonical_key(&mods);
        assert_eq!(
            crate::db::get_app_setting(&conn, &crate::relocate::archive_setting_key(&key)),
            None
        );
        assert!(crate::notices::take_mentioning(&inside_app_data.display().to_string()).is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_scan_of_another_folder_reads_its_stored_archive() {
        let base = temp_base("scanother");
        let conn = crate::db::open(&base.join("silo.db")).unwrap();
        let mods = base.join("mods");
        let other = base.join("other-mods");
        let hdd = base.join("hdd");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::create_dir_all(&hdd).unwrap();
        let key = crate::modsroot::canonical_key(&other);
        crate::relocate::store_archive_setting(&conn, &key, &other, &hdd).unwrap();
        let active = crate::modsroot::Active {
            layout: Layout::default_for(&mods),
            move_to: None,
        };
        let (archives, reconcile) = scan_archives(&conn, &other, Some(&active));
        assert_eq!(archives, vec![hdd]);
        assert!(reconcile);
        // No stored archive → that folder's default.
        let third = base.join("third");
        std::fs::create_dir_all(&third).unwrap();
        assert_eq!(
            scan_archives(&conn, &third, Some(&active)).0,
            vec![third.join("archive")]
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_scan_mid_move_reads_both_archives_and_never_reconciles() {
        let base = temp_base("scanmove");
        let conn = crate::db::open(&base.join("silo.db")).unwrap();
        let mods = base.join("mods");
        let key = crate::modsroot::canonical_key(&mods);
        let m = MoveMarker::new(mods.join("archive"), base.join("hdd"));
        crate::relocate::save_marker(&conn, &key, &m).unwrap();
        let (archives, reconcile) = scan_archives(&conn, &mods, None);
        assert_eq!(archives, vec![base.join("hdd"), mods.join("archive")]);
        assert!(!reconcile);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn repoint_is_for_a_missing_or_empty_archive_with_no_move() {
        let base = temp_base("repointgate");
        let mods = base.join("mods");
        let gone = Layout {
            mods: mods.clone(),
            archive: base.join("unplugged"),
        };
        assert_eq!(repoint_refusal(&gone, false, false), None);
        assert!(
            repoint_refusal(&gone, true, false).is_some(),
            "a move is unfinished"
        );
        let here = Layout {
            mods: mods.clone(),
            archive: base.join("hdd"),
        };
        std::fs::create_dir_all(&here.archive).unwrap();
        assert!(
            repoint_refusal(&here, false, true).is_some(),
            "an archive holding mods is moved, not re-pointed"
        );
        // Holding no mods, nothing can be orphaned: re-pointing is fine (the way back after
        // starting fresh with the default).
        assert_eq!(repoint_refusal(&here, false, false), None);
        assert_eq!(
            repoint_refusal(&Layout::default_for(&mods), false, false),
            None
        );
        assert!(repoint_refusal(&here, true, false).is_some());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn an_archive_holds_mods_only_when_a_category_has_one() {
        let base = temp_base("holdsmods");
        let layout = Layout::default_for(&base.join("mods"));
        assert!(!archive_holds_mods(&layout), "not created yet");
        let tools = layout.archive.join("Tools");
        std::fs::create_dir_all(&tools).unwrap();
        std::fs::write(tools.join(".FS25_A.zip.silo-tmp"), b"PK").unwrap();
        std::fs::write(tools.join("FS25_B.zip.silo-move-tmp"), b"PK").unwrap();
        std::fs::write(tools.join("notes.txt"), b"mine").unwrap();
        assert!(
            !archive_holds_mods(&layout),
            "Silo temps and other files don't count"
        );
        std::fs::write(tools.join("FS25_C.zip"), b"PK").unwrap();
        assert!(archive_holds_mods(&layout));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn an_unreadable_archive_holds_mods_and_is_never_reconciled() {
        let base = temp_base("unreadable");
        let conn = crate::db::open(&base.join("silo.db")).unwrap();
        let mods = base.join("mods");
        // Something exists at the default archive's path, but it can't be listed.
        std::fs::write(mods.join("archive"), b"not a folder").unwrap();
        let layout = Layout::default_for(&mods);
        assert!(
            archive_holds_mods(&layout),
            "unreadable ⇒ assume it holds mods (no re-point)"
        );
        let active = crate::modsroot::Active {
            layout,
            move_to: None,
        };
        let (_, reconcile) = scan_archives(&conn, &mods, Some(&active));
        assert!(
            !reconcile,
            "an unreadable archive must never wipe the manifest"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn repoint_fences_refuse_the_mods_folder_but_accept_a_filed_archive() {
        let base = temp_base("repointfence");
        let mods = base.join("mods");
        let restored = base.join("restored");
        std::fs::create_dir_all(restored.join("Tools")).unwrap();
        std::fs::write(restored.join("Tools/FS25_A.zip"), b"PK").unwrap();
        let layout = Layout {
            mods: mods.clone(),
            archive: base.join("unplugged"),
        };
        let f = Fences {
            mods: super::key_path(&mods),
            current_archive: super::key_path(&layout.archive),
            other_mods: vec![],
            other_archives: vec![],
            user_dir: None,
            profile_dirs: vec![],
            app_data: None,
        };
        // Mods filed in subfolders are the point of a re-point, not a problem.
        assert!(fence_problems(&layout, &restored, &f).is_empty());
        assert!(!fence_problems(&layout, &mods.join("sub"), &f).is_empty());
        assert!(!fence_problems(&layout, &mods, &f).is_empty());
        assert!(!fence_problems(&layout, &base, &f).is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn scope_is_read_only_by_default() {
        assert_eq!(build_gh_scope(false, false), "read:user");
    }

    #[test]
    fn scope_adds_capabilities_additively() {
        assert_eq!(build_gh_scope(true, false), "read:user public_repo");
        assert_eq!(build_gh_scope(false, true), "read:user gist");
        // The union — enabling one capability while holding the other keeps both.
        assert_eq!(build_gh_scope(true, true), "read:user public_repo gist");
    }

    #[test]
    fn scope_grants_reads_both_github_formats() {
        // Device-flow response: space-separated.
        assert!(scope_grants("read:user public_repo gist", "gist"));
        assert!(scope_grants("read:user public_repo gist", "public_repo"));
        assert!(!scope_grants("read:user", "gist"));
        // X-OAuth-Scopes header: comma-separated (with spaces).
        assert!(scope_grants("gist, public_repo, read:user", "public_repo"));
        // No partial matches.
        assert!(!scope_grants("read:user public", "public_repo"));
    }

    #[test]
    fn scope_star_sentinel_grants_everything() {
        // Fine-grained PATs report no scopes; "*" means assume capable.
        assert!(scope_grants("*", "gist"));
        assert!(scope_grants("*", "public_repo"));
    }

    #[test]
    fn a_stored_catalog_address_is_ignored_in_release_builds() {
        assert_eq!(
            super::effective_siloapi_base(Some("https://evil.example".into()), false),
            siloapi::DEFAULT_BASE
        );
        assert_eq!(
            super::effective_siloapi_base(Some("http://localhost:3000".into()), true),
            "http://localhost:3000"
        );
        assert_eq!(
            super::effective_siloapi_base(None, true),
            siloapi::DEFAULT_BASE
        );
    }

    #[test]
    fn use_this_version_and_keep_silos_copy_wait_for_the_game_to_close() {
        let e = super::swap_refusal(true).unwrap_err();
        assert_eq!(e, super::SWAP_GAME_RUNNING);
        assert_eq!(
            crate::error::AppError::from(e).code,
            crate::error::code::GAME_RUNNING
        );
        assert!(super::swap_refusal(false).is_ok());
    }

    #[test]
    fn fs25_settings_writes_wait_for_the_game_to_close() {
        assert_eq!(
            super::config_write_refusal(true).unwrap_err(),
            super::CONFIG_GAME_RUNNING
        );
        assert!(super::config_write_refusal(false).is_ok());
    }

    #[test]
    fn only_pure_commands_run_on_the_main_thread() {
        // A sync #[tauri::command] runs on the main thread: disk, DB, keychain or process-list
        // work there freezes the window ("Not Responding"). Only these touch nothing.
        const PURE: &[&str] = &[
            "bisect_plan",
            "bisect_narrow",
            "app_busy",
            "force_quit",
            "set_self_updating",
            "archive_move_pause",
            "app_version",
        ];
        let src = include_str!("lib.rs");
        let mut sync = Vec::new();
        let mut lines = src.lines();
        while let Some(line) = lines.next() {
            if line.trim() != "#[tauri::command]" {
                continue;
            }
            for next in lines.by_ref() {
                let t = next.trim();
                if t.starts_with("#[") {
                    continue; // another attribute
                }
                let t = t.strip_prefix("pub ").unwrap_or(t);
                if let Some(rest) = t.strip_prefix("fn ") {
                    sync.push(rest.split(['(', '<']).next().unwrap_or("").to_string());
                }
                break;
            }
        }
        let unexpected: Vec<&String> = sync
            .iter()
            .filter(|n| !PURE.contains(&n.as_str()))
            .collect();
        assert!(
            unexpected.is_empty(),
            "make these async and run their work with off_main: {unexpected:?}"
        );
    }

    /// WebView2's own autofill ("Saved info: …") pops up over Silo's text fields and offers
    /// values typed elsewhere; it doesn't always honor `autocomplete="off"`, so every window
    /// turns it off. Demo-profile windows are built from the same config (`build_profile_windows`).
    #[test]
    fn windows_turn_off_webview_autofill() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let windows = conf["app"]["windows"].as_array().unwrap();
        assert!(!windows.is_empty());
        for w in windows {
            assert_eq!(
                w["generalAutofillEnabled"],
                serde_json::Value::Bool(false),
                "window {:?} must set generalAutofillEnabled: false",
                w["title"]
            );
        }
    }
}

/// Collections and Multiplayer check reporting (#355).
#[cfg(test)]
mod collection_reporting_tests {
    use super::{
        merge_collection_listings, repo_upload_failed, unpinned_names, CollectionSummary, Listing,
    };
    use crate::collection::CollectionMod;

    fn summary(reference: &str, created_at: &str) -> CollectionSummary {
        CollectionSummary {
            kind: "gist".into(),
            reference: reference.into(),
            name: reference.into(),
            mod_count: 1,
            created_at: Some(created_at.into()),
            page_url: String::new(),
            source_url: String::new(),
            can_delete: true,
        }
    }

    fn found(items: Vec<CollectionSummary>) -> Result<Listing, String> {
        Ok(Listing {
            items,
            unreadable: 0,
        })
    }

    #[test]
    fn both_listings_failing_is_an_error_not_an_empty_list() {
        let r = merge_collection_listings(
            Err("GitHub rejected the token. Reconnect your account.".into()),
            Err("GitHub rejected the token. Reconnect your account.".into()),
        );
        assert!(r.err().is_some_and(|e| e.contains("Reconnect")));
    }

    #[test]
    fn one_listing_failing_gives_a_partial_list_and_a_warning() {
        let r = merge_collection_listings(
            found(vec![summary("g1", "2026-01-01")]),
            Err("rate limit".into()),
        )
        .unwrap();
        assert_eq!(r.items.len(), 1);
        assert!(r.warning.as_deref().unwrap().contains("rate limit"));

        let r = merge_collection_listings(
            Err("offline".into()),
            found(vec![summary("o/silo-x", "2026-01-01")]),
        )
        .unwrap();
        assert_eq!(r.items.len(), 1);
        assert!(r.warning.as_deref().unwrap().contains("offline"));
    }

    #[test]
    fn both_listings_merge_newest_first_with_no_warning() {
        let r = merge_collection_listings(
            found(vec![summary("old", "2026-01-01")]),
            found(vec![summary("new", "2026-02-01")]),
        )
        .unwrap();
        assert!(r.warning.is_none());
        let refs: Vec<&str> = r.items.iter().map(|c| c.reference.as_str()).collect();
        assert_eq!(refs, ["new", "old"]);
    }

    /// Both listings worked but a collection they found couldn't be fetched: a warning, so an
    /// empty result never reads as "nothing shared yet".
    #[test]
    fn a_collection_that_couldnt_be_read_is_a_warning() {
        let r = merge_collection_listings(
            Ok(Listing {
                items: vec![],
                unreadable: 1,
            }),
            found(vec![]),
        )
        .unwrap();
        assert!(r.items.is_empty());
        assert!(r.warning.as_deref().unwrap().contains("couldn't read 1"));
    }

    #[test]
    fn mods_without_a_fingerprint_are_named() {
        let m = |t: &str, h: Option<&str>| CollectionMod {
            tech_name: t.into(),
            version: None,
            source: None,
            source_url: None,
            manifest_hash: h.map(String::from),
            installable: None,
            catalog_id: None,
        };
        let entries = [m("FS25_A", Some("h")), m("FS25_B", None), m("FS25_C", None)];
        assert_eq!(unpinned_names(&entries), ["FS25_B", "FS25_C"]);
    }

    #[test]
    fn a_failed_first_upload_names_the_repo_it_made() {
        let msg = repo_upload_failed("https://github.com/me/silo-pack", "timed out");
        assert!(msg.contains("https://github.com/me/silo-pack"));
        assert!(msg.contains("Try again"));
        assert!(msg.contains("timed out"));
    }
}

/// Silo's own folders in the log, and known folders' mod counts (#385).
#[cfg(test)]
mod silo_folder_tests {
    use super::{known_mod_count, silo_folder_keys};

    /// Zips and folder mods both count, and an organized mod's projection counts once.
    #[test]
    fn known_folder_counts_folder_mods_and_projections_once() {
        let dir = std::env::temp_dir().join(format!("silo_known_count_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("FS25_Folder")).unwrap();
        std::fs::write(dir.join("FS25_Folder/modDesc.xml"), "<modDesc/>").unwrap();
        std::fs::create_dir_all(dir.join("NotAMod")).unwrap();
        std::fs::create_dir_all(dir.join("archive/Tools")).unwrap();
        std::fs::create_dir_all(dir.join("backups")).unwrap();
        for z in ["FS25_A.zip", "FS25_B.zip", "FS25_Parked.zip"] {
            std::fs::write(dir.join(z), b"PK").unwrap();
        }
        std::fs::write(dir.join("readme.txt"), b"x").unwrap();
        let names = crate::scan::root_mod_names(&dir);
        let row = |file: &str| crate::db::OrganizedRow {
            tech_name: file.trim_end_matches(".zip").into(),
            file_name: file.into(),
            kind: "zip".into(),
            category: "Tools".into(),
            subcategory: None,
            active: false,
        };
        // FS25_A is organized and active (its projection is in the folder); FS25_C is parked.
        let organized = [row("FS25_a.zip"), row("FS25_C.zip")];
        let n = known_mod_count(&organized, &names);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(names.len(), 4, "{names:?}");
        // A (organized), C (parked), B, Parked and the folder mod.
        assert_eq!(n, 5);
    }
    use crate::logscan::folder_key;
    use std::path::PathBuf;

    #[test]
    fn backups_always_and_the_archive_only_when_it_is_the_default() {
        let base = std::env::temp_dir().join(format!("silo_folder_keys_{}", std::process::id()));
        let (a, b) = (base.join("A"), base.join("B"));
        let custom = base.join("Elsewhere");
        let keys = silo_folder_keys(&[a.clone(), b.clone(), a.clone()], |m| {
            if m == a.as_path() {
                m.join("archive")
            } else {
                custom.clone()
            }
        });
        let key = |p: PathBuf| folder_key(&p.to_string_lossy());
        assert_eq!(
            keys,
            [
                key(a.join("archive")),
                key(a.join("backups")),
                key(b.join("backups"))
            ]
        );
    }
}

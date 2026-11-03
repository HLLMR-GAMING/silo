//! Ownership detection: whether a mods-folder entry is Silo's projection, and the scan for
//! foreign files sitting at organized mods' names.

use super::*;

/// True only if `link` in the flat root is provably the projection Silo created for archived
/// `src` — so removing it can't destroy a file the user put there themselves. Symlink /
/// junction resolves to `src`; a zip hardlink shares `src`'s file id; a cross-volume copy is
/// byte-identical to `src`; a copied dir carries our marker. Anything else is NOT ours.
pub(crate) fn is_silo_projection(link: &Path, src: &Path, kind: &str) -> bool {
    projection_verdict(link, src, kind).unwrap_or(false)
}

/// [`is_silo_projection`], telling "not Silo's" (`Ok(false)`) apart from "couldn't tell"
/// (`Err`: a file couldn't be inspected or read), so a caller can report the second as the
/// error it is instead of as a different build.
pub(crate) fn projection_verdict(link: &Path, src: &Path, kind: &str) -> std::io::Result<bool> {
    let meta = link.symlink_metadata()?;
    let ft = meta.file_type();

    if ft.is_symlink() {
        // Our dir symlink / Windows junction resolves to the archived source.
        return Ok(std::fs::canonicalize(link)? == std::fs::canonicalize(src)?);
    }

    if kind == "dir" || ft.is_dir() {
        // Copy-fallback directory projection carries our marker; a user's own folder won't.
        return match link.join(SILO_MARKER).metadata() {
            Ok(m) => Ok(m.is_file()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        };
    }

    // Regular file (zip). Same underlying file id → our hardlink (cheap, the common case).
    if let (Ok(a), Ok(b)) = (
        same_file::Handle::from_path(link),
        same_file::Handle::from_path(src),
    ) {
        if a == b {
            return Ok(true);
        }
    }
    // Distinct files: a cross-volume copy projection is byte-identical to the archive; a
    // user's replacement is not. Cheap size gate first, then a content hash.
    if std::fs::metadata(link)?.len() != std::fs::metadata(src)?.len() {
        return Ok(false);
    }
    let hash = |p: &Path| crate::provenance::sha256_file(p).map_err(std::io::Error::other);
    Ok(hash(link)? == hash(src)?)
}

/// A file in the flat root occupying an organized mod's managed name that ISN'T Silo's
/// projection — something the user (or another tool) placed there. Silo never deletes it, but
/// the user should know their intended mod isn't what the game will load from that name.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForeignFile {
    pub tech_name: String,
    pub file_name: String,
    pub kind: String, // "zip" | "dir"
    /// Version of the file currently in the mods folder (what "adopt" would keep).
    pub flat_version: Option<String>,
    /// Version of Silo's managed/archived copy (what "restore" would put back).
    pub managed_version: Option<String>,
    /// The entry is the one Silo linked; it's the copy in the library archive that changed since
    /// (#431). The choices are the same: keep the mods-folder copy, or put the library's in.
    pub archive_changed: bool,
    /// This check just noted the entry as not Silo's copy (new, or changed since the last note),
    /// so the scan before it still showed the library's copy there: a rescan shows the entry as
    /// itself (#436).
    pub newly_noted: bool,
}

/// Best-effort mod version from a mod's `modDesc.xml`, for showing which build is which.
fn mod_version(path: &Path, kind: &str) -> Option<String> {
    crate::scan::read_moddesc_xml(path, kind)
        .ok()
        .and_then(|xml| crate::moddesc::parse(&xml).version)
}

/// Scan the flat root for foreign/mismatched files sitting at organized mods' names. Cheap:
/// parked mods leave the flat root empty, so it only identity-checks the handful of entries
/// actually present against their archived source. Reuses [`is_silo_projection`], so a real
/// hardlink/symlink/copy projection is never flagged — only a file Silo can't prove it created.
pub fn detect_foreign_projections(conn: &Connection, layout: &Layout) -> Vec<ForeignFile> {
    detect_foreign_with(conn, layout, projection_verdict)
}

/// [`detect_foreign_projections`] with the ownership check (`projection_verdict`) injected, so
/// a read error is testable.
pub(super) fn detect_foreign_with(
    conn: &Connection,
    layout: &Layout,
    verdict: impl Fn(&Path, &Path, &str) -> std::io::Result<bool>,
) -> Vec<ForeignFile> {
    let root = &layout.mods;
    let key = crate::modsroot::canonical_key(root);
    let mut out = Vec::new();
    // Only organized mods count, each at its recorded archive path: a loose zip with no row is
    // an ordinary "unorganized" mod (Organize handles it), and an unmanaged file that happens to
    // sit in an archive folder is no one's counterpart. The manifest, not a directory listing,
    // decides which archived copy a name belongs to.
    for row in db::load_organized(conn, &key) {
        if row.file_name.eq_ignore_ascii_case(ARCHIVE)
            || row.file_name.eq_ignore_ascii_case("backups")
        {
            continue;
        }
        let link = root.join(&row.file_name);
        if link.symlink_metadata().is_err() {
            continue; // parked: nothing at the mod's name
        }
        let src = archive_path(&layout.archive, &row.category, &row.file_name);
        if src.symlink_metadata().is_err() {
            continue; // no archived copy to compare against (or to restore)
        }
        let kind = row.kind.as_str();
        // Fast path: provenance match → the entry is the one Silo linked, no hash. Then, for a zip,
        // the archived copy must still be the one it was linked from (#431): a library copy whose
        // size or time changed since, or that has no recorded stamp yet (a record from before
        // version 6), is hashed against the entry, and differs → reported. Else the careful check
        // (which hashes a copy). Either way, (re)record provenance (with the archived copy's
        // stamp) for a confirmed zip so the next scan, and any deactivate, is O(1) instead of
        // re-hashing gigabytes; and note an entry that isn't Silo's copy so the scan shows that
        // file (#436). A check that couldn't read a file proves nothing: no record, no note, no
        // report, and the next check tries again.
        let linked = matches_provenance(conn, &key, &link, &row.file_name);
        let library_same = linked
            && (kind != "zip"
                || db::projection_src_stats(conn, &key, &row.file_name)
                    .is_some_and(|was| file_stats(&src) == Some(was)));
        let ours = if library_same {
            true
        } else {
            match verdict(&link, &src, kind) {
                Ok(ours) => ours,
                Err(e) => {
                    log::warn!(
                        "[foreign] couldn't check {} against the library copy: {e}",
                        link.display()
                    );
                    continue;
                }
            }
        };
        if ours {
            if kind == "zip" {
                if let Some((size, mtime)) = file_stats(&link) {
                    db::record_projection(
                        conn,
                        &key,
                        &row.file_name,
                        size,
                        mtime,
                        file_stats(&src),
                    );
                }
            }
            continue;
        }
        let newly_noted = kind == "zip"
            && file_stats(&link).is_some_and(|(size, mtime)| {
                db::mark_foreign(conn, &key, &row.file_name, size, mtime)
            });
        out.push(ForeignFile {
            flat_version: mod_version(&link, kind),
            managed_version: mod_version(&src, kind),
            tech_name: row.tech_name,
            file_name: row.file_name,
            kind: row.kind,
            archive_changed: linked,
            newly_noted,
        });
    }
    out
}

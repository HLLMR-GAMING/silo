//! The organize / projection engine — the only code that writes to the game's mods
//! folder. Model (chosen by the user):
//!
//! * **At rest:** every mod is moved into `mods/archive/<Category>/` (subfolders the
//!   game ignores, since it only reads the flat root).
//! * **Active:** the chosen set is projected back into the flat root as **hardlinks**
//!   (zips) or dir links (unpacked mods) — same volume as the archive, so no admin /
//!   Developer Mode and no disk duplication. The game reads the flat root and loads
//!   exactly the active set.
//! * **Flatten:** everything moves back to a vanilla flat `mods/`; only *empty* archive
//!   directories are removed. Anything Silo doesn't recognize is left in place, never
//!   deleted — always one step from stock, and never a step that eats a file.
//!
//! Every move/link is recorded in the `organized` manifest so cleanup only ever
//! touches Silo-owned entries; nothing the user placed is deleted. Failures are
//! collected and reported, not panicked on.
//!
//! ## Module map
//!
//! Split out of the old single `organize.rs` (#186) as a pure move. Each file does
//! `use super::*;` and this module glob-re-exports each one, so every item keeps its
//! `organize::` path and its visibility to the rest of the crate.
//!
//! * `mod.rs` (here): the shared types: [`Layout`], [`ModInput`], [`PlannedMove`],
//!   [`Report`], [`RelinkReport`], and archive naming (`archive_dir` / `archive_path`).
//! * `engine`: the layout engine: [`plan_organize`] / [`apply_organize`] (file mods into the
//!   archive), [`set_active`] (project the active set), [`flatten`], and archive cleanup.
//! * `relink`: [`relink_projections`] (copies → links) and the atomic zip re-link
//!   (`replace_zip_link*`, `note_zip_projection`).
//! * `adopt`: [`adopt_foreign`] / [`restore_projection`] for a file the user swapped in, and the
//!   `backups/` naming (`update_backup_path`, …).
//! * `fsops`: filesystem primitives: `move_path` (rename, or cross-drive copy-then-remove),
//!   no-replace flushed copies (`copy_new`, `place_new`, `copy_dir_all`), temp handling.
//! * `links`: making and removing projections: `make_link`, `LinkPolicy` (hardlink → symlink
//!   → copy), folder links / junctions, `remove_link`.
//! * `volumes`: link probes ([`supports_hardlinks`], [`supports_symlinks`], probe sweeping) and
//!   mounts / same-volume checks (`mount_of`, `volume_relation`, `same_volume_for_links`).
//! * `foldercopy`: copy-fallback folder projections: the ownership marker, `copy_state`,
//!   `retire_folder_copy`.
//! * `foreign`: ownership detection: `is_silo_projection` and [`detect_foreign_projections`].
//! * `remove`: [`remove_from_library`], a mod to the Recycle Bin (parked first) and out of the library.
//! * `tests/`: the unit tests, one file per module above (`tests/layout.rs` covers this file);
//!   shared fixtures in `tests/mod.rs`.

use crate::db::{self, OrganizedRow};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

mod adopt;
mod engine;
mod foldercopy;
mod foreign;
mod fsops;
mod links;
mod relink;
mod remove;
mod volumes;

pub use adopt::*;
pub use engine::*;
pub(crate) use foldercopy::*;
pub use foreign::*;
pub(crate) use fsops::*;
pub(crate) use links::*;
pub use relink::*;
pub use remove::*;
pub use volumes::*;

const ARCHIVE: &str = "archive";

/// Where a mods folder's pieces live. `mods` is the flat folder the game reads; `archive` holds
/// parked mods as `<archive>/<Category>/<file>`. Default: `<mods>/archive` (#166 makes it
/// configurable per mods folder).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub mods: PathBuf,
    pub archive: PathBuf,
}

impl Layout {
    /// A validated layout — use this for anything built from user input or settings. Both
    /// folders must be absolute; the archive can't be the mods folder, can't contain it, and
    /// can't sit inside it unless it is the default `<mods>/archive` (the game would otherwise
    /// see parked mods, or Silo would file the mods folder into itself). Compared canonically.
    pub fn new(mods: PathBuf, archive: PathBuf) -> Result<Layout, String> {
        if !mods.is_absolute() {
            return Err(format!(
                "The mods folder must be an absolute path: {}",
                mods.display()
            ));
        }
        if !archive.is_absolute() {
            return Err(format!(
                "The library archive must be an absolute path: {}",
                archive.display()
            ));
        }
        let mods_key = crate::modsroot::canonical_key(&mods);
        let arch_key = crate::modsroot::canonical_key(&archive);
        if arch_key == mods_key {
            return Err("The library archive can't be the mods folder itself".into());
        }
        if Path::new(&mods_key).starts_with(&arch_key) {
            return Err("The library archive can't contain the mods folder".into());
        }
        let layout = Layout { mods, archive };
        if Path::new(&arch_key).starts_with(&mods_key) && !layout.archive_is_default() {
            return Err(format!(
                "The library archive can't be inside the mods folder (except the default {ARCHIVE}/ folder)"
            ));
        }
        Ok(layout)
    }

    pub fn default_for(mods: &Path) -> Layout {
        Layout {
            mods: mods.to_path_buf(),
            archive: mods.join(ARCHIVE),
        }
    }

    /// True when the archive is the default `<mods>/archive` (Silo may remove it when empty).
    /// Compared canonically, so a differently spelled path to the same folder still counts.
    pub fn archive_is_default(&self) -> bool {
        crate::modsroot::canonical_key(&self.archive)
            == crate::modsroot::canonical_key(&self.mods.join(ARCHIVE))
    }

    /// A custom archive whose folder isn't there (its drive unplugged, or moved away). The
    /// default archive is never "missing" — it simply may not exist yet.
    pub fn archive_missing(&self) -> bool {
        !self.archive_is_default() && !self.archive.is_dir()
    }
}

/// Dropped inside a copy-fallback **directory** projection so we can later prove Silo
/// created it (a dir has no hardlink identity to compare). Hardlink/symlink projections
/// are identified structurally and need no marker.
pub(crate) const SILO_MARKER: &str = ".silo-projection";

/// A mod present in the flat root that could be organized.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModInput {
    pub tech_name: String,
    /// Basename in the flat root: `FS25_Foo.zip` (zip) or `FS25_Foo` (dir).
    pub file_name: String,
    /// "zip" | "dir"
    pub kind: String,
    pub category: String,
    pub subcategory: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedMove {
    pub tech_name: String,
    pub file_name: String,
    pub category: String,
    pub rel_from: String,
    pub rel_to: String,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub changed: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
    /// Not errors, but worth telling the user (e.g. where an edited folder copy was kept).
    pub notices: Vec<String>,
    /// Tech names `set_active` left parked although wanted, because another mod of that name is
    /// already in the mods folder (#421). Not an error: the name is on through that copy. The
    /// Library rescans when it doesn't show that copy yet.
    pub other_copies: Vec<String>,
    /// Problems `set_active_touched` found with mods the change didn't touch (a file at a
    /// wanted mod's name that isn't Silo's copy). Already listed in Diagnose → Problems, so
    /// the change's result doesn't repeat them.
    pub standing: Vec<String>,
}

/// Result of the link-repair pass — how many copy-projections were deduped into links
/// (hardlinks on the archive's volume, symlinks when the archive is on another drive).
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelinkReport {
    /// Copy-projections converted to hardlinks (same volume) or symlinks (other drive).
    pub upgraded: usize,
    /// Already a hardlink/symlink to the archive — nothing to do.
    pub already_linked: usize,
    /// Left untouched: no archived twin, cross-volume, or the user's own (different) file.
    pub skipped: usize,
    /// Bytes reclaimed by collapsing duplicate copies into links to the archived file.
    pub reclaimed_bytes: u64,
    /// The archive is on the mods folder's volume, but that filesystem rejects hardlinks (a
    /// cloud-sync provider — OneDrive / Google Drive / Proton Drive). Silo uses copies there;
    /// there's nothing to optimize.
    pub hardlinks_unsupported: bool,
    /// The archive is on another drive (no hardlinks possible) and symlinks aren't available
    /// (Windows without Developer Mode, a sandboxed Mac App Store install, or a filesystem that
    /// refuses them). Active mods stay copies; nothing was done.
    pub cross_drive_copies: bool,
    pub errors: Vec<String>,
}

/// Windows-invalid path chars → underscore (spaces and `&` are fine in folder names). A
/// category that collapses to nothing once whitespace is trimmed and trailing dots/spaces are
/// dropped (as Windows does) — `""`, `"."`, `".."`, `". ."` … — would name the archive itself
/// or its parent, so it becomes `"_"`. Every other name is kept as-is, so existing archive
/// folders keep their names.
fn sanitize(category: &str) -> String {
    let s: String = category
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect();
    if s.trim().trim_end_matches(['.', ' ']).is_empty() {
        "_".into()
    } else {
        s
    }
}

pub(crate) fn archive_dir(archive: &Path, category: &str) -> PathBuf {
    archive.join(sanitize(category))
}

pub(crate) fn archive_path(archive: &Path, category: &str, file_name: &str) -> PathBuf {
    archive_dir(archive, category).join(file_name)
}

#[cfg(test)]
mod tests;

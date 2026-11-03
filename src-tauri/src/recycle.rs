//! Move a file or folder to the Recycle Bin / Trash (#429) the way deleting it in the system's own
//! file manager does, and say what became of that very item.
//!
//! - Windows: the shell's delete with undo (the `trash` crate). Where it can't recycle (a USB or
//!   network drive, the bin turned off or too small), Windows itself asks whether to delete the
//!   item permanently, and the user's answer stands. Afterwards the item's own new Recycle Bin
//!   record (`$Recycle.Bin\<user>\$I…`, naming its original path, beside its `$R…` payload) tells
//!   recycled from deleted.
//! - Linux: a folder on another drive than the Trash is refused (moving it there means copying and
//!   deleting it, which can stop halfway).
//! - macOS: the system moves it to the Trash of its volume.

use crate::organize::Binned;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Move `p` to the bin and say what became of it.
pub fn move_to_bin(p: &Path) -> Binned {
    #[cfg(windows)]
    {
        windows::move_to_bin(p)
    }
    #[cfg(not(windows))]
    {
        #[cfg(target_os = "linux")]
        if p.is_dir() && !same_drive_as_trash(p) {
            return Binned::Kept(
                "this folder is on another drive than your Trash, so Silo won't move it there. Delete it yourself if you want it gone".into(),
            );
        }
        match trash::delete(p) {
            Ok(()) => Binned::Recycled,
            Err(e) => after_error(p, &e.to_string()),
        }
    }
}

/// After a move that failed: still there, nothing happened; confirmed gone, it's gone without a
/// receipt; and when Silo can't look, it can't say.
fn after_error(p: &Path, e: &str) -> Binned {
    match p.symlink_metadata() {
        Ok(_) => Binned::Kept(format!("couldn't move it to the Recycle Bin ({e})")),
        Err(gone) if gone.kind() == std::io::ErrorKind::NotFound => Binned::Gone(format!(
            "it's gone from its folder, but moving it to the Recycle Bin reported a problem ({e}), so check the Recycle Bin for it"
        )),
        Err(other) => Binned::Unchecked(format!(
            "moving it to the Recycle Bin reported a problem ({e}), and Silo couldn't check where it is now ({other})"
        )),
    }
}

/// Whether `p` is on the same drive as the user's Trash (`$XDG_DATA_HOME/Trash`, or the nearest
/// folder of it that exists). False when it can't be told.
#[cfg(target_os = "linux")]
fn same_drive_as_trash(p: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Some(mut trash) = dirs::data_dir().map(|d| d.join("Trash")) else {
        return false;
    };
    let trash_dev = loop {
        if let Ok(m) = std::fs::metadata(&trash) {
            break m.dev();
        }
        if !trash.pop() {
            return false;
        }
    };
    std::fs::symlink_metadata(p).is_ok_and(|m| m.dev() == trash_dev)
}

/// The original path a Windows Recycle Bin `$I` record names. Version 1: 8-byte version, 8-byte
/// size, 8-byte deletion time, then the path in 520 bytes of UTF-16. Version 2: the same header,
/// a 4-byte length in UTF-16 units (with the terminating NUL), then the path. None if malformed.
pub fn i_record_path(bytes: &[u8]) -> Option<String> {
    let version = i64::from_le_bytes(bytes.get(0..8)?.try_into().ok()?);
    let utf16 = match version {
        1 => bytes.get(24..24 + 520)?,
        2 => {
            let units = u32::from_le_bytes(bytes.get(24..28)?.try_into().ok()?) as usize;
            bytes.get(28..28 + units.checked_mul(2)?)?
        }
        _ => return None,
    };
    let units: Vec<u16> = utf16
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    String::from_utf16(&units[..end]).ok()
}

/// A Windows path as the Recycle Bin records it (no `\\?\` prefix), lower-cased for comparing.
#[cfg_attr(not(windows), allow(dead_code))]
fn plain(p: &str) -> String {
    let p = p
        .strip_prefix(r"\\?\UNC\")
        .map(|r| format!(r"\\{r}"))
        .unwrap_or_else(|| p.strip_prefix(r"\\?\").unwrap_or(p).to_string());
    p.to_lowercase()
}

/// What a drive's `$Recycle.Bin` held before a move: each user folder in it, with the `$I`
/// records listed there, or None for one that couldn't be read whole (another user's, or an
/// error partway). A user folder that wasn't there at all is new, so all it holds is new.
#[derive(Debug, Default)]
#[cfg_attr(not(windows), allow(dead_code))]
struct Baseline(HashMap<PathBuf, Option<HashSet<PathBuf>>>);

/// The `$I` records in `dir`, or None unless it could be read whole.
#[cfg_attr(not(windows), allow(dead_code))]
fn listed_records(dir: &Path) -> Option<HashSet<PathBuf>> {
    let mut out = HashSet::new();
    for e in std::fs::read_dir(dir).ok()? {
        let e = e.ok()?;
        if e.file_name().to_string_lossy().starts_with("$I") {
            out.insert(e.path());
        }
    }
    Some(out)
}

/// The user folders `bin` lists, or None unless it could be read whole. A `bin` that doesn't
/// exist yet lists none.
#[cfg_attr(not(windows), allow(dead_code))]
fn user_folders(bin: &Path) -> Option<Vec<PathBuf>> {
    let rd = match std::fs::read_dir(bin) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(Vec::new()),
        Err(_) => return None,
    };
    rd.map(|e| e.ok().map(|e| e.path())).collect()
}

/// `bin` as it is before a move, or None when it couldn't be read whole (then no receipt can be
/// told apart from an older record, and none is accepted).
#[cfg_attr(not(windows), allow(dead_code))]
fn baseline(bin: &Path) -> Option<Baseline> {
    Some(Baseline(
        user_folders(bin)?
            .into_iter()
            .map(|d| {
                let r = listed_records(&d);
                (d, r)
            })
            .collect(),
    ))
}

/// What looking for a move's receipt found.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(not(windows), allow(dead_code))]
enum Receipt {
    /// This move's record and payload are in the bin.
    Found,
    /// Everything that could hold it was read, and it isn't there.
    Absent,
    /// Something that could hold it couldn't be read: no conclusion.
    Unknown,
}

/// Look for this move's receipt in `bin`: a `$I` record that wasn't there `before` (in a user
/// folder read whole then, or one that's new since), names `original` (compared as [`plain`]),
/// and whose `$R` payload is there, the same kind and, for a file, the same size. An older record
/// for the same path (an earlier removal) never counts. `Absent` only when every place it could be
/// was read; any folder, record or payload that couldn't be read makes it `Unknown`.
#[cfg_attr(not(windows), allow(dead_code))]
fn receipt(bin: &Path, before: &Baseline, original: &str, was_dir: bool, size: u64) -> Receipt {
    let Some(now) = user_folders(bin) else {
        return Receipt::Unknown;
    };
    let mut unknown = false;
    for dir in &now {
        let old = match before.0.get(dir) {
            Some(Some(listed)) => listed.clone(),
            // Couldn't be read whole before: new can't be told from old there.
            Some(None) => {
                unknown |= listed_records(dir).is_some();
                continue;
            }
            None => HashSet::new(), // new since: everything in it is new
        };
        let Some(records) = listed_records(dir) else {
            unknown = true;
            continue;
        };
        for r in records.into_iter().filter(|r| !old.contains(r)) {
            let Ok(bytes) = std::fs::read(&r) else {
                unknown = true;
                continue;
            };
            let Some(named) = i_record_path(&bytes) else {
                unknown = true; // a record Silo can't read might be this one
                continue;
            };
            if plain(&named) != original {
                continue;
            }
            let name = r
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let payload = r.with_file_name(format!("$R{}", &name[2..]));
            match payload.symlink_metadata() {
                Ok(m)
                    if !m.file_type().is_symlink()
                        && m.is_dir() == was_dir
                        && (was_dir || m.len() == size) =>
                {
                    return Receipt::Found
                }
                // Its record without a matching payload: not a recycled copy of it.
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => unknown = true,
            }
        }
    }
    if unknown {
        Receipt::Unknown
    } else {
        Receipt::Absent
    }
}

/// What became of an item that's confirmed gone from its folder, from what looking for its
/// receipt found (`None`: the bin couldn't be found or read before the move). Only a complete
/// look that found nothing says it was deleted permanently; anything inconclusive says so.
#[cfg_attr(not(windows), allow(dead_code))]
fn gone_outcome(looked: Option<Receipt>) -> Binned {
    match looked {
        Some(Receipt::Found) => Binned::Recycled,
        Some(Receipt::Absent) => Binned::Gone(
            "Windows deleted it permanently instead of moving it to the Recycle Bin".into(),
        ),
        Some(Receipt::Unknown) | None => Binned::Gone(
            "it's gone, but Silo couldn't confirm it went to the Recycle Bin, so look for it there"
                .into(),
        ),
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    pub(super) fn move_to_bin(p: &Path) -> Binned {
        // What the Recycle Bin will record, taken while it's still there: its path, kind and size,
        // and what the bin held, so only a new record counts as this move's receipt.
        let (original, meta) = match (std::fs::canonicalize(p), p.symlink_metadata()) {
            (Ok(c), Ok(m)) => (plain(&c.to_string_lossy()), m),
            (Err(e), _) | (_, Err(e)) => {
                return Binned::Kept(format!("Silo couldn't check it ({e})"))
            }
        };
        let bin = recycle_bin_of(p);
        let before = bin.as_deref().and_then(baseline);
        // Where it can't recycle, Windows asks before deleting permanently: the user's answer
        // stands ("No" leaves it, and this reports it kept).
        if let Err(e) = trash::delete(p) {
            return after_error(p, &e.to_string());
        }
        match p.symlink_metadata() {
            Ok(_) => return Binned::Kept("Windows didn't move or delete it".into()),
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Binned::Unchecked(format!("Silo couldn't check where it is now ({e})"))
            }
            Err(_) => {}
        }
        gone_outcome(match (bin, before) {
            (Some(bin), Some(before)) => {
                Some(receipt(&bin, &before, &original, meta.is_dir(), meta.len()))
            }
            _ => None,
        })
    }

    /// The `$Recycle.Bin` folder of `p`'s drive.
    fn recycle_bin_of(p: &Path) -> Option<PathBuf> {
        use windows_sys::Win32::Storage::FileSystem::GetVolumePathNameW;
        let wide: Vec<u16> = p.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut root = vec![0u16; 1024];
        // SAFETY: both buffers are valid for the call; `wide` is NUL-terminated and `root` holds
        // `root.len()` units.
        let ok = unsafe { GetVolumePathNameW(wide.as_ptr(), root.as_mut_ptr(), root.len() as u32) };
        if ok == 0 {
            return None;
        }
        let end = root.iter().position(|&c| c == 0)?;
        Some(PathBuf::from(std::ffi::OsString::from_wide(&root[..end])).join("$Recycle.Bin"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(version: i64) -> Vec<u8> {
        let mut b = version.to_le_bytes().to_vec();
        b.extend(1234i64.to_le_bytes());
        b.extend(0i64.to_le_bytes());
        b
    }

    /// A `$I` record (version 2) naming `path`.
    fn record(path: &str) -> Vec<u8> {
        let units: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        let mut b = header(2);
        b.extend((units.len() as u32).to_le_bytes());
        b.extend(units.iter().flat_map(|u| u.to_le_bytes()));
        b
    }

    #[test]
    fn reads_both_record_versions() {
        let path: Vec<u16> = r"C:\mods\FS25_Foo.zip"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut v2 = header(2);
        v2.extend((path.len() as u32).to_le_bytes());
        v2.extend(path.iter().flat_map(|u| u.to_le_bytes()));
        assert_eq!(i_record_path(&v2).as_deref(), Some(r"C:\mods\FS25_Foo.zip"));

        let mut v1 = header(1);
        let mut fixed = path.clone();
        fixed.resize(260, 0);
        v1.extend(fixed.iter().flat_map(|u| u.to_le_bytes()));
        assert_eq!(i_record_path(&v1).as_deref(), Some(r"C:\mods\FS25_Foo.zip"));

        assert_eq!(i_record_path(&header(3)), None);
        assert_eq!(i_record_path(&v2[..30]), None, "cut short");
    }

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("silo_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn only_a_new_record_with_its_payload_is_this_moves_receipt() {
        let bin = temp("receipt");
        let user = bin.join("S-1-5-21-test");
        std::fs::create_dir_all(&user).unwrap();
        let original = plain(r"C:\mods\FS25_Foo.zip");
        // An earlier removal of the same path: its record was there before this move.
        std::fs::write(user.join("$IOLD111.zip"), record(r"C:\mods\FS25_Foo.zip")).unwrap();
        std::fs::write(user.join("$ROLD111.zip"), b"PK old").unwrap();
        let before = baseline(&bin).unwrap();
        assert_eq!(
            receipt(&bin, &before, &original, false, 6),
            Receipt::Absent,
            "an older record never counts"
        );
        // A new record whose payload is missing, or the wrong size, doesn't count either.
        std::fs::write(user.join("$INEW222.zip"), record(r"C:\mods\FS25_Foo.zip")).unwrap();
        assert_eq!(receipt(&bin, &before, &original, false, 6), Receipt::Absent);
        std::fs::write(user.join("$RNEW222.zip"), b"PK other size").unwrap();
        assert_eq!(receipt(&bin, &before, &original, false, 6), Receipt::Absent);
        // Another path's new record doesn't count.
        std::fs::write(user.join("$IOTH333.zip"), record(r"C:\mods\FS25_Bar.zip")).unwrap();
        std::fs::write(user.join("$ROTH333.zip"), b"PK new").unwrap();
        assert_eq!(receipt(&bin, &before, &original, false, 6), Receipt::Absent);
        // Its own new record with its payload: the receipt (the prefix and case don't matter).
        std::fs::write(
            user.join("$IMINE44.zip"),
            record(r"\\?\C:\Mods\FS25_Foo.zip"),
        )
        .unwrap();
        std::fs::write(user.join("$RMINE44.zip"), b"PK new").unwrap();
        assert_eq!(receipt(&bin, &before, &original, false, 6), Receipt::Found);
        let _ = std::fs::remove_dir_all(&bin);
    }

    #[test]
    fn a_user_bin_folder_made_by_the_move_is_searched() {
        // No bin at all before (a drive never recycled to): the move makes it, with the receipt.
        let root = temp("receipt_new_bin");
        let bin = root.join("$Recycle.Bin");
        let before = baseline(&bin).unwrap();
        let user = bin.join("S-1-5-21-test");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(user.join("$IAAA111.zip"), record(r"C:\mods\FS25_Foo.zip")).unwrap();
        std::fs::write(user.join("$RAAA111.zip"), b"PK").unwrap();
        assert_eq!(
            receipt(&bin, &before, &plain(r"C:\mods\FS25_Foo.zip"), false, 2),
            Receipt::Found
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn a_user_folder_that_couldnt_be_read_before_never_yields_a_receipt() {
        use std::os::unix::fs::PermissionsExt;
        let bin = temp("receipt_unread");
        let user = bin.join("S-1-5-21-test");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(user.join("$IOLD111.zip"), record(r"C:\mods\FS25_Foo.zip")).unwrap();
        std::fs::write(user.join("$ROLD111.zip"), b"PK").unwrap();
        std::fs::set_permissions(&user, std::fs::Permissions::from_mode(0o000)).unwrap();
        let before = baseline(&bin).unwrap();
        std::fs::set_permissions(&user, std::fs::Permissions::from_mode(0o755)).unwrap();
        if before.0.get(&user).is_some_and(|r| r.is_some()) {
            // Running as root: permissions don't apply, nothing to test.
            let _ = std::fs::remove_dir_all(&bin);
            return;
        }
        let looked = receipt(&bin, &before, &plain(r"C:\mods\FS25_Foo.zip"), false, 2);
        assert_eq!(
            looked,
            Receipt::Unknown,
            "never a receipt, and never proof of absence"
        );
        assert!(
            matches!(gone_outcome(Some(looked)), Binned::Gone(ref m) if m.contains("couldn't confirm"))
        );
        let _ = std::fs::remove_dir_all(&bin);
    }

    #[test]
    fn only_a_complete_look_says_deleted_permanently() {
        assert_eq!(gone_outcome(Some(Receipt::Found)), Binned::Recycled);
        assert!(
            matches!(gone_outcome(Some(Receipt::Absent)), Binned::Gone(ref m) if m.contains("deleted it permanently"))
        );
        for inconclusive in [Some(Receipt::Unknown), None] {
            match gone_outcome(inconclusive) {
                Binned::Gone(m) => {
                    assert!(
                        m.contains("couldn't confirm") && !m.contains("permanently"),
                        "{m}"
                    )
                }
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn an_unreadable_new_record_or_a_bin_gone_after_is_inconclusive() {
        let bin = temp("receipt_unknown");
        let user = bin.join("S-1-5-21-test");
        std::fs::create_dir_all(&user).unwrap();
        let before = baseline(&bin).unwrap();
        let original = plain(r"C:\mods\FS25_Foo.zip");
        // A new record Silo can't make sense of might be this move's.
        std::fs::write(user.join("$IBAD111.zip"), b"not a record").unwrap();
        assert_eq!(
            receipt(&bin, &before, &original, false, 2),
            Receipt::Unknown
        );
        // The bin can't be read after the move at all.
        let _ = std::fs::remove_dir_all(&bin);
        std::fs::write(&bin, b"now a file, not a folder").unwrap();
        assert_eq!(
            receipt(&bin, &before, &original, false, 2),
            Receipt::Unknown
        );
        let _ = std::fs::remove_file(&bin);
    }

    #[test]
    fn a_failed_move_says_whether_the_item_is_there_gone_or_unknown() {
        let d = temp("after_error");
        let f = d.join("FS25_Foo.zip");
        std::fs::write(&f, b"PK").unwrap();
        assert!(matches!(after_error(&f, "busy"), Binned::Kept(_)));
        std::fs::remove_file(&f).unwrap();
        assert!(matches!(after_error(&f, "busy"), Binned::Gone(_)));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let locked = d.join("locked");
            std::fs::create_dir_all(&locked).unwrap();
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
            let inside = locked.join("FS25_Foo.zip");
            let unknown = matches!(
                inside.symlink_metadata().map_err(|e| e.kind()),
                Err(k) if k != std::io::ErrorKind::NotFound
            );
            if unknown {
                assert!(matches!(after_error(&inside, "busy"), Binned::Unchecked(_)));
            }
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Uses the real Recycle Bin: run by hand with `cargo test --lib recycle -- --ignored`.
    #[test]
    #[ignore]
    fn a_file_and_a_folder_reach_the_real_bin_with_a_receipt() {
        let d = temp("recycle_real");
        std::fs::create_dir_all(d.join("FS25_SiloBinTestFolder")).unwrap();
        std::fs::write(d.join("FS25_SiloBinTestFolder/modDesc.xml"), b"<modDesc/>").unwrap();
        let f = d.join("FS25_SiloBinTest.zip");
        std::fs::write(&f, b"PK").unwrap();
        assert_eq!(move_to_bin(&f), Binned::Recycled);
        assert_eq!(
            move_to_bin(&d.join("FS25_SiloBinTestFolder")),
            Binned::Recycled
        );
        assert!(!f.exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}

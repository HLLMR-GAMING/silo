//! Move a file or folder to the Recycle Bin / Trash, and only there (#429), saying what happened
//! to that very item: recycled, kept where it was, or gone without a record Silo could find.
//!
//! - Windows: refused up front where the Recycle Bin can't take it (not a fixed drive, recycling
//!   turned off by setting or policy, larger than the bin). Afterwards the item's own Recycle Bin
//!   record (`$Recycle.Bin\<user>\$I…`, which names its original path) is the receipt.
//! - Linux: a folder on another drive than the Trash is refused (moving it there means copying and
//!   deleting it, which can stop halfway).
//! - macOS: the system moves it to the Trash of its volume.

use crate::organize::Binned;
use std::path::Path;

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
        Err(gone) if gone.kind() == std::io::ErrorKind::NotFound => Binned::Unsure(format!(
            "it's gone from its folder, but moving it to the Recycle Bin reported a problem ({e})"
        )),
        Err(other) => Binned::Unchecked(format!(
            "moving it to the Recycle Bin reported a problem ({e}), and Silo couldn't check where it is now ({other})"
        )),
    }
}

/// A Windows path as the Recycle Bin records it (no `\\?\` prefix), lower-cased for comparing.
fn plain(p: &str) -> String {
    let p = p
        .strip_prefix(r"\\?\UNC\")
        .map(|r| format!(r"\\{r}"))
        .unwrap_or_else(|| p.strip_prefix(r"\\?\").unwrap_or(p).to_string());
    p.to_lowercase()
}

/// The `$I` records in the readable Recycle Bin folders `dirs` (one per user; another user's
/// can't be read and isn't listed).
fn records(dirs: &[std::path::PathBuf]) -> std::collections::HashSet<std::path::PathBuf> {
    dirs.iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .flatten()
        .filter(|r| r.file_name().to_string_lossy().starts_with("$I"))
        .map(|r| r.path())
        .collect()
}

/// This move's receipt: a `$I` record in `dirs` that wasn't there `before`, names `original`
/// (compared as [`plain`]), and whose `$R` payload is there, the same kind and, for a file, the
/// same size. An older record for the same path (an earlier removal) never counts.
fn new_receipt(
    dirs: &[std::path::PathBuf],
    before: &std::collections::HashSet<std::path::PathBuf>,
    original: &str,
    was_dir: bool,
    size: u64,
) -> bool {
    records(dirs)
        .into_iter()
        .filter(|r| !before.contains(r))
        .any(|r| {
            let named = std::fs::read(&r).ok().and_then(|b| i_record_path(&b));
            if !named.is_some_and(|n| plain(&n) == original) {
                return false;
            }
            let name = r
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let payload = r.with_file_name(format!("$R{}", &name[2..]));
            payload.symlink_metadata().is_ok_and(|m| {
                !m.file_type().is_symlink() && m.is_dir() == was_dir && (was_dir || m.len() == size)
            })
        })
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

#[cfg(windows)]
mod windows {
    use super::*;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    pub(super) fn move_to_bin(p: &Path) -> Binned {
        if let Err(why) = can_recycle(p) {
            return Binned::Kept(why);
        }
        // What the Recycle Bin will record, taken while it's still there: its path, kind and size,
        // and the records already in the bin, so only a new one counts as this move's receipt.
        let (original, meta) = match (std::fs::canonicalize(p), p.symlink_metadata()) {
            (Ok(c), Ok(m)) => (plain(&c.to_string_lossy()), m),
            (Err(e), _) | (_, Err(e)) => {
                return Binned::Kept(format!("Silo couldn't check it ({e})"))
            }
        };
        let dirs = bin_dirs(p);
        let before = records(&dirs);
        if let Err(e) = trash::delete(p) {
            return after_error(p, &e.to_string());
        }
        match p.symlink_metadata() {
            Ok(_) => return Binned::Kept("Windows didn't move it to the Recycle Bin".into()),
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Binned::Unchecked(format!("Silo couldn't check where it is now ({e})"))
            }
            Err(_) => {}
        }
        if new_receipt(&dirs, &before, &original, meta.is_dir(), meta.len()) {
            Binned::Recycled
        } else {
            Binned::Unsure(
                "it's gone from its folder, but Silo found no Recycle Bin record for it, so Windows may have deleted it permanently".into(),
            )
        }
    }

    /// The readable Recycle Bin folders on `p`'s drive (`<root>\$Recycle.Bin\<user>`).
    fn bin_dirs(p: &Path) -> Vec<std::path::PathBuf> {
        let Some(root) = volume_root(p) else {
            return Vec::new();
        };
        let end = root.iter().position(|&c| c == 0).unwrap_or(root.len());
        let bin = std::path::PathBuf::from(std::ffi::OsString::from_wide(&root[..end]))
            .join("$Recycle.Bin");
        std::fs::read_dir(&bin)
            .map(|users| {
                users
                    .flatten()
                    .map(|u| u.path())
                    .filter(|u| std::fs::read_dir(u).is_ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Why the Recycle Bin can't take `p`, if it can't: not a fixed drive, recycling turned off
    /// for that drive or by policy, or `p` larger than the bin. Unknown limits don't refuse.
    fn can_recycle(p: &Path) -> Result<(), String> {
        let no_bin = "this drive has no Recycle Bin, so Silo won't remove it. Delete it yourself if you want it gone";
        let root = volume_root(p).ok_or(no_bin)?;
        // SAFETY: `root` is a NUL-terminated volume path.
        const DRIVE_FIXED: u32 = 3; // windows-sys WindowsProgramming::DRIVE_FIXED
        if unsafe { windows_sys::Win32::Storage::FileSystem::GetDriveTypeW(root.as_ptr()) }
            != DRIVE_FIXED
        {
            return Err(no_bin.into());
        }
        let off = "the Recycle Bin is turned off for this drive, so Silo won't remove it. Delete it yourself if you want it gone";
        let policy = r"Software\Microsoft\Windows\CurrentVersion\Policies\Explorer";
        if dword(true, policy, "NoRecycleFiles") == Some(1)
            || dword(false, policy, "NoRecycleFiles") == Some(1)
        {
            return Err(off.into());
        }
        if let Some(guid) = volume_guid(&root) {
            let key = format!(
                r"Software\Microsoft\Windows\CurrentVersion\Explorer\BitBucket\Volume\{guid}"
            );
            if dword(false, &key, "NukeOnDelete") == Some(1) {
                return Err(off.into());
            }
            if let Some(max_mb) = dword(false, &key, "MaxCapacity") {
                if size_of(p) > u64::from(max_mb) * 1024 * 1024 {
                    return Err("it's larger than this drive's Recycle Bin can hold, so Silo won't remove it. Delete it yourself if you want it gone".into());
                }
            }
        }
        Ok(())
    }

    /// A file's size, or a folder's total, not following links.
    fn size_of(p: &Path) -> u64 {
        walkdir::WalkDir::new(p)
            .follow_links(false)
            .into_iter()
            .flatten()
            .filter_map(|e| e.metadata().ok())
            .filter(|m| m.is_file())
            .map(|m| m.len())
            .sum()
    }

    /// The NUL-terminated root of the volume `p` is on (a drive letter's root, or a mount folder).
    fn volume_root(p: &Path) -> Option<Vec<u16>> {
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
        root.truncate(end + 1);
        Some(root)
    }

    /// The `{…}` GUID of the volume whose root is `root` (NUL-terminated).
    fn volume_guid(root: &[u16]) -> Option<String> {
        use windows_sys::Win32::Storage::FileSystem::GetVolumeNameForVolumeMountPointW;
        let mut name = vec![0u16; 64];
        // SAFETY: `root` is NUL-terminated; `name` holds `name.len()` units.
        let ok = unsafe {
            GetVolumeNameForVolumeMountPointW(root.as_ptr(), name.as_mut_ptr(), name.len() as u32)
        };
        if ok == 0 {
            return None;
        }
        let end = name.iter().position(|&c| c == 0)?;
        let s = String::from_utf16(&name[..end]).ok()?;
        let open = s.find('{')?;
        let close = s.find('}')?;
        Some(s[open..=close].to_string())
    }

    /// A DWORD registry value under HKLM (`machine`) or HKCU, or None.
    fn dword(machine: bool, key: &str, value: &str) -> Option<u32> {
        use windows_sys::Win32::System::Registry::{
            RegGetValueW, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD,
        };
        let wide = |s: &str| -> Vec<u16> {
            std::ffi::OsStr::new(s)
                .encode_wide()
                .chain(Some(0))
                .collect()
        };
        let (k, v) = (wide(key), wide(value));
        let mut data: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        let hive = if machine {
            HKEY_LOCAL_MACHINE
        } else {
            HKEY_CURRENT_USER
        };
        // SAFETY: NUL-terminated names; `data` and `size` are valid for a DWORD.
        let rc = unsafe {
            RegGetValueW(
                hive,
                k.as_ptr(),
                v.as_ptr(),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                (&mut data as *mut u32).cast(),
                &mut size,
            )
        };
        (rc == 0).then_some(data)
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

    /// A `$I` record (version 2) naming `path`.
    fn record(path: &str) -> Vec<u8> {
        let units: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        let mut b = header(2);
        b.extend((units.len() as u32).to_le_bytes());
        b.extend(units.iter().flat_map(|u| u.to_le_bytes()));
        b
    }

    #[test]
    fn only_a_new_record_with_its_payload_is_this_moves_receipt() {
        let d = std::env::temp_dir().join(format!("silo_receipt_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let user = d.join("S-1-5-21-test");
        std::fs::create_dir_all(&user).unwrap();
        let dirs = vec![user.clone()];
        let original = plain(r"C:\mods\FS25_Foo.zip");
        // An earlier removal of the same path: its record was there before this move.
        std::fs::write(user.join("$IOLD111.zip"), record(r"C:\mods\FS25_Foo.zip")).unwrap();
        std::fs::write(user.join("$ROLD111.zip"), b"PK old").unwrap();
        let before = records(&dirs);
        assert!(
            !new_receipt(&dirs, &before, &original, false, 6),
            "an older record never counts"
        );
        // A new record whose payload is missing, or the wrong size, doesn't count either.
        std::fs::write(user.join("$INEW222.zip"), record(r"C:\mods\FS25_Foo.zip")).unwrap();
        assert!(!new_receipt(&dirs, &before, &original, false, 6));
        std::fs::write(user.join("$RNEW222.zip"), b"PK other size").unwrap();
        assert!(!new_receipt(&dirs, &before, &original, false, 6));
        // Another path's new record doesn't count.
        std::fs::write(user.join("$IOTH333.zip"), record(r"C:\mods\FS25_Bar.zip")).unwrap();
        std::fs::write(user.join("$ROTH333.zip"), b"PK new").unwrap();
        assert!(!new_receipt(&dirs, &before, &original, false, 6));
        // Its own new record with its payload: the receipt (the prefix and case don't matter).
        std::fs::write(
            user.join("$IMINE44.zip"),
            record(r"\\?\C:\Mods\FS25_Foo.zip"),
        )
        .unwrap();
        std::fs::write(user.join("$RMINE44.zip"), b"PK new").unwrap();
        assert!(new_receipt(&dirs, &before, &original, false, 6));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_failed_move_says_whether_the_item_is_there_gone_or_unknown() {
        let d = std::env::temp_dir().join(format!("silo_after_error_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("FS25_Foo.zip");
        std::fs::write(&f, b"PK").unwrap();
        assert!(matches!(after_error(&f, "busy"), Binned::Kept(_)));
        std::fs::remove_file(&f).unwrap();
        assert!(matches!(after_error(&f, "busy"), Binned::Unsure(_)));
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
        let d = std::env::temp_dir().join(format!("silo_recycle_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
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

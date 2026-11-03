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

/// After a move that failed: still there, nothing happened; otherwise it's gone unconfirmed.
fn after_error(p: &Path, e: &str) -> Binned {
    if p.symlink_metadata().is_ok() {
        Binned::Kept(format!("couldn't move it to the Recycle Bin ({e})"))
    } else {
        Binned::Unsure(format!(
            "it's gone from its folder, but moving it to the Recycle Bin reported a problem ({e})"
        ))
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

#[cfg(windows)]
mod windows {
    use super::*;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    pub(super) fn move_to_bin(p: &Path) -> Binned {
        if let Err(why) = can_recycle(p) {
            return Binned::Kept(why);
        }
        // The path as the Recycle Bin will record it, taken while it's still there.
        let original = match std::fs::canonicalize(p) {
            Ok(c) => plain(&c.to_string_lossy()),
            Err(e) => return Binned::Kept(format!("Silo couldn't check it ({e})")),
        };
        let started = std::time::SystemTime::now();
        if let Err(e) = trash::delete(p) {
            return after_error(p, &e.to_string());
        }
        if receipt(p, &original, started) {
            Binned::Recycled
        } else if p.symlink_metadata().is_ok() {
            Binned::Kept("Windows didn't move it to the Recycle Bin".into())
        } else {
            Binned::Unsure(
                "it's gone from its folder, but Silo found no Recycle Bin record for it, so Windows may have deleted it permanently".into(),
            )
        }
    }

    /// `\\?\C:\x` and `\\?\UNC\…` as Windows shows them elsewhere, lower-cased for comparing.
    fn plain(p: &str) -> String {
        let p = p
            .strip_prefix(r"\\?\UNC\")
            .map(|r| format!(r"\\{r}"))
            .unwrap_or_else(|| p.strip_prefix(r"\\?\").unwrap_or(p).to_string());
        p.to_lowercase()
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

    /// Whether the Recycle Bin of `p`'s drive holds a record of `original`, made since `started`.
    fn receipt(p: &Path, original: &str, started: std::time::SystemTime) -> bool {
        let Some(root) = volume_root(p) else {
            return false;
        };
        let end = root.iter().position(|&c| c == 0).unwrap_or(root.len());
        let bin = std::path::PathBuf::from(std::ffi::OsString::from_wide(&root[..end]))
            .join("$Recycle.Bin");
        let since = started - std::time::Duration::from_secs(2);
        let Ok(users) = std::fs::read_dir(&bin) else {
            return false;
        };
        for user in users.flatten() {
            let Ok(records) = std::fs::read_dir(user.path()) else {
                continue; // another user's bin
            };
            for r in records.flatten() {
                if !r.file_name().to_string_lossy().starts_with("$I") {
                    continue;
                }
                let recent = r
                    .metadata()
                    .and_then(|m| m.modified())
                    .is_ok_and(|t| t >= since);
                if recent
                    && std::fs::read(r.path())
                        .ok()
                        .and_then(|b| i_record_path(&b))
                        .is_some_and(|named| plain(&named) == original)
                {
                    return true;
                }
            }
        }
        false
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
        assert_eq!(move_to_bin(&d.join("FS25_SiloBinTestFolder")), Binned::Recycled);
        assert!(!f.exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}

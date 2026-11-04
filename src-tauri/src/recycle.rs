//! Move a file or folder to the Recycle Bin / Trash (#429) the way deleting it in the system's own
//! file manager does, and say what became of that very item.
//!
//! - Windows: the shell's delete with undo, asking where File Explorer asks (#442). Where it can't
//!   recycle (a USB or network drive, the bin turned off or too small), Windows itself asks
//!   whether to delete the item permanently, and the user's answer stands. Afterwards the item's
//!   own new Recycle Bin record (`$Recycle.Bin\<user>\$I…`, naming its original path, beside its
//!   `$R…` payload) tells recycled from deleted.
//! - Linux: a folder on another drive than the Trash is refused (moving it there means copying and
//!   deleting it, which can stop halfway).
//! - macOS: the system moves it to the Trash of its volume.

use crate::organize::Binned;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// The window Windows' own prompts sit on top of: its handle (HWND) as a number. `None` leaves
/// them on the desktop. Only Windows uses it.
pub type Owner = Option<isize>;

/// Move `p` to the bin and say what became of it.
pub fn move_to_bin(p: &Path, owner: Owner) -> Binned {
    #[cfg(windows)]
    {
        windows::move_to_bin(p, owner)
    }
    #[cfg(not(windows))]
    {
        let _ = owner;
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

/// `p` as the shell's delete takes it: a full path without the `\\?\` prefix (which it doesn't
/// read), letters kept as they are. `None` for a path that isn't full, which the shell would read
/// against Silo's current folder.
#[cfg_attr(not(windows), allow(dead_code))]
fn shell_path(p: &Path) -> Option<String> {
    let s = p.to_str()?;
    let s = s
        .strip_prefix(r"\\?\UNC\")
        .map(|r| format!(r"\\{r}"))
        .unwrap_or_else(|| s.strip_prefix(r"\\?\").unwrap_or(s).to_string())
        .replace('/', r"\");
    let b = s.as_bytes();
    let drive = b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\';
    (drive || s.starts_with(r"\\")).then_some(s)
}

/// What this user's Recycle Bin folder (`$Recycle.Bin\<user SID>` on the item's drive) held
/// before a move: the `$I` records listed there (`Some`; an empty set when the folder isn't there
/// yet, a drive never recycled to), or `None` when it couldn't be listed whole.
#[cfg_attr(not(windows), allow(dead_code))]
type Before = Option<HashSet<PathBuf>>;

/// How the receipt check reads the disk: list a folder's `$I` records (`Ok(None)` when it isn't
/// there; `Err` when it couldn't be listed whole), read a record, and inspect a payload. The real
/// one is [`DISK`]; a test passes one that fails where it chooses.
#[cfg_attr(not(windows), allow(dead_code))]
struct Io<'a> {
    list: &'a ListFn<'a>,
    read: &'a (dyn Fn(&Path) -> std::io::Result<Vec<u8>> + 'a),
    inspect: &'a InspectFn<'a>,
}

/// Lists a folder's `$I` records: `Ok(None)` when it isn't there, `Err` unless listed whole.
type ListFn<'a> = dyn Fn(&Path) -> std::io::Result<Option<HashSet<PathBuf>>> + 'a;
/// Inspects a payload: (is a folder, size, is a link).
type InspectFn<'a> = dyn Fn(&Path) -> std::io::Result<(bool, u64, bool)> + 'a;

/// The `$I` records in `dir`, read from the disk.
#[cfg_attr(not(windows), allow(dead_code))]
fn list_records(dir: &Path) -> std::io::Result<Option<HashSet<PathBuf>>> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut out = HashSet::new();
    for e in rd {
        let e = e?;
        if e.file_name().to_string_lossy().starts_with("$I") {
            out.insert(e.path());
        }
    }
    Ok(Some(out))
}

#[cfg_attr(not(windows), allow(dead_code))]
const DISK: Io<'static> = Io {
    list: &list_records,
    read: &|p| std::fs::read(p),
    inspect: &|p| {
        std::fs::symlink_metadata(p).map(|m| (m.is_dir(), m.len(), m.file_type().is_symlink()))
    },
};

/// This user's bin folder as it is before a move.
#[cfg_attr(not(windows), allow(dead_code))]
fn before(io: &Io, user_bin: &Path) -> Before {
    match (io.list)(user_bin) {
        Ok(Some(listed)) => Some(listed),
        Ok(None) => Some(HashSet::new()),
        Err(_) => None,
    }
}

/// What looking for a move's receipt found.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(not(windows), allow(dead_code))]
enum Receipt {
    /// This move's record and payload are in the bin.
    Found,
    /// The user's bin folder was read whole before and after, and it isn't there.
    Absent,
    /// Something that could hold it couldn't be read: no conclusion.
    Unknown,
}

/// Look for this move's receipt in this user's bin folder: a `$I` record that wasn't there
/// `before`, names `original` (compared as [`plain`]), and whose `$R` payload is there, not a
/// link, the same kind and, for a file, the same size. An older record for the same path (an
/// earlier removal) never counts. `Absent` only when the folder was listed whole before and after
/// and every new record and payload could be read; anything else that leaves it open is `Unknown`.
#[cfg_attr(not(windows), allow(dead_code))]
fn receipt(
    io: &Io,
    user_bin: &Path,
    before: &Before,
    original: &str,
    was_dir: bool,
    size: u64,
) -> Receipt {
    let Some(old) = before else {
        return Receipt::Unknown;
    };
    let now = match (io.list)(user_bin) {
        Ok(Some(now)) => now,
        Ok(None) => return Receipt::Absent, // no folder, so no record of anything
        Err(_) => return Receipt::Unknown,
    };
    let mut unknown = false;
    for r in now.iter().filter(|r| !old.contains(*r)) {
        let Ok(bytes) = (io.read)(r) else {
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
        match (io.inspect)(&r.with_file_name(format!("$R{}", &name[2..]))) {
            Ok((dir, len, link)) if !link && dir == was_dir && (was_dir || len == size) => {
                return Receipt::Found
            }
            // Its record without a matching payload: not a recycled copy of it.
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => unknown = true,
        }
    }
    if unknown {
        Receipt::Unknown
    } else {
        Receipt::Absent
    }
}

/// What became of an item that's confirmed gone from its folder, from what looking for its
/// receipt found (`None`: this user's bin folder couldn't be found). Only a complete look that
/// found nothing says it was deleted permanently; anything inconclusive says so.
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

    pub(super) fn move_to_bin(p: &Path, owner: Owner) -> Binned {
        let Some(shell) = shell_path(p) else {
            return Binned::Kept("Silo couldn't check it (not a full path)".into());
        };
        // What the Recycle Bin will record, taken while it's still there: its path, kind and size,
        // and what the bin held, so only a new record counts as this move's receipt.
        let (original, meta) = match (std::fs::canonicalize(p), p.symlink_metadata()) {
            (Ok(c), Ok(m)) => (plain(&c.to_string_lossy()), m),
            (Err(e), _) | (_, Err(e)) => {
                return Binned::Kept(format!("Silo couldn't check it ({e})"))
            }
        };
        let user_bin = recycle_bin_of(p)
            .zip(user_sid())
            .map(|(bin, sid)| bin.join(sid));
        let was = user_bin.as_deref().map(|d| before(&DISK, d));
        // Where it can't recycle, Windows asks before deleting permanently: the user's answer
        // stands ("No" leaves it, and this reports it kept).
        match shell_delete(&shell, owner) {
            Shell::Done => {}
            Shell::Declined if p.symlink_metadata().is_ok() => return Binned::Declined,
            Shell::Declined => {}
            Shell::Failed(e) => return after_error(p, &e),
        }
        match p.symlink_metadata() {
            Ok(_) => return Binned::Kept("Windows didn't move or delete it".into()),
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Binned::Unchecked(format!("Silo couldn't check where it is now ({e})"))
            }
            Err(_) => {}
        }
        gone_outcome(
            user_bin
                .zip(was)
                .map(|(dir, was)| receipt(&DISK, &dir, &was, &original, meta.is_dir(), meta.len())),
        )
    }

    /// What the shell's delete did.
    enum Shell {
        /// It finished: the disk says what became of the item.
        Done,
        /// The user answered No (or closed) a prompt Windows showed.
        Declined,
        /// It failed outright, with the shell's code.
        Failed(String),
    }

    /// File Explorer's delete (#442): to the Recycle Bin with undo, and Windows' own prompts where
    /// Explorer shows them: "permanently delete?" where it can't recycle, and "move to the Recycle
    /// Bin?" only if the user turned that on. No error pop-ups (Silo says what went wrong) and no
    /// "create folder?" question. Never `FOF_NOCONFIRMATION`: with it Windows deletes permanently
    /// without asking, even with `FOF_WANTNUKEWARNING`.
    pub(super) const DELETE_FLAGS: u32 = windows_sys::Win32::UI::Shell::FOF_ALLOWUNDO
        | windows_sys::Win32::UI::Shell::FOF_NOERRORUI
        | windows_sys::Win32::UI::Shell::FOF_NOCONFIRMMKDIR;

    /// Delete `path` (as [`shell_path`] gives it) the way File Explorer does, with its prompts on
    /// top of `owner`.
    fn shell_delete(path: &str, owner: Owner) -> Shell {
        use windows_sys::Win32::System::Com::{
            CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
        };
        use windows_sys::Win32::UI::Shell::{SHFileOperationW, FO_DELETE, SHFILEOPSTRUCTW};
        // The list of paths ends with an empty one: two NULs.
        let from: Vec<u16> = path.encode_utf16().chain([0, 0]).collect();
        let mut op = SHFILEOPSTRUCTW {
            hwnd: owner.unwrap_or(0) as _,
            wFunc: FO_DELETE,
            pFrom: from.as_ptr(),
            pTo: std::ptr::null(),
            fFlags: DELETE_FLAGS as u16,
            fAnyOperationsAborted: 0,
            hNameMappings: std::ptr::null_mut(),
            lpszProgressTitle: std::ptr::null(),
        };
        // The shell's dialogs want a single-threaded apartment. A thread already set up another
        // way (RPC_E_CHANGED_MODE) is used as it is and left so.
        // SAFETY: no reserved pointer; balanced by CoUninitialize below when it succeeded.
        let com = unsafe {
            CoInitializeEx(
                std::ptr::null(),
                (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
            )
        };
        // SAFETY: `op` and the `from` list it points to live across the call; `from` ends with
        // two NULs, `pTo` isn't used by a delete.
        let code = unsafe { SHFileOperationW(&mut op) };
        if com >= 0 {
            // SAFETY: CoInitializeEx succeeded on this thread above (S_OK or S_FALSE).
            unsafe { CoUninitialize() };
        }
        // ERROR_CANCELLED, or the shell's own DE_OPCANCELLED.
        if op.fAnyOperationsAborted != 0 || code == 1223 || code == 0x75 {
            Shell::Declined
        } else if code != 0 {
            Shell::Failed(format!("Windows reported error {code:#x}"))
        } else {
            Shell::Done
        }
    }

    /// The current user's SID as text (`S-1-5-21-…`): the name of their folder in each drive's
    /// `$Recycle.Bin`.
    fn user_sid() -> Option<String> {
        use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
        use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
        use windows_sys::Win32::Security::{
            GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER,
        };
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: the pseudo-handle of this process; `token` receives a handle closed below.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return None;
        }
        let mut buf = vec![0u8; 256];
        let mut len = 0u32;
        // SAFETY: `buf` holds `buf.len()` bytes for the TOKEN_USER the call writes.
        let ok = unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buf.as_mut_ptr().cast(),
                buf.len() as u32,
                &mut len,
            )
        };
        // SAFETY: `token` was opened above.
        unsafe { CloseHandle(token) };
        if ok == 0 {
            return None;
        }
        // SAFETY: the call filled `buf` with a TOKEN_USER; its SID points inside `buf`.
        let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
        let mut text: windows_sys::core::PWSTR = std::ptr::null_mut();
        // SAFETY: `sid` is valid while `buf` lives; `text` is freed with LocalFree below.
        if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
            return None;
        }
        // SAFETY: `text` is a NUL-terminated string from the call.
        let out = unsafe {
            let n = (0..).take_while(|&i| *text.add(i) != 0).count();
            String::from_utf16(std::slice::from_raw_parts(text, n)).ok()
        };
        // SAFETY: allocated by ConvertSidToStringSidW.
        unsafe { LocalFree(text.cast()) };
        out
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

    /// Look on the disk at `dir`, as the real check does.
    fn look(dir: &Path, was: &Before, size: u64) -> Receipt {
        receipt(
            &DISK,
            dir,
            was,
            &plain(r"C:\mods\FS25_Foo.zip"),
            false,
            size,
        )
    }

    #[test]
    fn only_a_new_record_with_its_payload_is_this_moves_receipt() {
        let user = temp("receipt").join("S-1-5-21-test");
        std::fs::create_dir_all(&user).unwrap();
        // An earlier removal of the same path: its record was there before this move.
        std::fs::write(user.join("$IOLD111.zip"), record(r"C:\mods\FS25_Foo.zip")).unwrap();
        std::fs::write(user.join("$ROLD111.zip"), b"PK old").unwrap();
        let was = before(&DISK, &user);
        assert_eq!(
            look(&user, &was, 6),
            Receipt::Absent,
            "an older record never counts"
        );
        // A new record whose payload is missing, or the wrong size, doesn't count either.
        std::fs::write(user.join("$INEW222.zip"), record(r"C:\mods\FS25_Foo.zip")).unwrap();
        assert_eq!(look(&user, &was, 6), Receipt::Absent);
        std::fs::write(user.join("$RNEW222.zip"), b"PK other size").unwrap();
        assert_eq!(look(&user, &was, 6), Receipt::Absent);
        // Another path's new record doesn't count.
        std::fs::write(user.join("$IOTH333.zip"), record(r"C:\mods\FS25_Bar.zip")).unwrap();
        std::fs::write(user.join("$ROTH333.zip"), b"PK new").unwrap();
        assert_eq!(look(&user, &was, 6), Receipt::Absent);
        // Its own new record with its payload: the receipt (the prefix and case don't matter).
        std::fs::write(
            user.join("$IMINE44.zip"),
            record(r"\\?\C:\Mods\FS25_Foo.zip"),
        )
        .unwrap();
        std::fs::write(user.join("$RMINE44.zip"), b"PK new").unwrap();
        assert_eq!(look(&user, &was, 6), Receipt::Found);
        let _ = std::fs::remove_dir_all(user.parent().unwrap());
    }

    #[test]
    fn a_user_bin_folder_made_by_the_move_is_searched() {
        // No folder before (a drive never recycled to): the move makes it, with the receipt.
        let root = temp("receipt_new_bin");
        let user = root.join("$Recycle.Bin").join("S-1-5-21-test");
        let was = before(&DISK, &user);
        assert_eq!(was, Some(HashSet::new()));
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(user.join("$IAAA111.zip"), record(r"C:\mods\FS25_Foo.zip")).unwrap();
        std::fs::write(user.join("$RAAA111.zip"), b"PK").unwrap();
        assert_eq!(look(&user, &was, 2), Receipt::Found);
        let _ = std::fs::remove_dir_all(&root);
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
    fn any_read_that_fails_leaves_it_inconclusive_never_deleted_permanently() {
        let dir = Path::new("bin/S-1-5-21-test");
        let rec = dir.join("$INEW111.zip");
        let fail = || std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let mine = record(r"C:\mods\FS25_Foo.zip");
        let listed =
            |set: &[&PathBuf]| -> HashSet<PathBuf> { set.iter().map(|p| (*p).clone()).collect() };
        let ok_list = |_: &Path| Ok(Some(listed(&[&rec])));
        let ok_read = |_: &Path| Ok(mine.clone());
        let ok_payload = |_: &Path| Ok((false, 2, false));
        let outcome = |io: &Io, was: &Before| {
            gone_outcome(Some(receipt(
                io,
                dir,
                was,
                &plain(r"C:\mods\FS25_Foo.zip"),
                false,
                2,
            )))
        };
        let inconclusive =
            |b: Binned| matches!(b, Binned::Gone(ref m) if m.contains("couldn't confirm"));
        let empty: Before = Some(HashSet::new());

        // Everything readable: the receipt.
        let io = Io {
            list: &ok_list,
            read: &ok_read,
            inspect: &ok_payload,
        };
        assert_eq!(outcome(&io, &empty), Binned::Recycled);
        // The folder couldn't be listed before (and then not after either).
        let fail_list = |_: &Path| Err(fail());
        let io = Io {
            list: &fail_list,
            read: &ok_read,
            inspect: &ok_payload,
        };
        assert!(inconclusive(outcome(&io, &before(&io, dir))));
        // Listed before, not after (open or partway through: both come back as an error).
        assert!(inconclusive(outcome(&io, &empty)));
        // A new record that can't be read.
        let fail_read = |_: &Path| Err(fail());
        let io = Io {
            list: &ok_list,
            read: &fail_read,
            inspect: &ok_payload,
        };
        assert!(inconclusive(outcome(&io, &empty)));
        // Its payload can't be inspected.
        let fail_payload = |_: &Path| Err(fail());
        let io = Io {
            list: &ok_list,
            read: &ok_read,
            inspect: &fail_payload,
        };
        assert!(inconclusive(outcome(&io, &empty)));
        // Read whole both times and nothing there: deleted permanently.
        let none_list = |_: &Path| Ok(Some(HashSet::new()));
        let io = Io {
            list: &none_list,
            read: &ok_read,
            inspect: &ok_payload,
        };
        assert!(
            matches!(outcome(&io, &empty), Binned::Gone(ref m) if m.contains("deleted it permanently"))
        );
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

    #[test]
    fn the_shell_gets_a_full_plain_path_or_nothing() {
        assert_eq!(
            shell_path(Path::new(r"\\?\F:\Mods\FS25_Foo.zip")).as_deref(),
            Some(r"F:\Mods\FS25_Foo.zip"),
            "prefix dropped, letters kept"
        );
        assert_eq!(
            shell_path(Path::new(r"\\?\UNC\nas\games\FS25_Foo.zip")).as_deref(),
            Some(r"\\nas\games\FS25_Foo.zip")
        );
        assert_eq!(
            shell_path(Path::new(r"\\nas\games\FS25_Foo.zip")).as_deref(),
            Some(r"\\nas\games\FS25_Foo.zip")
        );
        assert_eq!(
            shell_path(Path::new("F:/Mods/archive/FS25_Foo.zip")).as_deref(),
            Some(r"F:\Mods\archive\FS25_Foo.zip")
        );
        for relative in [
            "FS25_Foo.zip",
            r"mods\FS25_Foo.zip",
            r"\mods\FS25_Foo.zip",
            "F:FS25_Foo.zip",
        ] {
            assert_eq!(shell_path(Path::new(relative)), None, "{relative}");
        }
    }

    /// #442: with "no confirmation" Windows deletes permanently without asking where it can't
    /// recycle, so the delete leaves Windows' prompts on, as File Explorer does.
    #[cfg(windows)]
    #[test]
    fn the_delete_keeps_windows_prompts() {
        use windows_sys::Win32::UI::Shell::{
            FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOF_WANTNUKEWARNING,
        };
        let f = windows::DELETE_FLAGS;
        assert_ne!(f & FOF_ALLOWUNDO, 0, "to the Recycle Bin");
        assert_eq!(
            f & FOF_NOCONFIRMATION,
            0,
            "Windows asks before deleting permanently"
        );
        assert_ne!(f & FOF_NOERRORUI, 0, "Silo reports errors itself");
        assert_eq!(f & (FOF_SILENT | FOF_WANTNUKEWARNING), 0);
        assert!(f <= u16::MAX as u32, "fits the shell's 16-bit flags");
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
        assert_eq!(move_to_bin(&f, None), Binned::Recycled);
        assert_eq!(
            move_to_bin(&d.join("FS25_SiloBinTestFolder"), None),
            Binned::Recycled
        );
        assert!(!f.exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}

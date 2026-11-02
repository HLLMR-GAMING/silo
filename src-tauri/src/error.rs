//! Errors the frontend can tell apart. A command returning `Result<T, AppError>` rejects with
//! `{ code, message }`: `message` is for the user, `code` is for code. Commands still returning
//! `Result<T, String>` reject with the bare string; the frontend accepts both (src/lib/errors.ts).
//!
//! Convention for new code: a command whose failure the UI must branch on returns `AppError`
//! with a code from `code`; never match on message text, in Rust or TypeScript. The one place
//! that does read message text is `classify`, which turns the `String` errors the core still
//! produces into a code at the command boundary (`From<String>`), so callers don't have to.

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AppError {
    pub code: &'static str,
    pub message: String,
}

pub mod code {
    pub const STILL_LOADING: &str = "STILL_LOADING";
    pub const LIBRARY_BUSY: &str = "LIBRARY_BUSY";
    /// A library archive move is paused or unfinished (or already under way): it must be
    /// resumed or cancelled before this can run.
    pub const MOVE_UNFINISHED: &str = "MOVE_UNFINISHED";
    /// A library archive move is running right now (it can be paused): e.g. launching the game.
    pub const MOVE_RUNNING: &str = "MOVE_RUNNING";
    /// A paused library archive move left active mods FS25 can't open from the mods folder
    /// (#415): Play asks first, and starts the game only when asked again with "Play anyway".
    pub const MOVE_LINKS_BROKEN: &str = "MOVE_LINKS_BROKEN";
    /// The game hasn't written this file yet (log.txt, inputBinding.xml): FS25 was never played
    /// with this user folder. Not a failure; the UI shows an empty state.
    pub const NO_LOG: &str = "NO_LOG";
    pub const GAME_RUNNING: &str = "GAME_RUNNING";
    /// Text pasted into "Open a shared set" isn't a Silo set link or a GitHub gist.
    pub const NOT_A_SET_LINK: &str = "NOT_A_SET_LINK";
    /// Another program (often the game, an antivirus scan or a sync client) has the file open.
    pub const FILE_IN_USE: &str = "FILE_IN_USE";
    pub const PERMISSION_DENIED: &str = "PERMISSION_DENIED";
    pub const DISK_FULL: &str = "DISK_FULL";
    pub const NOT_FOUND: &str = "NOT_FOUND";
    /// The library archive's drive or folder isn't there (unplugged drive, renamed folder).
    pub const ARCHIVE_UNAVAILABLE: &str = "ARCHIVE_UNAVAILABLE";
    pub const NET_OFFLINE: &str = "NET_OFFLINE";
    pub const NET_TIMEOUT: &str = "NET_TIMEOUT";
    /// GitHub rejected the token (expired, revoked, or missing a permission).
    pub const GITHUB_AUTH: &str = "GITHUB_AUTH";
    pub const GITHUB_RATE_LIMIT: &str = "GITHUB_RATE_LIMIT";
    /// The file isn't a Farming Simulator mod (no modDesc.xml, or not a readable .zip).
    pub const NOT_A_MOD: &str = "NOT_A_MOD";
    /// A download arrived cut off or damaged (not a readable .zip): trying again can help.
    pub const DOWNLOAD_BROKEN: &str = "DOWNLOAD_BROKEN";
    /// A collection changed between its preview and the apply.
    pub const CHANGED_SINCE_PREVIEW: &str = "CHANGED_SINCE_PREVIEW";
    /// The library database was written by a newer Silo.
    pub const DB_NEWER: &str = "DB_NEWER";
    /// Nothing more specific is known. `classify` falls back to this.
    pub const UNKNOWN: &str = "UNKNOWN";
    /// Legacy catch-all, kept because frontend code may already read it. Nothing new emits it;
    /// the UI must treat `OTHER` exactly like `UNKNOWN`.
    pub const OTHER: &str = "OTHER";
}

impl AppError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        AppError {
            code,
            message: message.into(),
        }
    }
}

/// A `String` error from the core gets its code from `classify`; the text is kept as-is.
impl From<String> for AppError {
    fn from(message: String) -> Self {
        let code = classify(&message);
        AppError { code, message }
    }
}

impl From<&str> for AppError {
    fn from(message: &str) -> Self {
        message.to_string().into()
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        let message = e.to_string();
        let code = io_code(e.kind(), e.raw_os_error(), &message, cfg!(windows));
        AppError { code, message }
    }
}

impl From<crate::net::HttpError> for AppError {
    fn from(e: crate::net::HttpError) -> Self {
        use crate::net::{HttpError, RespExt};
        let message = e.to_string();
        let code = match e {
            HttpError::Status(status, s) => {
                let host = url::Url::parse(s.url())
                    .ok()
                    .and_then(|u| u.host_str().map(str::to_ascii_lowercase));
                let rate_header = s.header("x-ratelimit-remaining") == Some("0")
                    || s.header("retry-after").is_some();
                // Only 403/429 bodies are read, and only a little: enough for GitHub's
                // "API rate limit exceeded" JSON.
                let rate_body = matches!(status, 403 | 429) && {
                    use std::io::Read;
                    let mut body = String::new();
                    let _ = s
                        .into_response()
                        .into_reader()
                        .take(8192)
                        .read_to_string(&mut body);
                    body.to_ascii_lowercase().contains("rate limit")
                };
                http_code(status, host.as_deref(), rate_header || rate_body)
                    .unwrap_or_else(|| classify(&message))
            }
            HttpError::Transport(t) => transport_code(t.error(), &message),
        };
        AppError { code, message }
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// The code for an error message. Pure: reads only the text, most specific pattern first —
/// Silo's own refusals, then OS error numbers, then GitHub, then network, then OS error text.
/// Anything else is `UNKNOWN`.
pub fn classify(message: &str) -> &'static str {
    classify_on(message, cfg!(windows))
}

/// `classify` with the OS spelled out: `(os error N)` numbers mean different things on Windows
/// and Unix (32 is a sharing violation on Windows, a broken pipe on Unix). Tests use both.
fn classify_on(message: &str, windows: bool) -> &'static str {
    if message.trim().is_empty() {
        return code::UNKNOWN;
    }
    let lower = message.to_lowercase();
    if let Some(c) = known_phrase(&lower) {
        return c;
    }
    if let Some(c) = os_error_numbers(message).find_map(|n| os_error_code(n, windows)) {
        return c;
    }
    if let Some(c) = github_code(&lower) {
        return c;
    }
    if let Some(c) = network_code(&lower) {
        return c;
    }
    if let Some(c) = os_text_code(&lower) {
        return c;
    }
    code::UNKNOWN
}

/// Fixed texts Silo itself produces for a known condition (lower-cased). Their producers:
/// lib.rs `STILL_LOADING`, `LIBRARY_BUSY`, `MOVE_LOCK_HELD`, `ensure_no_move`, `launch_game`'s
/// move refusal, `UPDATE_GAME_RUNNING`, `SWAP_GAME_RUNNING`,
/// `CONFIG_GAME_RUNNING`, `archive_unavailable`, the collection_apply re-preview refusal;
/// relocate.rs `GAME_RUNNING` and its "already running"; db.rs `newer_schema`;
/// github.rs / siloapi.rs / update.rs not-a-mod and not-a-zip texts and GitHub token/rate texts.
fn known_phrase(lower: &str) -> Option<&'static str> {
    const PHRASES: &[(&str, &str)] = &[
        ("silo is still loading", code::STILL_LOADING),
        ("silo is busy with another change", code::LIBRARY_BUSY),
        ("archive move is already", code::MOVE_UNFINISHED),
        ("already unfinished", code::MOVE_UNFINISHED),
        (
            "finish or cancel the archive move first",
            code::MOVE_UNFINISHED,
        ),
        // Before the plain "moving" phrase: launching while a move runs.
        ("pause the move to play", code::MOVE_RUNNING),
        ("silo is moving your library archive", code::LIBRARY_BUSY),
        ("close farming simulator 25 first", code::GAME_RUNNING),
        ("reconnect its drive", code::ARCHIVE_UNAVAILABLE),
        (
            "changed since you previewed it",
            code::CHANGED_SINCE_PREVIEW,
        ),
        ("saved by a newer version of silo", code::DB_NEWER),
        ("isn't a farming simulator mod", code::NOT_A_MOD),
        ("doesn't look like a silo set link", code::NOT_A_SET_LINK),
        // A download that isn't a readable .zip was cut off or damaged (retry), before the
        // general "not a valid .zip" (a local file that isn't a mod).
        ("download isn't a valid .zip", code::DOWNLOAD_BROKEN),
        ("downloaded file is not a valid .zip", code::DOWNLOAD_BROKEN),
        ("downloaded file isn't a valid .zip", code::DOWNLOAD_BROKEN),
        ("not a valid .zip", code::NOT_A_MOD),
        ("isn't a valid .zip", code::NOT_A_MOD),
        ("invalid zip archive", code::NOT_A_MOD),
        ("github rejected the token", code::GITHUB_AUTH),
        ("this token can't perform that action", code::GITHUB_AUTH),
        ("github rate limit", code::GITHUB_RATE_LIMIT),
    ];
    PHRASES
        .iter()
        .find(|(p, _)| lower.contains(p))
        .map(|&(_, c)| c)
}

/// Every `N` in `(os error N)`, in order. Exact: `(os error 320)` yields 320, never 32.
fn os_error_numbers(message: &str) -> impl Iterator<Item = i32> + '_ {
    const MARK: &str = "(os error ";
    message.match_indices(MARK).filter_map(|(i, _)| {
        let rest = &message[i + MARK.len()..];
        let digits = rest.find(|c: char| !c.is_ascii_digit())?;
        if digits == 0 || !rest[digits..].starts_with(')') {
            return None;
        }
        rest[..digits].parse().ok()
    })
}

/// The code for a raw OS error number, per OS.
fn os_error_code(n: i32, windows: bool) -> Option<&'static str> {
    let c = if windows {
        match n {
            // ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION
            32 | 33 => code::FILE_IN_USE,
            // ERROR_ACCESS_DENIED
            5 => code::PERMISSION_DENIED,
            // ERROR_DISK_FULL, ERROR_HANDLE_DISK_FULL
            112 | 39 => code::DISK_FULL,
            // ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND
            2 | 3 => code::NOT_FOUND,
            // WSAETIMEDOUT
            10060 => code::NET_TIMEOUT,
            // WSAENETUNREACH, WSAECONNREFUSED, WSAEHOSTUNREACH, WSAHOST_NOT_FOUND
            10051 | 10061 | 10065 | 11001 => code::NET_OFFLINE,
            _ => return None,
        }
    } else {
        match n {
            // EPERM, EACCES
            1 | 13 => code::PERMISSION_DENIED,
            // ENOSPC
            28 => code::DISK_FULL,
            // EBUSY, ETXTBSY
            16 | 26 => code::FILE_IN_USE,
            // ENOENT
            2 => code::NOT_FOUND,
            _ => return None,
        }
    };
    Some(c)
}

fn github_code(lower: &str) -> Option<&'static str> {
    if lower.contains("bad credentials") || (lower.contains("github") && has_word(lower, "401")) {
        return Some(code::GITHUB_AUTH);
    }
    let rate_status = has_word(lower, "403") || has_word(lower, "429");
    if lower.contains("rate limit")
        || lower.contains("rate-limit")
        || (rate_status && lower.contains("rate"))
    {
        return Some(code::GITHUB_RATE_LIMIT);
    }
    None
}

fn network_code(lower: &str) -> Option<&'static str> {
    if lower.contains("timed out") || lower.contains("timeout") {
        return Some(code::NET_TIMEOUT);
    }
    const OFFLINE: &[&str] = &[
        "dns failed",
        "failed to lookup address",
        "no such host",
        "connection refused",
        "network is unreachable",
        "host is unreachable",
        "connection failed",
    ];
    if has_word(lower, "dns") || OFFLINE.iter().any(|p| lower.contains(p)) {
        return Some(code::NET_OFFLINE);
    }
    None
}

/// OS error wording without a number (an `io::Error` re-worded or wrapped by a library).
fn os_text_code(lower: &str) -> Option<&'static str> {
    const TEXTS: &[(&str, &str)] = &[
        ("being used by another process", code::FILE_IN_USE),
        ("has locked a portion of the file", code::FILE_IN_USE),
        ("resource busy", code::FILE_IN_USE),
        ("access is denied", code::PERMISSION_DENIED),
        ("permission denied", code::PERMISSION_DENIED),
        ("not enough space on the disk", code::DISK_FULL),
        ("no space left on device", code::DISK_FULL),
        ("cannot find the file specified", code::NOT_FOUND),
        ("cannot find the path specified", code::NOT_FOUND),
        ("no such file or directory", code::NOT_FOUND),
    ];
    TEXTS
        .iter()
        .find(|(p, _)| lower.contains(p))
        .map(|&(_, c)| c)
}

/// `word` occurs in `s` with no letter or digit directly on either side.
fn has_word(s: &str, word: &str) -> bool {
    s.match_indices(word).any(|(i, _)| {
        let before = s[..i].chars().next_back();
        let after = s[i + word.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

/// `From<io::Error>`'s code: the raw OS number first (most specific), then the portable kind,
/// then the text. `StorageFull`/`ResourceBusy` kinds need Rust 1.83 (above our MSRV), so disk
/// full and file in use come from the raw number or the text.
fn io_code(
    kind: std::io::ErrorKind,
    raw: Option<i32>,
    message: &str,
    windows: bool,
) -> &'static str {
    use std::io::ErrorKind;
    if let Some(c) = raw.and_then(|n| os_error_code(n, windows)) {
        return c;
    }
    match kind {
        ErrorKind::PermissionDenied => code::PERMISSION_DENIED,
        ErrorKind::NotFound => code::NOT_FOUND,
        ErrorKind::TimedOut => code::NET_TIMEOUT,
        _ => classify_on(message, windows),
    }
}

/// An HTTP status's code, when the status alone (plus host and rate-limit evidence) says it.
/// 401 means GITHUB_AUTH only from GitHub itself; any other host's 401 isn't ours to name.
fn http_code(status: u16, host: Option<&str>, rate_limited: bool) -> Option<&'static str> {
    let github = matches!(host, Some("api.github.com" | "github.com"));
    match status {
        401 if github => Some(code::GITHUB_AUTH),
        403 | 429 if rate_limited => Some(code::GITHUB_RATE_LIMIT),
        _ => None,
    }
}

/// A ureq transport failure: a timeout is a timeout; DNS failures are offline; refused,
/// reset or failed connections (TLS included) are offline unless the text says it timed out;
/// other kinds (bad URL, bad header…) go by the text alone. The same buckets as ureq 2's
/// `Dns` / `ConnectionFailed | Io | ProxyConnect` kinds.
fn transport_code(e: &ureq::Error, message: &str) -> &'static str {
    use ureq::Error as E;
    let by_text = classify(message);
    match e {
        E::Timeout(_) => code::NET_TIMEOUT,
        E::HostNotFound => code::NET_OFFLINE,
        E::Other(o) if o.is::<crate::net::DnsFailed>() => code::NET_OFFLINE,
        E::Other(o) if o.is::<crate::net::ConnectFailed>() => match by_text {
            code::NET_TIMEOUT | code::NET_OFFLINE => by_text,
            _ => code::NET_OFFLINE,
        },
        E::Io(_) | E::ConnectionFailed | E::ConnectProxyFailed(_) | E::Rustls(_) | E::Tls(_) => {
            match by_text {
                code::NET_TIMEOUT | code::NET_OFFLINE => by_text,
                _ => code::NET_OFFLINE,
            }
        }
        _ => by_text,
    }
}

/// Longest frontend message or context written to the log, in bytes.
const FRONTEND_LOG_MAX: usize = 4096;

/// One frontend-reported event as one log line: the level from `level` ("error" | "warn" |
/// "info", anything else is error), the text prefixed `[frontend]`, newlines folded to ` | `
/// and other control characters to spaces,
/// message and context each cut to 4 KB.
pub fn format_frontend_log(
    level: &str,
    message: &str,
    context: Option<&str>,
) -> (log::Level, String) {
    let level = match level.trim().to_ascii_lowercase().as_str() {
        "warn" | "warning" => log::Level::Warn,
        "info" => log::Level::Info,
        _ => log::Level::Error,
    };
    let mut line = format!("[frontend] {}", one_line(message));
    if let Some(ctx) = context.filter(|c| !c.trim().is_empty()) {
        line.push_str(" | context: ");
        line.push_str(&one_line(ctx));
    }
    (level, line)
}

/// `s` on one line (CR/LF runs → ` | `; every other control character → a space, see
/// `log_safe_char`), cut to `FRONTEND_LOG_MAX` bytes on a char boundary.
fn one_line(s: &str) -> String {
    let flat = mask_secrets(s)
        .split(['\r', '\n'])
        .filter(|part| !part.is_empty())
        .map(|part| part.chars().map(log_safe_char).collect::<String>())
        .collect::<Vec<_>>()
        .join(" | ");
    if flat.len() <= FRONTEND_LOG_MAX {
        return flat;
    }
    let mut end = FRONTEND_LOG_MAX;
    while !flat.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &flat[..end])
}

/// What replaces a masked secret.
const REDACTED: &str = "[redacted]";

/// GitHub token prefixes: fine-grained PATs, classic PATs, OAuth, user-to-server,
/// server-to-server and refresh tokens.
const TOKEN_PREFIXES: [&str; 6] = ["github_pat_", "ghp_", "gho_", "ghu_", "ghs_", "ghr_"];

/// `s` with anything shaped like a credential replaced by `[redacted]`, for text bound for the
/// log or diagnostics: the quoted header in ureq 2's "invalid header '…'" error (which carries the
/// whole `Authorization` line, even across a line break), the word after `Bearer `, and GitHub
/// tokens (a known prefix plus 8+ token characters, not inside a longer word). Callers should
/// never pass secrets; this is the backstop for one that slips through.
pub fn mask_secrets(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let rest = &s[i..];
        let after_word = s[..i]
            .chars()
            .next_back()
            .map_or(true, |c| !(c.is_alphanumeric() || c == '_'));
        if let Some(n) = starts_with_ci(rest, "invalid header '") {
            let quoted = rest[n..].find('\'').unwrap_or(rest.len() - n);
            out.push_str(&rest[..n]);
            out.push_str(REDACTED);
            i += n + quoted;
            continue;
        }
        if after_word {
            if let Some(n) = starts_with_ci(rest, "bearer ") {
                let run = rest[n..]
                    .find(|c: char| c.is_whitespace() || c == '\'' || c == '"')
                    .unwrap_or(rest.len() - n);
                if run > 0 {
                    out.push_str(&rest[..n]);
                    out.push_str(REDACTED);
                    i += n + run;
                    continue;
                }
            }
            if let Some(p) = TOKEN_PREFIXES.iter().find(|p| rest.starts_with(**p)) {
                let run = rest[p.len()..]
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(rest.len() - p.len());
                if run >= 8 {
                    out.push_str(p);
                    out.push_str(REDACTED);
                    i += p.len() + run;
                    continue;
                }
            }
        }
        let c = rest.chars().next().unwrap_or_default();
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// The length of `prefix` (ASCII) when `s` starts with it in any case.
fn starts_with_ci(s: &str, prefix: &str) -> Option<usize> {
    let head = s.as_bytes().get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix.as_bytes())
        .then_some(prefix.len())
}

/// A space for any character that could break or fake a log line: C0 controls (tab included),
/// DEL, NEL (U+0085) and the Unicode line / paragraph separators. CR and LF never get here
/// (`one_line` splits on them).
fn log_safe_char(c: char) -> char {
    match c {
        '\u{0}'..='\u{1f}' | '\u{7f}' | '\u{85}' | '\u{2028}' | '\u{2029}' => ' ',
        _ => c,
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_pasted_non_link_has_its_own_code() {
        assert_eq!(
            classify("That doesn't look like a Silo set link. Links look like https://silo.hllmr.com/c/… or a GitHub gist."),
            code::NOT_A_SET_LINK
        );
    }
    use super::*;

    #[test]
    fn errors_reach_the_webview_as_code_and_message() {
        let e = AppError::new(code::STILL_LOADING, "Silo is still loading");
        assert_eq!(
            serde_json::to_value(&e).unwrap(),
            serde_json::json!({ "code": "STILL_LOADING", "message": "Silo is still loading" })
        );
        let plain: AppError = String::from("boom").into();
        assert_eq!(
            serde_json::to_value(&plain).unwrap(),
            serde_json::json!({ "code": "UNKNOWN", "message": "boom" })
        );
    }

    #[test]
    fn from_string_classifies_and_keeps_the_text() {
        let msg = "Couldn't move FS25_A.zip: The process cannot access the file because it is \
                   being used by another process. (os error 32)";
        let e: AppError = msg.to_string().into();
        assert_eq!(e.code, classify(msg));
        assert_eq!(e.code, code::FILE_IN_USE);
        assert_eq!(e.message, msg);
        assert_eq!(e.to_string(), msg);
    }

    const WIN: bool = true;
    const UNIX: bool = false;

    #[test]
    fn classifier_table() {
        let cases: &[(&str, bool, &str)] = &[
            // File in use
            (
                "Couldn't move FS25_A.zip: The process cannot access the file because it is \
                 being used by another process. (os error 32)",
                WIN,
                code::FILE_IN_USE,
            ),
            (
                "The process cannot access the file because another process has locked a \
                 portion of the file. (os error 33)",
                WIN,
                code::FILE_IN_USE,
            ),
            (
                "rename /a -> /b: Device or resource busy (os error 16)",
                UNIX,
                code::FILE_IN_USE,
            ),
            ("Text file busy (os error 26)", UNIX, code::FILE_IN_USE),
            // Permission denied
            (
                "Couldn't write C:\\Mods\\x.zip: Access is denied. (os error 5)",
                WIN,
                code::PERMISSION_DENIED,
            ),
            (
                "open /mods/x.zip: Permission denied (os error 13)",
                UNIX,
                code::PERMISSION_DENIED,
            ),
            (
                "Operation not permitted (os error 1)",
                UNIX,
                code::PERMISSION_DENIED,
            ),
            // Disk full
            (
                "There is not enough space on the disk. (os error 112)",
                WIN,
                code::DISK_FULL,
            ),
            ("The disk is full. (os error 39)", WIN, code::DISK_FULL),
            ("No space left on device (os error 28)", UNIX, code::DISK_FULL),
            // Not found
            (
                "The system cannot find the file specified. (os error 2)",
                WIN,
                code::NOT_FOUND,
            ),
            (
                "The system cannot find the path specified. (os error 3)",
                WIN,
                code::NOT_FOUND,
            ),
            (
                "No such file or directory (os error 2)",
                UNIX,
                code::NOT_FOUND,
            ),
            // Network
            (
                "https://api.github.com/repos/a/b: Connection Failed: Connect error: A \
                 connection attempt failed because the connected party did not properly \
                 respond after a period of time (os error 10060)",
                WIN,
                code::NET_TIMEOUT,
            ),
            (
                "https://x.test/: Network Error: timed out reading response",
                UNIX,
                code::NET_TIMEOUT,
            ),
            (
                "https://api.github.com/: Dns Failed: resolve dns name 'api.github.com:443': \
                 No such host is known. (os error 11001)",
                WIN,
                code::NET_OFFLINE,
            ),
            (
                "https://api.github.com/: Dns Failed: resolve dns name 'api.github.com:443': \
                 failed to lookup address information: Name or service not known",
                UNIX,
                code::NET_OFFLINE,
            ),
            (
                "https://x.test/: Connection Failed: Connect error: No connection could be made \
                 because the target machine actively refused it. (os error 10061)",
                WIN,
                code::NET_OFFLINE,
            ),
            (
                "https://x.test/: Connection Failed: Connect error: Connection refused (os error 111)",
                UNIX,
                code::NET_OFFLINE,
            ),
            (
                "A socket operation was attempted to an unreachable network. (os error 10051)",
                WIN,
                code::NET_OFFLINE,
            ),
            (
                "Network is unreachable (os error 101)",
                UNIX,
                code::NET_OFFLINE,
            ),
            // GitHub
            (
                "https://api.github.com/user: status code 401",
                WIN,
                code::GITHUB_AUTH,
            ),
            ("Bad credentials", UNIX, code::GITHUB_AUTH),
            (
                "GitHub rejected the token. Reconnect your account.",
                WIN,
                code::GITHUB_AUTH,
            ),
            (
                "GitHub rate limit hit (60/hr unauthenticated). Try again later.",
                UNIX,
                code::GITHUB_RATE_LIMIT,
            ),
            (
                "403 Forbidden: API rate limit exceeded for 1.2.3.4",
                WIN,
                code::GITHUB_RATE_LIMIT,
            ),
            // Silo's own refusals
            (
                "Silo is still loading your library. Try again in a moment.",
                WIN,
                code::STILL_LOADING,
            ),
            (
                "Silo is busy with another change to your library. Try again in a moment.",
                WIN,
                code::LIBRARY_BUSY,
            ),
            (
                "Finish or cancel the archive move first.",
                WIN,
                code::MOVE_UNFINISHED,
            ),
            (
                "Silo is moving your library archive. Pause the move to play.",
                UNIX,
                code::MOVE_RUNNING,
            ),
            (
                "Silo is moving your library archive.",
                WIN,
                code::LIBRARY_BUSY,
            ),
            (
                "An archive move is already under way.",
                UNIX,
                code::MOVE_UNFINISHED,
            ),
            (
                "An archive move is already running.",
                WIN,
                code::MOVE_UNFINISHED,
            ),
            (
                "An archive move is already unfinished. Resume or cancel it first.",
                UNIX,
                code::MOVE_UNFINISHED,
            ),
            (
                "Close Farming Simulator 25 first, then update.",
                WIN,
                code::GAME_RUNNING,
            ),
            (
                "Close Farming Simulator 25 first. Silo won't move mods while the game is using them.",
                UNIX,
                code::GAME_RUNNING,
            ),
            (
                "Your library archive at E:\\Silo isn't available. Reconnect its drive, or use \
                 Choose another to point Silo at where it is now.",
                WIN,
                code::ARCHIVE_UNAVAILABLE,
            ),
            (
                "This collection changed since you previewed it. Preview it again before installing.",
                WIN,
                code::CHANGED_SINCE_PREVIEW,
            ),
            (
                "Your Silo library was saved by a newer version of Silo (database version 3; this \
                 version reads up to 1). Install the latest Silo to keep using it. Nothing was changed.",
                UNIX,
                code::DB_NEWER,
            ),
            (
                "the downloaded file isn't a Farming Simulator mod (no modDesc.xml). It may be a source archive or the wrong release asset.",
                WIN,
                code::NOT_A_MOD,
            ),
            ("Downloaded file is not a valid .zip", UNIX, code::DOWNLOAD_BROKEN),
            (
                "Downloaded file isn't a valid .zip (corrupt or truncated)",
                WIN,
                code::DOWNLOAD_BROKEN,
            ),
            (
                "The download isn't a valid .zip (it may be corrupt or cut off). Try again.",
                WIN,
                code::DOWNLOAD_BROKEN,
            ),
            ("FS25_X.zip is not a valid .zip", WIN, code::NOT_A_MOD),
            ("invalid Zip archive: Could not find EOCD", UNIX, code::NOT_A_MOD),
            // Not matched
            ("", WIN, code::UNKNOWN),
            ("   ", UNIX, code::UNKNOWN),
            ("boom", WIN, code::UNKNOWN),
            ("Something failed (os error 320)", WIN, code::UNKNOWN),
            ("Something failed (os error 320)", UNIX, code::UNKNOWN),
            ("Something failed (os error 1)", WIN, code::UNKNOWN),
            ("Broken pipe (os error 32)", UNIX, code::UNKNOWN),
            ("FS25_Mod401.zip failed on github", WIN, code::UNKNOWN),
            ("https://example.com/: status code 401", WIN, code::UNKNOWN),
        ];
        for &(msg, windows, want) in cases {
            assert_eq!(
                classify_on(msg, windows),
                want,
                "{msg:?} (windows: {windows})"
            );
        }
    }

    #[test]
    fn os_error_numbers_are_exact() {
        let found: Vec<i32> =
            os_error_numbers("a (os error 320) b (os error 32) (os error ) (os error 5x)")
                .collect();
        assert_eq!(found, vec![320, 32]);
    }

    #[test]
    fn io_errors_map_by_kind_then_number_then_text() {
        use std::io::{Error, ErrorKind};
        let denied: AppError = Error::from(ErrorKind::PermissionDenied).into();
        assert_eq!(denied.code, code::PERMISSION_DENIED);
        let missing: AppError = Error::new(ErrorKind::NotFound, "gone").into();
        assert_eq!(
            (missing.code, missing.message.as_str()),
            (code::NOT_FOUND, "gone")
        );
        assert_eq!(
            io_code(ErrorKind::Other, Some(32), "x", WIN),
            code::FILE_IN_USE
        );
        assert_eq!(
            io_code(ErrorKind::Other, Some(28), "x", UNIX),
            code::DISK_FULL
        );
        assert_eq!(
            io_code(ErrorKind::Other, None, "No space left on device", UNIX),
            code::DISK_FULL
        );
        assert_eq!(
            io_code(ErrorKind::TimedOut, None, "x", WIN),
            code::NET_TIMEOUT
        );
        assert_eq!(io_code(ErrorKind::Other, None, "x", WIN), code::UNKNOWN);
        // The real OS error on this platform round-trips through the portable path.
        let real: AppError = Error::from_raw_os_error(2).into();
        assert_eq!(real.code, code::NOT_FOUND);
    }

    #[test]
    fn http_status_codes() {
        assert_eq!(
            http_code(401, Some("api.github.com"), false),
            Some(code::GITHUB_AUTH)
        );
        assert_eq!(
            http_code(401, Some("github.com"), false),
            Some(code::GITHUB_AUTH)
        );
        assert_eq!(http_code(401, Some("siloapi.example"), false), None);
        assert_eq!(http_code(401, None, false), None);
        assert_eq!(
            http_code(403, Some("api.github.com"), true),
            Some(code::GITHUB_RATE_LIMIT)
        );
        assert_eq!(http_code(429, None, true), Some(code::GITHUB_RATE_LIMIT));
        assert_eq!(http_code(403, Some("api.github.com"), false), None);
        assert_eq!(http_code(404, Some("api.github.com"), true), None);
    }

    /// A 4xx/5xx answer from `url`, as `net::CallExt` hands it back.
    fn status_err(url: &str, status: u16, headers: &[(&str, &str)], body: &str) -> AppError {
        let mut b = ureq::http::Response::builder().status(status);
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let resp = b
            .body(ureq::Body::builder().data(body.as_bytes().to_vec()))
            .unwrap();
        crate::net::HttpError::status(url, resp).into()
    }

    #[test]
    fn ureq_status_errors() {
        // A 401 is GitHub's only when GitHub said it; example.com's isn't.
        let e = status_err("https://example.com/", 401, &[], "");
        assert_eq!(e.code, code::UNKNOWN);
        assert!(e.message.ends_with("status code 401"), "{}", e.message);
        let e = status_err("https://api.github.com/user", 401, &[], "");
        assert_eq!(e.code, code::GITHUB_AUTH);
        assert_eq!(e.message, "https://api.github.com/user: status code 401");

        let e = status_err(
            "https://example.com/",
            403,
            &[("x-ratelimit-remaining", "0")],
            "",
        );
        assert_eq!(e.code, code::GITHUB_RATE_LIMIT);
        let e = status_err("https://example.com/", 429, &[("retry-after", "30")], "");
        assert_eq!(e.code, code::GITHUB_RATE_LIMIT);

        let e = status_err(
            "https://example.com/",
            403,
            &[],
            "{\"message\":\"API rate limit exceeded\"}",
        );
        assert_eq!(e.code, code::GITHUB_RATE_LIMIT);

        let e = status_err("https://example.com/", 403, &[], "no");
        assert_eq!(e.code, code::UNKNOWN);
        // Only 403/429 bodies are read: a 404 saying "rate limit" is still just a 404.
        let e = status_err("https://example.com/", 404, &[], "rate limit");
        assert_eq!(e.code, code::UNKNOWN);
    }

    #[test]
    fn ureq_transport_kinds() {
        use crate::net::{DnsFailed, HttpError};
        use std::io::{Error as IoError, ErrorKind as IoKind};
        use ureq::Error as E;
        let app =
            |e: E| -> AppError { HttpError::transport(Some("https://x.test/".into()), e).into() };
        let dns = || DnsFailed::error("x.test:443".into(), IoError::other("no such host"));

        // DNS failures (and a lookup Silo refused) are offline, as ureq 2's Dns kind was.
        let e = app(dns());
        assert_eq!(e.code, code::NET_OFFLINE);
        assert!(e.message.contains("Dns Failed"), "{}", e.message);
        assert_eq!(transport_code(&dns(), "x: Dns Failed"), code::NET_OFFLINE);
        assert_eq!(transport_code(&E::HostNotFound, "x"), code::NET_OFFLINE);
        assert_eq!(
            app(DnsFailed::error(
                "evil.test:443".into(),
                IoError::new(
                    IoKind::PermissionDenied,
                    "refusing evil.test:443: it has no public address"
                ),
            ))
            .code,
            code::NET_OFFLINE
        );
        // A failed connection is offline unless its text says it timed out.
        assert_eq!(
            transport_code(&E::ConnectionFailed, "x: Connection Failed: timed out"),
            code::NET_TIMEOUT
        );
        assert_eq!(
            transport_code(&E::ConnectionFailed, "x: Connection Failed"),
            code::NET_OFFLINE
        );
        assert_eq!(
            transport_code(
                &E::Io(IoError::new(IoKind::ConnectionReset, "connection reset")),
                "x: Network Error: connection reset"
            ),
            code::NET_OFFLINE
        );
        // A connection Silo's connector reports as failed: offline, and its text says so.
        let failed = app(crate::net::ConnectFailed::error(IoError::other(
            "No route to host (os error 113)",
        )));
        assert_eq!(failed.code, code::NET_OFFLINE);
        assert!(
            failed.message.contains("Connection Failed"),
            "{}",
            failed.message
        );
        assert_eq!(AppError::from(failed.message).code, code::NET_OFFLINE);
        let refused = app(E::Io(IoError::new(IoKind::ConnectionRefused, "refused")));
        assert_eq!(refused.code, code::NET_OFFLINE);
        assert!(refused
            .message
            .starts_with("https://x.test/: Network Error: refused"));
        // Every ureq timeout is a timeout.
        for t in [
            ureq::Timeout::Global,
            ureq::Timeout::PerRead,
            ureq::Timeout::Connect,
        ] {
            let e = app(E::Timeout(t));
            assert_eq!(e.code, code::NET_TIMEOUT, "{}", e.message);
            assert!(e.message.contains("timed out"), "{}", e.message);
        }
        // Anything else goes by its text.
        assert_eq!(
            transport_code(&E::BadUri("x".into()), "x: Bad URL"),
            code::UNKNOWN
        );
        assert_eq!(app(E::TooManyRedirects).code, code::UNKNOWN);
        assert_eq!(
            app(E::RequireHttpsOnly("http://x.test/".into())).code,
            code::UNKNOWN
        );
    }

    #[test]
    fn token_shaped_text_is_masked() {
        let cases = [
            (
                "invalid header 'Authorization: Bearer ghp_abcdefgh12345678\u{200b}' then",
                "invalid header '[redacted]' then",
            ),
            (
                "invalid header 'Authorization: Bearer ghp_abcd\nefgh12345678' x",
                "invalid header '[redacted]' x",
            ),
            (
                "auth Bearer abc.def-ghi done",
                "auth Bearer [redacted] done",
            ),
            ("token ghp_0123456789abcdefABCDEF.", "token ghp_[redacted]."),
            (
                "pat github_pat_11ABCDEFG0123456789_xyz end",
                "pat github_pat_[redacted] end",
            ),
            (
                "gho_abcdefghijk and ghs_ABCDEFGHIJ",
                "gho_[redacted] and ghs_[redacted]",
            ),
            // Not tokens: too short, or inside a longer word.
            (
                "ghp_short myghp_0123456789abcdef",
                "ghp_short myghp_0123456789abcdef",
            ),
            ("a Bearer  b", "a Bearer  b"),
            ("Ünïcode ok — caf\u{e9}", "Ünïcode ok — caf\u{e9}"),
        ];
        for (input, want) in cases {
            assert_eq!(mask_secrets(input), want, "{input:?}");
        }
        let (_, line) = format_frontend_log(
            "error",
            "Saving your GitHub token: invalid header 'Authorization: Bearer ghp_abcd\nefgh12345678'",
            Some("Bearer ghp_0123456789abcdef"),
        );
        assert!(
            !line.contains("abcd") && !line.contains("efgh") && !line.contains("0123"),
            "{line}"
        );
    }

    #[test]
    fn frontend_log_lines() {
        let (lvl, line) =
            format_frontend_log("warn", "first\nsecond\r\nthird", Some("view=Library"));
        assert_eq!(lvl, log::Level::Warn);
        assert_eq!(
            line,
            "[frontend] first | second | third | context: view=Library"
        );

        assert_eq!(format_frontend_log("INFO", "x", None).0, log::Level::Info);
        assert_eq!(format_frontend_log("error", "x", None).0, log::Level::Error);
        let (lvl, line) = format_frontend_log("bogus", "x", Some("  "));
        assert_eq!((lvl, line.as_str()), (log::Level::Error, "[frontend] x"));

        // Other control characters can't split or fake a line: each becomes a space.
        let (_, line) = format_frontend_log(
            "error",
            "a\tb\u{0}c\u{1b}[31md\u{7f}e\u{85}f\u{2028}g\u{2029}h",
            Some("x\u{8}y"),
        );
        assert_eq!(line, "[frontend] a b c [31md e f g h | context: x y");

        // Each part is cut to 4 KB on a char boundary, so a huge event stays one bounded line.
        let big = "é".repeat(5000); // 10 000 bytes
        let (_, line) = format_frontend_log("error", &big, Some(&big));
        assert!(!line.contains('\n'));
        let parts: Vec<&str> = line
            .trim_start_matches("[frontend] ")
            .split(" | context: ")
            .collect();
        assert_eq!(parts.len(), 2);
        for p in parts {
            assert!(p.ends_with('…'));
            assert!(p.len() <= FRONTEND_LOG_MAX + '…'.len_utf8());
        }
    }
}

// What Silo says when something fails. Every error the UI shows goes through describeError():
//   const d = describeError(e, "Moving mods");
//   d.title   -> "Silo couldn't move FS25_Foo.zip because another program is using it. ..."
//   formatDetail(d.detail) -> the text behind "Copy details".
// Typed backend errors carry a `code`; older commands still reject with a bare string, which
// classifyMessage() maps from the OS error number or a few well-known phrases.

import { AppError, toAppError } from "./errors";

/** The technical side of an error, for "Copy details" and the activity log. */
export interface ErrorDetail {
  code: string;
  /** The raw error message. */
  message: string;
  /** What Silo was doing ("Moving mods"); "" when not given. */
  op: string;
  /** When it happened, ISO 8601. */
  at: string;
}

export interface DescribedError {
  /** The normalized code the copy came from (OTHER and unknown codes become UNKNOWN). */
  code: string;
  /** The sentence shown to the user. Empty when `silent`. */
  title: string;
  /** A second line under the title: the raw message, when an error Silo has no copy for
   *  (UNKNOWN) already reads as a sentence meant for people, cut to about HUMAN_COPY_MAX
   *  characters on a word boundary. Otherwise absent (the raw text stays behind "Copy details"). */
  body?: string;
  detail: ErrorDetail;
  /** Trying the same thing again can work (show "Try again" when a retry is offered). */
  retryable: boolean;
  /** Background work says nothing about it (STILL_LOADING: the UI simply waits); a user's own
   *  action still shows the title. Always logged. */
  silent: boolean;
}

export const ERROR_CODES = [
  "STILL_LOADING",
  "LIBRARY_BUSY",
  "MOVE_UNFINISHED",
  "MOVE_RUNNING",
  "MOVE_LINKS_BROKEN",
  "GAME_RUNNING",
  "NOT_A_SET_LINK",
  "UNKNOWN",
  "FILE_IN_USE",
  "PERMISSION_DENIED",
  "DISK_FULL",
  "NOT_FOUND",
  "ARCHIVE_UNAVAILABLE",
  "NET_OFFLINE",
  "NET_TIMEOUT",
  "GITHUB_AUTH",
  "GITHUB_RATE_LIMIT",
  "NOT_A_MOD",
  "DOWNLOAD_BROKEN",
  "NO_LOG",
  "CHANGED_SINCE_PREVIEW",
  "DB_NEWER",
] as const;

export type ErrorCode = (typeof ERROR_CODES)[number];

const RETRYABLE: ReadonlySet<string> = new Set([
  "STILL_LOADING",
  "FILE_IN_USE",
  "GAME_RUNNING",
  "LIBRARY_BUSY",
  "NET_OFFLINE",
  "NET_TIMEOUT",
  "ARCHIVE_UNAVAILABLE",
  "DISK_FULL",
  "DOWNLOAD_BROKEN",
  "UNKNOWN",
]);

/** OS error numbers (Windows and Unix errno) Silo knows how to explain. */
const OS_ERRORS: Record<number, ErrorCode> = {
  32: "FILE_IN_USE", // Windows: sharing violation
  33: "FILE_IN_USE", // Windows: lock violation
  16: "FILE_IN_USE", // EBUSY
  26: "FILE_IN_USE", // ETXTBSY
  5: "PERMISSION_DENIED", // Windows: access denied
  13: "PERMISSION_DENIED", // EACCES
  1: "PERMISSION_DENIED", // EPERM
  112: "DISK_FULL", // Windows: disk full
  39: "DISK_FULL", // Windows: handle disk full
  28: "DISK_FULL", // ENOSPC
  2: "NOT_FOUND", // file not found / ENOENT
  3: "NOT_FOUND", // Windows: path not found
  10060: "NET_TIMEOUT", // WSAETIMEDOUT
  10051: "NET_OFFLINE", // WSAENETUNREACH
  10061: "NET_OFFLINE", // WSAECONNREFUSED
  11001: "NET_OFFLINE", // WSAHOST_NOT_FOUND
};

/**
 * Work out a code for an error that arrived without one.
 * @param msg the raw error message
 * @returns one of the known codes; UNKNOWN when nothing matches
 */
export function classifyMessage(msg: string): ErrorCode {
  const os = /\bos error (\d+)\b/i.exec(msg);
  if (os) {
    const code = OS_ERRORS[Number(os[1])];
    if (code) return code;
  }
  // Launching while a move runs: pause it (MOVE_RUNNING), not finish or cancel it.
  if (/pause the move to play/i.test(msg)) return "MOVE_RUNNING";
  if (/finish or cancel the (?:archive|library) move first|archive move is already|already unfinished/i.test(msg)) {
    return "MOVE_UNFINISHED";
  }
  if (/no game log yet|no control settings yet|no log\.txt found|no inputbinding\.xml found/i.test(msg)) return "NO_LOG";
  if (/download(?:ed file)? (?:isn't|is not) a valid \.zip/i.test(msg)) return "DOWNLOAD_BROKEN";
  if (/doesn't look like a silo set link/i.test(msg)) return "NOT_A_SET_LINK";
  if (/rate limit/i.test(msg)) return "GITHUB_RATE_LIMIT";
  if (/bad credentials/i.test(msg) || (/\b401\b/.test(msg) && /github/i.test(msg))) {
    return "GITHUB_AUTH";
  }
  if (/timed out|\btimeout\b/i.test(msg)) return "NET_TIMEOUT";
  if (
    /\bdns\b|failed to lookup address|no such host|connection refused|network is unreachable/i.test(
      msg,
    )
  ) {
    return "NET_OFFLINE";
  }
  return "UNKNOWN";
}

/** "Moving mods" -> "moving mods". */
export function lowerFirst(s: string): string {
  return s ? s[0].toLowerCase() + s.slice(1) : s;
}

/** True on Windows (and when the platform can't be read, since that's where most players are). */
export function isWindowsPlatform(): boolean {
  const nav = (globalThis as { navigator?: Navigator }).navigator;
  if (!nav) return true;
  const hinted = (nav as Navigator & { userAgentData?: { platform?: string } }).userAgentData
    ?.platform;
  const s = `${hinted ?? ""} ${nav.platform ?? ""} ${nav.userAgent ?? ""}`;
  if (/\bwin/i.test(s)) return true;
  return !/mac|linux|x11|cros|android|iphone|ipad/i.test(s);
}

/** The file name a message mentions ("...\FS25_Foo.zip": ... -> "FS25_Foo.zip"), if any. */
export function fileNameIn(msg: string): string | null {
  const m = /(?:^|[\\/\s"'`(])([^\\/\s"'`():]+\.(?:zip|xml|i3d|dds|lua|json|txt|bak))(?![\w.])/i.exec(
    msg,
  );
  return m ? m[1] : null;
}

function copyFor(code: ErrorCode, message: string, op: string): string {
  switch (code) {
    case "FILE_IN_USE": {
      const file = fileNameIn(message);
      const what = file ? `${file}, so Silo couldn't change it` : "a file Silo needs";
      return `Another program is using ${what}. Close Farming Simulator 25 or the program that has it open, then try again.`;
    }
    case "GAME_RUNNING":
      return "Close Farming Simulator 25 first, then try again.";
    case "NOT_A_SET_LINK":
      return "That doesn't look like a Silo set link. Links look like https://silo.hllmr.com/c/… or a link from GitHub.";
    case "LIBRARY_BUSY":
      return "Silo is finishing another change to your library. Try again in a moment.";
    case "MOVE_UNFINISHED":
      return MOVE_UNFINISHED_COPY;
    case "MOVE_RUNNING":
      return "Silo is moving your library archive. Pause the move to play, or wait for it to finish.";
    case "MOVE_LINKS_BROKEN":
      // Play asks about this itself (launch.ts); this copy is for anywhere it isn't asked.
      return "While the library move is paused, some active mods can't be opened from your mods folder. Resume the move from the notice at the top, then play.";
    case "ARCHIVE_UNAVAILABLE":
      return "Your library archive isn't available. Connect the drive, or choose another folder.";
    case "PERMISSION_DENIED":
      return isWindowsPlatform()
        ? "Windows didn't let Silo change this file. If it's in a protected folder, choose a mods folder in your Documents."
        : "Your system didn't let Silo change this file. If it's in a protected folder, choose a mods folder in your Documents.";
    case "DISK_FULL":
      return "There isn't enough space on the drive for this. Free some space and try again.";
    case "NOT_FOUND":
      return "Silo couldn't find a file it needed. It may have been moved or deleted. Rescan and try again.";
    case "NET_OFFLINE":
      return "Silo can't reach the internet right now.";
    case "NET_TIMEOUT":
      return "The connection stopped responding. Try again in a moment.";
    case "GITHUB_AUTH":
      return "GitHub needs you to sign in again.";
    case "GITHUB_RATE_LIMIT":
      return githubConnected
        ? "GitHub is limiting requests for a while. Try again in a few minutes."
        : "GitHub is limiting requests for a while. Connecting your GitHub account raises the limit.";
    case "NOT_A_MOD":
      return "That file isn't a Farming Simulator mod.";
    case "DOWNLOAD_BROKEN":
      return "The download was cut off or damaged. Try again.";
    case "NO_LOG":
      return NO_LOG_COPY;
    case "CHANGED_SINCE_PREVIEW":
      return "This set changed after you opened it. Preview it again.";
    case "DB_NEWER":
      return "This library was used by a newer Silo. Install the latest Silo to open it.";
    case "STILL_LOADING":
      return "Silo is still loading your library. Try again in a moment.";
    case "UNKNOWN":
      return `Something went wrong while ${op ? lowerFirst(op) : "doing that"}.`;
  }
}

/** No game log (or controls file) yet: FS25 hasn't been played. An empty state, not a failure. */
export const NO_LOG_COPY = "No game log yet. Play Farming Simulator 25 once, then come back here.";

let githubConnected = false;

/** Whether a GitHub account is connected (githubAuth sets it): the rate-limit copy only suggests
 *  connecting when it isn't. */
export function setGithubConnected(connected: boolean): void {
  githubConnected = connected;
}

/** What Silo says when a library write is refused because an archive move is paused/unfinished. */
export const MOVE_UNFINISHED_COPY =
  "Finish or cancel the library move first. You can resume or cancel it from the notice at the top.";

const KNOWN: ReadonlySet<string> = new Set(ERROR_CODES);

/** Longest raw message shown as a body line; longer ones are cut (the rest stays in Copy details). */
export const HUMAN_COPY_MAX = 240;

/**
 * Cut `text` to about `max` characters, on a word boundary when there is one, adding "…".
 * @param text the text
 * @param max the most characters kept before the ellipsis
 */
export function truncateWords(text: string, max: number = HUMAN_COPY_MAX): string {
  if (text.length <= max) return text;
  const cut = text.slice(0, max);
  // The cut already ends a word when the next character is a space.
  const space = /\s/.test(text[max]) ? max : cut.search(/\s\S*$/);
  const kept = space >= max * 0.6 ? cut.slice(0, space) : cut;
  return `${kept.replace(/[\s,;:.\-]+$/, "")}…`;
}

/**
 * Whether a raw error message reads as copy written for people ("Close FS25 first.") rather
 * than technical text: no OS error number, no `Error:` prefix, no stack-trace lines, not just a
 * path. Long ones still count; describeError cuts them with truncateWords.
 * @param msg the raw message
 */
export function looksLikeHumanCopy(msg: string): boolean {
  const m = msg.trim();
  if (!m) return false;
  if (/\(os error\b/i.test(m)) return false;
  if (/^[\w.]*error:/i.test(m)) return false;
  if (/^\s*at\s/m.test(m)) return false;
  if (!/\s/.test(m) && /[\\/]/.test(m)) return false; // a bare path
  return true;
}

/**
 * Turn any thrown value into what the user sees.
 * @param e a thrown value (AppError, `{code, message}`, string, Error...)
 * @param op what Silo was doing, as a phrase that fits "while ...": "Moving mods"
 * @returns the copy, the technical detail, and whether retrying can help
 */
export function describeError(e: unknown, op?: string): DescribedError {
  const err = toAppError(e);
  const raw = err.code;
  let code: ErrorCode;
  if (raw === null) code = classifyMessage(err.message);
  else if (KNOWN.has(raw)) code = raw as ErrorCode;
  else code = "UNKNOWN"; // OTHER, or a code this build doesn't know yet
  const opText = op ?? "";
  // Only for messages Silo's backend wrote (strings, `{code, message}`, AppError): a JS Error is
  // a bug's own words ("Cannot read properties of undefined"), never shown as copy.
  const fromCode = !(e instanceof Error) || e instanceof AppError;
  const body =
    code === "UNKNOWN" && fromCode && looksLikeHumanCopy(err.message)
      ? truncateWords(err.message.trim())
      : undefined;
  return {
    code,
    title: copyFor(code, err.message, opText),
    ...(body !== undefined ? { body } : {}),
    detail: {
      code: raw ?? code,
      message: err.message,
      op: opText,
      at: new Date().toISOString(),
    },
    retryable: RETRYABLE.has(code),
    silent: code === "STILL_LOADING",
  };
}

let knownVersion: string | null = null;

/** Remember the app version so formatDetail() can include it (fetched once at startup). */
export function rememberAppVersion(version: string | null): void {
  knownVersion = version;
}

/**
 * The text behind "Copy details": one fact per line, for bug reports.
 * @param d the error's detail
 * @param version Silo's version; defaults to the one remembered at startup
 */
export function formatDetail(d: ErrorDetail, version: string | null = knownVersion): string {
  const lines = [`Silo ${version ?? "(version unknown)"}`, `Time: ${d.at}`];
  if (d.op) lines.push(`While: ${d.op}`);
  lines.push(`Code: ${d.code}`, `Message: ${d.message}`);
  return lines.join("\n");
}

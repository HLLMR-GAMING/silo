import { afterEach, describe, expect, it, vi } from "vitest";
import { AppError } from "./errors";
import {
  classifyMessage,
  describeError,
  ERROR_CODES,
  setGithubConnected,
  fileNameIn,
  formatDetail,
  HUMAN_COPY_MAX,
  looksLikeHumanCopy,
  rememberAppVersion,
  truncateWords,
} from "./errorCopy";

afterEach(() => {
  vi.unstubAllGlobals();
  rememberAppVersion(null);
});

const WINDOWS_UA = { userAgent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64)", platform: "Win32" };
const LINUX_UA = { userAgent: "Mozilla/5.0 (X11; Linux x86_64)", platform: "Linux x86_64" };

describe("describeError copy", () => {
  const table: [code: string, title: string, retryable: boolean][] = [
    ["FILE_IN_USE", "Another program is using a file Silo needs. Close Farming Simulator 25 or the program that has it open, then try again.", true],
    ["GAME_RUNNING", "Close Farming Simulator 25 first, then try again.", true],
    ["NOT_A_SET_LINK", "That doesn't look like a Silo set link. Links look like https://silo.hllmr.com/c/… or a link from GitHub.", false],
    ["LIBRARY_BUSY", "Silo is finishing another change to your library. Try again in a moment.", true],
    ["MOVE_UNFINISHED", "Finish or cancel the library move first. You can resume or cancel it from the notice at the top.", false],
    ["MOVE_RUNNING", "Silo is moving your library archive. Pause the move to play, or wait for it to finish.", false],
    ["MOVE_LINKS_BROKEN", "While the library move is paused, some active mods can't be opened from your mods folder. Resume the move from the notice at the top, then play.", false],
    ["ARCHIVE_UNAVAILABLE", "Your library archive isn't available. Connect the drive, or choose another folder.", true],
    ["PERMISSION_DENIED", "Windows didn't let Silo change this file. If it's in a protected folder, choose a mods folder in your Documents.", false],
    ["DISK_FULL", "There isn't enough space on the drive for this. Free some space and try again.", true],
    ["NOT_FOUND", "Silo couldn't find a file it needed. It may have been moved or deleted. Rescan and try again.", false],
    ["NET_OFFLINE", "Silo can't reach the internet right now.", true],
    ["NET_TIMEOUT", "The connection stopped responding. Try again in a moment.", true],
    ["GITHUB_AUTH", "GitHub needs you to sign in again.", false],
    ["GITHUB_RATE_LIMIT", "GitHub is limiting requests for a while. Connecting your GitHub account raises the limit.", false],
    ["NOT_A_MOD", "That file isn't a Farming Simulator mod.", false],
    ["DOWNLOAD_BROKEN", "The download was cut off or damaged. Try again.", true],
    ["NO_LOG", "No game log yet. Play Farming Simulator 25 once, then come back here.", false],
    ["CHANGED_SINCE_PREVIEW", "This set changed after you opened it. Preview it again.", false],
    ["DB_NEWER", "This library was used by a newer Silo. Install the latest Silo to open it.", false],
    ["UNKNOWN", "Something went wrong while moving mods.", true],
  ];

  it.each(table)("%s", (code, title, retryable) => {
    vi.stubGlobal("navigator", WINDOWS_UA);
    const d = describeError({ code, message: "raw text" }, "Moving mods");
    expect(d).toMatchObject({ code, title, retryable, silent: false });
    expect(d.detail).toMatchObject({ code, message: "raw text", op: "Moving mods" });
    expect(Number.isNaN(Date.parse(d.detail.at))).toBe(false);
  });

  it("covers every code", () => {
    const covered = new Set([...table.map((r) => r[0]), "STILL_LOADING"]);
    expect([...ERROR_CODES].filter((c) => !covered.has(c))).toEqual([]);
  });

  it("copy is plain: no exclamation marks or em-dashes", () => {
    for (const code of ERROR_CODES) {
      const t = describeError(new AppError(code, "x"), "Doing it").title;
      expect(t).not.toMatch(/[!\u2014]/);
    }
  });

  it("STILL_LOADING is silent for background work, with copy for a user's own action", () => {
    const d = describeError(new AppError("STILL_LOADING", "Silo is still loading"));
    expect(d).toMatchObject({
      code: "STILL_LOADING",
      title: "Silo is still loading your library. Try again in a moment.",
      silent: true,
      retryable: true,
    });
  });

  it("classifies archive-move refusals without a code as MOVE_UNFINISHED", () => {
    for (const msg of [
      "Finish or cancel the archive move first.",
      "An archive move is already running.",
      "An archive move is already unfinished. Resume or cancel it first.",
    ]) {
      expect(classifyMessage(msg)).toBe("MOVE_UNFINISHED");
      expect(describeError(msg).retryable).toBe(false);
    }
  });

  it("launching while a move runs says to pause it, not to finish or cancel it (L2)", () => {
    expect(classifyMessage("Silo is moving your library archive. Pause the move to play.")).toBe("MOVE_RUNNING");
  });

  it("a cut-off download can be tried again; a local file that isn't a mod can't (L11)", () => {
    for (const msg of [
      "The download isn't a valid .zip (it may be corrupt or cut off). Try again.",
      "Downloaded file is not a valid .zip",
    ]) {
      expect(classifyMessage(msg)).toBe("DOWNLOAD_BROKEN");
      expect(describeError(msg).retryable).toBe(true);
    }
    expect(describeError({ code: "NOT_A_MOD", message: "x" }).retryable).toBe(false);
  });

  it("no log yet is its own code (L5)", () => {
    expect(classifyMessage("No log.txt found. Launch FS25 at least once first.")).toBe("NO_LOG");
    expect(classifyMessage("No game log yet. Play Farming Simulator 25 once, then come back here.")).toBe("NO_LOG");
  });

  it("the rate-limit copy suggests connecting only when GitHub isn't connected (L12)", () => {
    setGithubConnected(true);
    expect(describeError({ code: "GITHUB_RATE_LIMIT", message: "x" }).title).toBe(
      "GitHub is limiting requests for a while. Try again in a few minutes.",
    );
    setGithubConnected(false);
    expect(describeError({ code: "GITHUB_RATE_LIMIT", message: "x" }).title).toMatch(/Connecting your GitHub account/);
  });

  it("OTHER and codes this build doesn't know read as UNKNOWN, keeping the raw code in detail", () => {
    const other = describeError({ code: "OTHER", message: "x" }, "Saving");
    expect(other.code).toBe("UNKNOWN");
    expect(other.title).toBe("Something went wrong while saving.");
    expect(other.detail.code).toBe("OTHER");
    expect(describeError({ code: "BRAND_NEW", message: "x" }).code).toBe("UNKNOWN");
  });

  it("interpolates the op, lowercased first, with a default", () => {
    expect(describeError("boom", "Checking for updates").title).toBe(
      "Something went wrong while checking for updates.",
    );
    expect(describeError("boom").title).toBe("Something went wrong while doing that.");
    expect(describeError("boom").detail.op).toBe("");
  });

  it("names the file when a FILE_IN_USE message mentions one", () => {
    const msg =
      'failed to rename "C:\\Users\\me\\Documents\\My Games\\FarmingSimulator2025\\mods\\FS25_Big_Tractor.zip": The process cannot access the file because it is being used by another process. (os error 32)';
    expect(describeError(msg).title).toBe(
      "Another program is using FS25_Big_Tractor.zip, so Silo couldn't change it. Close Farming Simulator 25 or the program that has it open, then try again.",
    );
  });

  it("says 'Your system' for PERMISSION_DENIED off Windows", () => {
    vi.stubGlobal("navigator", LINUX_UA);
    expect(describeError(new AppError("PERMISSION_DENIED", "x")).title).toMatch(
      /^Your system didn't let Silo change this file\./,
    );
    vi.stubGlobal("navigator", { userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) darwin", platform: "MacIntel" });
    expect(describeError(new AppError("PERMISSION_DENIED", "x")).title).toMatch(/^Your system/);
  });
});

describe("classifyMessage", () => {
  const cases: [msg: string, code: string][] = [
    ["The process cannot access the file because it is being used by another process. (os error 32)", "FILE_IN_USE"],
    ["The process cannot access the file because another process has locked a portion of the file. (os error 33)", "FILE_IN_USE"],
    ["Device or resource busy (os error 16)", "FILE_IN_USE"],
    ["Text file busy (os error 26)", "FILE_IN_USE"],
    ["Access is denied. (os error 5)", "PERMISSION_DENIED"],
    ["Permission denied (os error 13)", "PERMISSION_DENIED"],
    ["Operation not permitted (os error 1)", "PERMISSION_DENIED"],
    ["There is not enough space on the disk. (os error 112)", "DISK_FULL"],
    ["The disk is full. (os error 39)", "DISK_FULL"],
    ["No space left on device (os error 28)", "DISK_FULL"],
    ["The system cannot find the file specified. (os error 2)", "NOT_FOUND"],
    ["No such file or directory (os error 2)", "NOT_FOUND"],
    ["The system cannot find the path specified. (os error 3)", "NOT_FOUND"],
    ["A connection attempt failed because the connected party did not properly respond (os error 10060)", "NET_TIMEOUT"],
    ["error sending request: operation timed out", "NET_TIMEOUT"],
    ["request timeout after 30s", "NET_TIMEOUT"],
    ["No connection could be made because the target machine actively refused it. (os error 10061)", "NET_OFFLINE"],
    ["A socket operation was attempted to an unreachable network. (os error 10051)", "NET_OFFLINE"],
    ["No such host is known. (os error 11001)", "NET_OFFLINE"],
    ["error trying to connect: dns error: failed to lookup address information", "NET_OFFLINE"],
    ["tcp connect error: Connection refused (os error 111)", "NET_OFFLINE"],
    ["Network is unreachable (os error 101)", "NET_OFFLINE"],
    ["GitHub said: Bad credentials", "GITHUB_AUTH"],
    ["GitHub API returned 401 Unauthorized", "GITHUB_AUTH"],
    ["API rate limit exceeded for 1.2.3.4", "GITHUB_RATE_LIMIT"],
    ["something odd happened", "UNKNOWN"],
    ["HTTP 401 from example.com", "UNKNOWN"],
  ];
  it.each(cases)("%s -> %s", (msg, code) => expect(classifyMessage(msg)).toBe(code));

  it("matches whole OS error numbers only (320 is not 32, 12 is not 1 or 2)", () => {
    expect(classifyMessage("weird (os error 320)")).toBe("UNKNOWN");
    expect(classifyMessage("weird (os error 12)")).toBe("UNKNOWN");
    expect(classifyMessage("weird (os error 1120)")).toBe("UNKNOWN");
  });

  it("a code-less error uses the classification", () => {
    vi.stubGlobal("navigator", WINDOWS_UA);
    const d = describeError(new AppError(null, "Access is denied. (os error 5)"));
    expect(d.code).toBe("PERMISSION_DENIED");
    expect(d.detail.code).toBe("PERMISSION_DENIED");
  });
});

describe("fileNameIn", () => {
  it("takes the base name of a path", () => {
    expect(fileNameIn('rename "/home/me/mods/FS25_A.zip" failed')).toBe("FS25_A.zip");
    expect(fileNameIn("C:\\mods\\FS25_B.zip: in use")).toBe("FS25_B.zip");
    expect(fileNameIn("no file here")).toBeNull();
  });
});

describe("formatDetail", () => {
  it("lists version, time, op, code and message", () => {
    const d = { code: "FILE_IN_USE", message: "in use (os error 32)", op: "Moving mods", at: "2026-10-17T12:00:00.000Z" };
    expect(formatDetail(d, "0.11.0")).toBe(
      "Silo 0.11.0\nTime: 2026-10-17T12:00:00.000Z\nWhile: Moving mods\nCode: FILE_IN_USE\nMessage: in use (os error 32)",
    );
  });
  it("uses the remembered version, and copes without one", () => {
    const d = { code: "UNKNOWN", message: "x", op: "", at: "t" };
    expect(formatDetail(d)).toBe("Silo (version unknown)\nTime: t\nCode: UNKNOWN\nMessage: x");
    rememberAppVersion("1.2.3");
    expect(formatDetail(d).split("\n")[0]).toBe("Silo 1.2.3");
  });
});

describe("body line for errors without copy (#228)", () => {
  it("shows a human-looking backend message under the generic title", () => {
    const d = describeError("Close the other Silo window first.", "Saving the loadout");
    expect(d.title).toBe("Something went wrong while saving the loadout.");
    expect(d.body).toBe("Close the other Silo window first.");
    const typed = describeError({ code: "OTHER", message: "That loadout is gone." }, "Saving");
    expect(typed.body).toBe("That loadout is gone.");
    expect(describeError(new AppError(null, "Nothing to cancel.")).body).toBe("Nothing to cancel.");
  });

  it("leaves technical text behind Copy details", () => {
    const cases = [
      "weird failure (os error 1234)",
      "Error: invalid zip header",
      "error: something broke",
      "TypeError: x is undefined",
      "boom\n    at foo (bar.js:1:2)",
      "C:\\Users\\me\\mods\\FS25_X.zip",
      "/home/me/mods/FS25_X.zip",
    ];
    for (const msg of cases) expect(describeError(msg, "Saving").body).toBeUndefined();
  });

  it("never shows a JS Error's own words, and never for codes with their own copy", () => {
    expect(describeError(new Error("Cannot read properties of undefined"), "Saving").body).toBeUndefined();
    expect(describeError(new AppError("GAME_RUNNING", "FS25 is running.")).body).toBeUndefined();
    expect(describeError("Access is denied. (os error 5)").body).toBeUndefined();
  });

  it("a long human message keeps its body, cut on a word boundary (the full text is in detail)", () => {
    const words = "Silo couldn't finish linking these mods because the folder changed while it worked ";
    const long = words.repeat(5).trim();
    expect(long.length).toBeGreaterThan(HUMAN_COPY_MAX);
    const d = describeError(long, "Saving");
    expect(d.body!.endsWith("…")).toBe(true);
    expect(d.body!.length).toBeLessThanOrEqual(HUMAN_COPY_MAX + 1);
    // Cut between words: what's kept is a prefix of the message ending in a whole word.
    const kept = d.body!.slice(0, -1);
    expect(long.startsWith(kept)).toBe(true);
    expect(long[kept.length]).toBe(" ");
    expect(d.detail.message).toBe(long);
  });

  it("truncateWords: short text unchanged, no space to cut at falls back to a hard cut", () => {
    expect(truncateWords("Short.")).toBe("Short.");
    expect(truncateWords("x".repeat(300))).toBe(`${"x".repeat(HUMAN_COPY_MAX)}…`);
    expect(truncateWords("aaaa bbbb, cccc", 11)).toBe("aaaa bbbb…");
  });

  it("looksLikeHumanCopy: the rule on its own", () => {
    expect(looksLikeHumanCopy("Couldn't link FS25_X: the file changed.")).toBe(true);
    expect(looksLikeHumanCopy("x".repeat(240))).toBe(true);
    expect(looksLikeHumanCopy("word ".repeat(100))).toBe(true);
    expect(looksLikeHumanCopy("   ")).toBe(false);
    expect(looksLikeHumanCopy("C:\\mods")).toBe(false);
  });
});

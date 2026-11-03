// The Updates view's sections, from one check's rows and the library as it is now. Pure, so the
// grouping, the rail badge and the ModHub "you saved the new zip" detection unit-test without the
// app.
//   const s = groupUpdates({ mods, rows, checkYourself, catalogReached: true });
//   s.ready     // newer version Silo can install in place (a GitHub release)
//   s.modhub    // newer version that downloads from its page (ModHub)
//   updatesBadge(s, checked)  // the rail count: ready + modhub, only after a check

import type { ForeignFile } from "../api";
import type { CheckYourselfRow } from "../sources";
import type { UpdateRow } from "../types";
import { displayVersion } from "../format";

/** What the grouping reads from a library mod. */
export interface LibraryModLike {
  techName: string;
  title?: string | null;
  version?: string | null;
  /** Where the mod's file is now (Organize or auto-file can move it after a check). */
  path?: string;
}

/** A mod Silo's catalog doesn't know, with no linked repo and no saved page. */
export interface UntrackedMod {
  techName: string;
  title: string;
}

export interface UpdateSections {
  /** A newer version Silo can download and swap in (a release asset). */
  ready: UpdateRow[];
  /** A newer version that only downloads from its page (ModHub). */
  modhub: UpdateRow[];
  /** A saved source page the check couldn't answer: the user looks for themselves. */
  checkYourself: CheckYourselfRow[];
  /** The check failed for these (a linked repo that didn't answer), with the reason. */
  failed: UpdateRow[];
  upToDate: UpdateRow[];
  /** Only filled when the catalog answered (otherwise Silo can't tell). */
  notTracked: UntrackedMod[];
  /** Mods the user switched update checks off for (#333): never checked, never in Update all. */
  skipped: UntrackedMod[];
}

export const NO_SECTIONS: UpdateSections = {
  ready: [],
  modhub: [],
  checkYourself: [],
  failed: [],
  upToDate: [],
  notTracked: [],
  skipped: [],
};

/** A version as compared and shown: no leading "v", "V." or "Ver ", no surrounding space (displayVersion). */
export function normVersion(v: string): string {
  return displayVersion(v);
}

/**
 * Whether version `a` is newer than `b`, comparing dot-separated numbers ("1.0.10.0" > "1.0.9.0";
 * missing parts count as 0). Versions that aren't plain numbers are never called newer.
 * @param a candidate
 * @param b baseline
 */
export function isNewerVersion(a: string, b: string): boolean {
  const parse = (v: string) => normVersion(v).split(".").map((p) => (/^\d+$/.test(p) ? Number(p) : NaN));
  const x = parse(a);
  const y = parse(b);
  if (x.some(Number.isNaN) || y.some(Number.isNaN)) return false;
  for (let i = 0; i < Math.max(x.length, y.length); i++) {
    const d = (x[i] ?? 0) - (y[i] ?? 0);
    if (d !== 0) return d > 0;
  }
  return false;
}

/**
 * Whether the library already has the version a row offers (updated since the check, by Silo or
 * by hand), so the row stops offering it.
 * @param mod the library mod now
 * @param row the check's row for it
 */
export function installedSinceCheck(mod: LibraryModLike, row: UpdateRow): boolean {
  if (!mod.version || !row.latest) return false;
  if (normVersion(mod.version) === normVersion(row.latest)) return true;
  // The same or a newer version ("v1.2.0" is 1.2.0.0; a 1.3 installed by hand over an offered
  // 1.2): offering the row would re-download it, or put the older one back.
  return isPlainVersion(mod.version) && isPlainVersion(row.latest) && !isNewerVersion(row.latest, mod.version);
}

/** Dot-separated numbers only (an optional leading "v"), so `isNewerVersion` can order it. */
function isPlainVersion(v: string): boolean {
  return /^\d+(\.\d+)*$/.test(normVersion(v));
}

// One shared collator: localeCompare with an options object builds a new one per call, which is
// tens of ms on a 2,500-mod "Not tracked" list (#258).
const titleCollator = new Intl.Collator(undefined, { sensitivity: "base" });
const byTitle = (a: { title: string }, b: { title: string }) => titleCollator.compare(a.title, b.title);

/**
 * Sort one check's rows into the Updates view's sections.
 * @param opts `mods` the library now; `rows` the check's rows; `checkYourself` saved pages the
 *   check couldn't answer; `catalogReached` false when the catalog request failed (then no mod is
 *   called "not tracked", because Silo can't tell); `checkedTechs` the mods the check asked about
 * @returns each section sorted by title; rows for mods no longer in the library are dropped
 */
export function groupUpdates(opts: {
  mods: LibraryModLike[];
  rows: UpdateRow[];
  checkYourself: CheckYourselfRow[];
  catalogReached: boolean;
  /** The mods the check asked about; one added since isn't called "not tracked". */
  checkedTechs?: ReadonlySet<string>;
  /** Mods with update checks switched off: listed only under Not checked, whatever a check said. */
  skipped?: ReadonlySet<string>;
}): UpdateSections {
  const skipped = opts.skipped ?? new Set<string>();
  const byTech = new Map(opts.mods.filter((m) => !skipped.has(m.techName)).map((m) => [m.techName, m]));
  const out: UpdateSections = {
    ready: [],
    modhub: [],
    checkYourself: opts.checkYourself.filter((r) => !skipped.has(r.techName)),
    failed: [],
    upToDate: [],
    notTracked: [],
    skipped: opts.mods
      .filter((m) => skipped.has(m.techName))
      .map((m) => ({ techName: m.techName, title: m.title || m.techName })),
  };
  const answered = new Set<string>(opts.checkYourself.map((r) => r.techName));
  for (const checked of opts.rows) {
    const mod = byTech.get(checked.techName);
    if (!mod) continue;
    // Install from where the file is now, not where it was at check time.
    const row = mod.path && mod.path !== checked.path ? { ...checked, path: mod.path } : checked;
    if (typeof row.hasUpdate !== "boolean") {
      if (row.error) {
        out.failed.push(row);
        answered.add(row.techName);
      }
      continue;
    }
    answered.add(row.techName);
    if (!row.hasUpdate) out.upToDate.push(row);
    else if (installedSinceCheck(mod, row))
      out.upToDate.push({ ...row, hasUpdate: false, current: mod.version ?? row.current });
    else if (row.assetUrl) out.ready.push(row);
    else out.modhub.push(row);
  }
  if (opts.catalogReached) {
    for (const m of byTech.values()) {
      if (!answered.has(m.techName) && (!opts.checkedTechs || opts.checkedTechs.has(m.techName)))
        out.notTracked.push({ techName: m.techName, title: m.title || m.techName });
    }
  }
  out.ready.sort(byTitle);
  out.modhub.sort(byTitle);
  out.failed.sort(byTitle);
  out.upToDate.sort(byTitle);
  out.notTracked.sort(byTitle);
  out.skipped.sort(byTitle);
  return out;
}

/** The rows with a newer version (Ready to install, then Download from ModHub). */
export function availableRows(s: UpdateSections): UpdateRow[] {
  return [...s.ready, ...s.modhub];
}

/**
 * The rail badge: Ready to install + Download from ModHub. No badge before a check (it means
 * "never checked", not "0"), and none when nothing is newer.
 * @param s the sections
 * @param checked a check has finished this session
 */
export function updatesBadge(s: UpdateSections, checked: boolean): number | undefined {
  const n = s.ready.length + s.modhub.length;
  return checked && n > 0 ? n : undefined;
}

/**
 * The file in the mods folder that's a newer build of a ModHub row's mod (the zip the user saved
 * from the page): the version the check found, or anything newer than the installed one. An older
 * or same build (a rollback, a multiplayer server's copy) isn't offered as "newer".
 * @param row a Download from ModHub row
 * @param foreign files at a managed mod's name that Silo didn't put there
 */
export function droppedBuildFor(row: UpdateRow, foreign: ForeignFile[]): ForeignFile | null {
  return (
    foreign.find(
      (f) =>
        // The library copy changing (#431) leaves the older one in the mods folder: no drop.
        !f.archiveChanged &&
        f.techName === row.techName &&
        !!f.flatVersion &&
        ((!!row.latest && normVersion(f.flatVersion) === normVersion(row.latest)) ||
          isNewerVersion(f.flatVersion, f.managedVersion ?? row.current ?? "")),
    ) ?? null
  );
}

/** Identifies one dropped build, so each is announced once. */
export function droppedKey(f: ForeignFile): string {
  return `${f.techName}\u0000${f.fileName}\u0000${f.flatVersion ?? ""}`;
}

/**
 * ModHub builds that newly arrived in the mods folder: one "Found a newer ..." toast each.
 * @param modhub the Download from ModHub rows
 * @param foreign the latest foreign-file scan
 * @param announced keys already announced (droppedKey); not changed here
 */
export function newDroppedBuilds(
  modhub: UpdateRow[],
  foreign: ForeignFile[],
  announced: ReadonlySet<string>,
): { row: UpdateRow; file: ForeignFile }[] {
  const out: { row: UpdateRow; file: ForeignFile }[] = [];
  for (const row of modhub) {
    const file = droppedBuildFor(row, foreign);
    if (file && !announced.has(droppedKey(file))) out.push({ row, file });
  }
  return out;
}

/** "8.0.0.4 → 8.0.1.0" (never a doubled "v"); just the new version when the old is unknown. */
export function versionChange(row: Pick<UpdateRow, "current" | "latest">): string {
  const to = row.latest ? normVersion(row.latest) : "?";
  return row.current ? `${normVersion(row.current)} → ${to}` : to;
}

/**
 * Is this a page on ModHub (farming-simulator.com)? Read as a URL and judged by its host, so a
 * link that only mentions the name elsewhere (another site's path or query, or a look-alike host
 * such as farming-simulator.com.example.net) isn't labelled ModHub (#358).
 * @param url the page link, if any
 */
export function isModHubPage(url: string | null | undefined): boolean {
  if (!url) return false;
  let u: URL;
  try {
    u = new URL(url);
  } catch {
    return false;
  }
  if (u.protocol !== "https:") return false;
  const host = u.hostname.toLowerCase();
  return host === "farming-simulator.com" || host.endsWith(".farming-simulator.com");
}

/** The source chip's words: "GitHub", "ModHub", or the source as given. */
export function sourceLabel(row: Pick<UpdateRow, "source" | "pageUrl">): string | null {
  const s = row.source?.toLowerCase();
  if (s === "github") return "GitHub";
  if (s === "modhub") return "ModHub";
  if (!s && isModHubPage(row.pageUrl)) return "ModHub";
  return row.source ?? null;
}

const relative = new Intl.RelativeTimeFormat("en", { numeric: "auto" });

/**
 * When the last check ran, for the header: "just now", "2 minutes ago", "3 hours ago", then a
 * date.
 * @param at epoch ms of the check
 * @param now epoch ms now
 */
export function lastCheckedText(at: number, now: number): string {
  const secs = Math.max(0, Math.round((now - at) / 1000));
  if (secs < 60) return "just now";
  const mins = Math.floor(secs / 60);
  if (mins < 60) return relative.format(-mins, "minute");
  const hours = Math.floor(mins / 60);
  if (hours < 24) return relative.format(-hours, "hour");
  return new Date(at).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

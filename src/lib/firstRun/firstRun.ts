// First run (#250): who sees the setup flow, who sees the one-time "What's new" card, and the
// organize summary both the setup flow and the Organize dialog show.
//   const which = firstRunScreen(await firstRunState());   // "onboarding" | "whatsNew" | null
import { invoke } from "@tauri-apps/api/core";
import type { ModInput, PlannedMove } from "../types";
import { formatCount, plural } from "../plural";
import type { ModsRootStatus } from "../api";

/** What Silo's database says about this library (Rust `firstrun::FirstRunState`). */
export interface FirstRunState {
  onboardingDone: boolean;
  whatsNewSeen: boolean;
  hasOrganized: boolean;
  /** Anything else the user made: curation, tags, category overrides, repo or page links, sets. */
  hasCuration: boolean;
}

export type FirstRunFlag = "onboarding_done" | "whats_new_seen";

/** Rejects when the database can't be read (then neither screen shows). */
export function firstRunState(): Promise<FirstRunState> {
  return invoke<FirstRunState>("first_run_state");
}

export function setFirstRunFlag(name: FirstRunFlag, on = true): Promise<void> {
  return invoke<void>("set_first_run_flag", { name, on });
}

/**
 * Which first-run screen to show, if any.
 * - The setup flow: never finished or skipped, and the library has nothing Silo made (no
 *   organized mods, nothing the user made: curation, tags, sets…). An upgrading user never sees it.
 * - The "What's new" card: an upgrading user (organized mods or curation rows) who hasn't
 *   dismissed it.
 */
export function firstRunScreen(s: FirstRunState): "onboarding" | "whatsNew" | null {
  const existing = s.hasOrganized || s.hasCuration;
  if (!s.onboardingDone && !existing) return "onboarding";
  if (!s.whatsNewSeen && existing) return "whatsNew";
  return null;
}

/** The planned moves grouped by category: largest first, each group's files by name. */
export function groupPlan(plan: PlannedMove[]): { category: string; moves: PlannedMove[] }[] {
  const m = new Map<string, PlannedMove[]>();
  for (const p of plan) {
    const arr = m.get(p.category) ?? [];
    arr.push(p);
    m.set(p.category, arr);
  }
  return [...m.entries()]
    .map(([category, moves]) => ({
      category,
      moves: [...moves].sort((a, b) => a.fileName.localeCompare(b.fileName)),
    }))
    .sort((a, b) => b.moves.length - a.moves.length || a.category.localeCompare(b.category));
}

/** One mod that couldn't be filed. */
export interface FilingFailure {
  techName: string;
  fileName: string;
  message: string;
}

/**
 * Match organize report lines (`"<techName>: <why>"`) to the planned files. A line that names no
 * planned mod is kept with an empty techName so nothing is dropped.
 */
export function filingFailures(errors: string[], plan: PlannedMove[]): FilingFailure[] {
  const byTech = new Map(plan.map((p) => [p.techName, p]));
  return errors.map((line) => {
    const i = line.indexOf(": ");
    const tech = i > 0 ? line.slice(0, i) : "";
    const mv = byTech.get(tech);
    if (mv) return { techName: tech, fileName: mv.fileName, message: line.slice(i + 2) };
    return { techName: "", fileName: "", message: line };
  });
}

/**
 * What to file for `plan`, after a scan since the preview (#427). A planned mod still loose is
 * filed as it is now. One the scan no longer finds loose, whose file isn't in the library either,
 * is passed on as previewed, so the backend checks the disk and says whether it's gone or why it
 * can't tell: a scan that couldn't read a folder leaves mods out that aren't gone. One whose file
 * is in the library now is left out (filed since).
 * @param loose fileable now, by tech name; @param planned the preview's inputs, by tech name
 * @param filedFiles file names in the library now
 */
export function filingInputs(
  plan: PlannedMove[],
  loose: Map<string, ModInput>,
  planned: Map<string, ModInput>,
  filedFiles: Set<string>,
): ModInput[] {
  const out: ModInput[] = [];
  for (const p of plan) {
    const now = loose.get(p.techName);
    if (now) out.push(now);
    else if (!filedFiles.has(p.fileName))
      out.push(
        planned.get(p.techName) ?? {
          techName: p.techName,
          fileName: p.fileName,
          kind: "zip",
          category: p.category,
          subcategory: null,
        },
      );
  }
  return out;
}

/**
 * After a run that stopped partway (a later batch threw): each submitted input not already
 * reported and whose file isn't in the library now, as a failure with `why`. Judged by what was
 * submitted, not by what the scan shows loose, so a file the scan missed is still accounted for.
 * @param filedNow file names in the library after the run's rescan
 */
export function unfinishedFailures(
  submitted: ModInput[],
  reported: FilingFailure[],
  filedNow: Set<string>,
  why: string,
): FilingFailure[] {
  const listed = new Set(reported.map((f) => f.techName));
  return submitted
    .filter((i) => !listed.has(i.techName) && !filedNow.has(i.fileName))
    .map((i) => ({ techName: i.techName, fileName: i.fileName, message: why }));
}

/** How filing went: refused (another change is running; the app said why) or done. */
/** `message`: why nothing was filed, when it was tried and stopped (the game is running…). */
export type OrganizeOutcome =
  | { refused: true; message?: string }
  | { refused?: false; failures: FilingFailure[]; filed: number; relinkFailed?: boolean };
/** What choosing another archive folder did: cancelled, a problem to show, or the new path. */
export type ArchiveChoice = null | { problem: string } | { path: string };
/** How the setup flow ended: Skip setup, Not now, or after organizing. */
export type FinishHow = "skip" | "notNow" | "done";

/** The text "Copy details" copies for a partial failure. */
export function failureDetails(failures: FilingFailure[], filed: number, total: number): string {
  const lines = [`Silo filed ${filed} of ${nMods(total)}. These couldn't be moved:`];
  for (const f of failures) lines.push(`- ${f.fileName || f.techName || "?"}: ${f.message}`);
  return lines.join("\n");
}

/** What filing in runs did: problem lines, a thrown error (it stopped), and counts. */
export interface FilingRun {
  errors: string[];
  thrown: unknown;
  /** Mods moved into the library (the reports' `changed`). */
  changed: number;
  /** Mods handed to apply_organize before it finished or threw. */
  done: number;
  /** Linking the filed mods back into the mods folder threw (see `relinkFiled`). */
  relinkError?: unknown;
}

/**
 * After filing, link the filed mods back into the mods folder so they stay active. A throw (the
 * archive drive went away, Silo is updating itself) is kept on `run` instead of escaping, so the
 * Organize dialog or the setup flow that filed them can finish and say so.
 */
export async function relinkFiled(run: FilingRun, relink: () => Promise<unknown>): Promise<void> {
  try {
    await relink();
  } catch (e) {
    run.relinkError = e;
  }
}

/**
 * File `inputs` in runs of `size` with `apply` (apply_organize), reporting progress after each
 * run. Stops at the first throw (the game started, the library got busy); what was filed before
 * stays filed.
 */
export async function fileInRuns<T>(
  inputs: T[],
  apply: (run: T[]) => Promise<{ changed: number; errors: string[] }>,
  size: number,
  onProgress?: (done: number, total: number) => void,
): Promise<FilingRun> {
  const out: FilingRun = { errors: [], thrown: null, changed: 0, done: 0 };
  try {
    for (const run of onProgress ? chunks(inputs, size) : [inputs]) {
      const rep = await apply(run);
      out.errors.push(...rep.errors);
      out.changed += rep.changed;
      out.done += run.length;
      onProgress?.(out.done, inputs.length);
    }
  } catch (e) {
    out.thrown = e;
  }
  return out;
}

/**
 * The progress bar while filing: mods done of total, then, once the last run is filed, the steps
 * that still follow (linking the mods back and reading the library again). Those can take a
 * while on a large library, so the bar says so instead of sitting on "Filing 194 of 194…" (#322).
 * `reading`: the library scan's progress, when one is running and has counted its mods.
 * @returns the bar's value (undefined: indeterminate), max and label
 */
export function filingProgress(
  done: number,
  total: number,
  reading?: { done: number; total: number } | null,
): { value: number | undefined; max: number; label: string } {
  if (!total) return { value: undefined, max: 1, label: "Filing…" };
  if (done < total) return { value: done, max: total, label: `Filing ${formatCount(done)} of ${formatCount(total)}…` };
  if (reading?.total)
    return {
      value: reading.done,
      max: reading.total,
      label: `Reading your library: ${formatCount(reading.done)} of ${nMods(reading.total)}…`,
    };
  return { value: undefined, max: 1, label: "Turning your mods back on and reading your library…" };
}

/** Split `items` into runs of `size` (the last one shorter), for filing with progress. */
export function chunks<T>(items: T[], size: number): T[][] {
  const out: T[][] = [];
  for (let i = 0; i < items.length; i += Math.max(1, size)) out.push(items.slice(i, i + size));
  return out;
}

/** "1 mod" / "1,204 mods". */
export function nMods(n: number): string {
  return `${formatCount(n)} ${plural(n, { one: "mod", other: "mods" })}`;
}

/**
 * Which mods-folder situation the setup flow's first step explains (the same conditions, in the
 * same order, as the mods-folder notices in modsStatus.ts).
 * - `unavailable`: a folder the user chose can't be opened (drive unplugged).
 * - `layout`: Silo won't change files there (#220); the reason sentence explains.
 * - `missing`: FS25 hasn't created its mods folder yet.
 * - `mismatch`: FS25 reads a different folder than the one Silo uses.
 * - `override`: FS25 is set to read a custom folder, and Silo follows it.
 * - `ok`: the game's own folder.
 * - `unknown`: the status couldn't be read yet.
 */
export type FolderVariant =
  | "unavailable"
  | "layout"
  | "missing"
  | "mismatch"
  | "override"
  | "ok"
  | "unknown";

export function folderVariant(st: ModsRootStatus | null): FolderVariant {
  if (!st || !st.path) return "unknown";
  if (st.source === "user" && !st.available) return "unavailable";
  if (st.layoutProblem) return "layout";
  if (st.modsFolderMissing) return "missing";
  if (st.mismatch) return "mismatch";
  if (st.source === "gameOverride") return "override";
  return "ok";
}

/** Whether the first step can move on: there is a folder Silo can read and change. */
export function folderUsable(v: FolderVariant): boolean {
  return v === "ok" || v === "override" || v === "mismatch";
}

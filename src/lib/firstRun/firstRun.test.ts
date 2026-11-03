// First run (#250): who sees the setup flow / What's new, the plan grouping, and failures.
import { describe, expect, it, vi } from "vitest";
import {
  chunks,
  failureDetails,
  fileInRuns,
  filingFailures,
  vanishedFromPlan,
  filingProgress,
  firstRunScreen,
  folderUsable,
  folderVariant,
  groupPlan,
  nMods,
  relinkFiled,
  type FirstRunState,
} from "./firstRun";
import type { ModsRootStatus } from "../api";
import type { PlannedMove } from "../types";

const fresh: FirstRunState = {
  onboardingDone: false,
  whatsNewSeen: false,
  hasOrganized: false,
  hasCuration: false,
};

describe("firstRunScreen", () => {
  it("shows the setup flow for a new library with no flag", () => {
    expect(firstRunScreen(fresh)).toBe("onboarding");
  });

  it("never shows the setup flow again once it was finished or skipped", () => {
    expect(firstRunScreen({ ...fresh, onboardingDone: true })).toBeNull();
    expect(firstRunScreen({ ...fresh, onboardingDone: true, whatsNewSeen: true })).toBeNull();
  });

  it("an upgrading user with organized mods gets What's new instead", () => {
    expect(firstRunScreen({ ...fresh, hasOrganized: true })).toBe("whatsNew");
  });

  it("curation rows also mean an existing library", () => {
    expect(firstRunScreen({ ...fresh, hasCuration: true })).toBe("whatsNew");
  });

  it("What's new shows once", () => {
    expect(firstRunScreen({ ...fresh, hasOrganized: true, whatsNewSeen: true })).toBeNull();
  });

  it("a library organized after the setup flow sees neither", () => {
    expect(
      firstRunScreen({ onboardingDone: true, whatsNewSeen: true, hasOrganized: true, hasCuration: true }),
    ).toBeNull();
  });
});

const mv = (techName: string, category: string, fileName = `${techName}.zip`): PlannedMove => ({
  techName,
  fileName,
  category,
  relFrom: fileName,
  relTo: `archive/${category}/${fileName}`,
});

describe("groupPlan", () => {
  it("groups by category, biggest first, files by name", () => {
    const g = groupPlan([mv("b", "Maps"), mv("z", "Tractors"), mv("a", "Tractors"), mv("c", "Animals")]);
    expect(g.map((x) => [x.category, x.moves.map((m) => m.techName)])).toEqual([
      ["Tractors", ["a", "z"]],
      ["Animals", ["c"]],
      ["Maps", ["b"]],
    ]);
  });
});

describe("vanishedFromPlan (#427)", () => {
  const plan = [mv("FS25_A", "Tractors"), mv("FS25_B", "Maps")];
  const gone = (t: string) => ({
    techName: t,
    fileName: `${t}.zip`,
    message: "is no longer in your mods folder, so Silo didn't file it.",
  });

  it("names a planned mod that is neither loose nor filed now", () => {
    expect(vanishedFromPlan(plan, new Set(["FS25_A"]), new Set())).toEqual([gone("FS25_B")]);
  });

  it("names every planned mod when none is left", () => {
    expect(vanishedFromPlan(plan, new Set(), new Set())).toEqual([gone("FS25_A"), gone("FS25_B")]);
  });

  it("leaves out a mod already in the library and one still loose", () => {
    expect(vanishedFromPlan(plan, new Set(["FS25_A"]), new Set(["FS25_B"]))).toEqual([]);
  });
});

describe("filingFailures", () => {
  const plan = [mv("FS25_A", "Tractors"), mv("FS25_B", "Maps")];

  it("matches report lines to the planned files", () => {
    expect(filingFailures(["FS25_B: move: Access is denied. (os error 5)"], plan)).toEqual([
      { techName: "FS25_B", fileName: "FS25_B.zip", message: "move: Access is denied. (os error 5)" },
    ]);
  });

  it("keeps a line that names no planned mod", () => {
    expect(filingFailures(["something odd"], plan)).toEqual([
      { techName: "", fileName: "", message: "something odd" },
    ]);
  });

  it("copies a readable summary", () => {
    const f = filingFailures(["FS25_B: move: denied"], plan);
    expect(failureDetails(f, 1, 2)).toBe(
      "Silo filed 1 of 2 mods. These couldn't be moved:\n- FS25_B.zip: move: denied",
    );
  });
});

describe("folderVariant", () => {
  const st = (over: Partial<ModsRootStatus>): ModsRootStatus => ({
    path: "C:\\mods",
    source: "default",
    gameReads: null,
    mismatch: false,
    available: true,
    modsFolderMissing: false,
    known: [],
    canWriteGameSetting: true,
    gameSettingProblem: null,
    unresolvedOverride: null,
    layoutProblem: null,
    layoutProblemCause: null,
    layoutConflict: null,
    gameProfile: null,
    ...over,
  });

  it("follows the mods-folder notices' order", () => {
    expect(folderVariant(null)).toBe("unknown");
    expect(folderVariant(st({ path: null }))).toBe("unknown");
    expect(folderVariant(st({ source: "user", available: false, layoutProblem: "x" }))).toBe("unavailable");
    expect(folderVariant(st({ layoutProblem: "Shares files", modsFolderMissing: true }))).toBe("layout");
    expect(folderVariant(st({ modsFolderMissing: true, mismatch: true }))).toBe("missing");
    expect(folderVariant(st({ source: "user", mismatch: true }))).toBe("mismatch");
    expect(folderVariant(st({ source: "gameOverride" }))).toBe("override");
    expect(folderVariant(st({}))).toBe("ok");
  });

  it("only moves on from a folder Silo can use", () => {
    expect(["ok", "override", "mismatch"].every((v) => folderUsable(v as never))).toBe(true);
    expect(["unknown", "unavailable", "layout", "missing"].some((v) => folderUsable(v as never))).toBe(false);
  });
});

describe("fileInRuns", () => {
  const apply = (fail: Set<string> = new Set(), throwOn = -1) => {
    let call = 0;
    return vi.fn(async (run: string[]) => {
      if (call++ === throwOn) throw new Error("GAME_RUNNING");
      const errors = run.filter((t) => fail.has(t)).map((t) => `${t}: move: in use`);
      return { changed: run.length - errors.length, errors };
    });
  };

  it("files in runs with progress after each", async () => {
    const a = apply(new Set(["c"]));
    const seen: number[] = [];
    const out = await fileInRuns(["a", "b", "c", "d", "e"], a, 2, (d) => seen.push(d));
    expect(a.mock.calls.map((c) => c[0])).toEqual([["a", "b"], ["c", "d"], ["e"]]);
    expect(seen).toEqual([2, 4, 5]);
    expect(out).toEqual({ errors: ["c: move: in use"], thrown: null, changed: 4, done: 5 });
  });

  it("without progress, files in one call", async () => {
    const a = apply();
    await fileInRuns(["a", "b", "c"], a, 2);
    expect(a).toHaveBeenCalledOnce();
  });

  it("stops at a throw and keeps what was filed", async () => {
    const out = await fileInRuns(["a", "b", "c", "d"], apply(new Set(), 1), 2, () => {});
    expect(out.changed).toBe(2);
    expect(out.done).toBe(2);
    expect(String(out.thrown)).toContain("GAME_RUNNING");
  });

  it("a throw on the first run files nothing", async () => {
    const out = await fileInRuns(["a", "b"], apply(new Set(), 0), 2, () => {});
    expect(out.changed).toBe(0);
    expect(out.thrown).not.toBeNull();
  });
});

describe("relinkFiled", () => {
  it("keeps a throw on the run instead of rejecting, so the dialog can finish", async () => {
    const run = { errors: [], thrown: null, changed: 3, done: 3 };
    await expect(
      relinkFiled(run, async () => {
        throw new Error("The library archive drive isn't connected.");
      }),
    ).resolves.toBeUndefined();
    expect(String((run as { relinkError?: unknown }).relinkError)).toContain("archive drive");
  });

  it("leaves a clean run as it was", async () => {
    const run = { errors: [], thrown: null, changed: 1, done: 1 };
    await relinkFiled(run, async () => ({ errors: [] }));
    expect(run).toEqual({ errors: [], thrown: null, changed: 1, done: 1 });
  });
});

describe("helpers", () => {
  it("chunks", () => {
    expect(chunks([1, 2, 3, 4, 5], 2)).toEqual([[1, 2], [3, 4], [5]]);
    expect(chunks([], 25)).toEqual([]);
  });

  it("nMods", () => {
    expect(nMods(1)).toBe("1 mod");
    expect(nMods(1204)).toBe("1,204 mods");
  });
});

describe("filingProgress (#322)", () => {
  it("counts mods while filing", () => {
    expect(filingProgress(25, 50)).toEqual({ value: 25, max: 50, label: "Filing 25 of 50…" });
  });

  it("names the steps after the last run instead of sitting on N of N", () => {
    expect(filingProgress(194, 194)).toEqual({
      value: undefined,
      max: 1,
      label: "Turning your mods back on and reading your library…",
    });
  });

  it("shows the library read's own progress once it has counted the mods", () => {
    expect(filingProgress(194, 194, { done: 40, total: 194 })).toEqual({
      value: 40,
      max: 194,
      label: "Reading your library: 40 of 194 mods…",
    });
    // Not counted yet: the generic step.
    expect(filingProgress(194, 194, { done: 0, total: 0 }).value).toBeUndefined();
    // A read still running mid-filing doesn't take over the filing count.
    expect(filingProgress(10, 194, { done: 40, total: 194 }).label).toBe("Filing 10 of 194…");
  });

  it("has no count before filing knows its total", () => {
    expect(filingProgress(0, 0)).toEqual({ value: undefined, max: 1, label: "Filing…" });
  });
});

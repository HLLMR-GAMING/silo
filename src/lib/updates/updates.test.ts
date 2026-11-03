// @vitest-environment jsdom
// The Updates view's logic: section grouping, the rail badge, ModHub dropped builds, the check,
// Update all (refused / partial / stopped / success) and its summary, and the opt-in auto-check
// (offered once, off by default, persisted).
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ForeignFile } from "../api";
import type { CheckYourselfRow } from "../sources";
import type { UpdateOutcome, UpdateRow } from "../types";
import {
  availableRows,
  droppedBuildFor,
  groupUpdates,
  lastCheckedText,
  newDroppedBuilds,
  droppedKey,
  isModHubPage,
  isNewerVersion,
  normVersion,
  sourceLabel,
  updatesBadge,
  versionChange,
} from "./sections";
import {
  checkBlockedReason,
  installBlockedReason,
  fileStamps,
  heldRefresh,
  holdRefresh,
  linkRefresh,
  mergeRows,
  recheckRow,
  refreshTargets,
  releaseInstalled,
  sameStamps,
  runUpdateCheck,
  updateAll,
  updateAllSummary,
  type WriteToken,
} from "./run";
import {
  answerAutoCheckOffer,
  AUTO_CHECK_KEY,
  AUTO_CHECK_OFFERED_KEY,
  autoCheck,
  autoCheckEnabled,
  autoCheckOffer,
  reloadAutoCheck,
  setAutoCheck,
  takeAutoCheckOffer,
} from "./autoCheck.svelte";

const mod = (techName: string, version = "1.0.0.0", title = techName) => ({
  techName,
  title,
  version,
  path: `C:/mods/${techName}.zip`,
});

const row = (techName: string, extra: Partial<UpdateRow> = {}): UpdateRow => ({
  techName,
  title: techName,
  path: `C:/mods/${techName}.zip`,
  current: "1.0.0.0",
  latest: "1.1.0.0",
  ...extra,
});

describe("groupUpdates", () => {
  const mods = [
    mod("A_Ready"),
    mod("B_Hub"),
    mod("C_Current"),
    mod("D_Failed"),
    mod("E_Page"),
    mod("F_Unknown", "1.0.0.0", "Zed"),
    mod("G_Unknown", "1.0.0.0", "alpha"),
    mod("H_Done", "1.1.0.0"),
  ];
  const rows = [
    row("A_Ready", { hasUpdate: true, assetUrl: "https://x/a.zip", source: "github" }),
    row("B_Hub", { hasUpdate: true, assetUrl: null, pageUrl: "https://www.farming-simulator.com/mod.php?mod_id=1" }),
    row("C_Current", { hasUpdate: false, latest: "1.0.0.0" }),
    row("D_Failed", { error: "GitHub is limiting requests for a while." }),
    // Updated since the check: the library now has the offered version.
    row("H_Done", { hasUpdate: true, assetUrl: "https://x/h.zip" }),
    // A row for a mod no longer in the library is dropped.
    row("Z_Gone", { hasUpdate: true, assetUrl: "https://x/z.zip" }),
  ];
  const yourself: CheckYourselfRow[] = [
    { techName: "E_Page", title: "E_Page", url: "https://example.com/e", host: "example.com", lastChecked: null },
  ];

  it("sorts rows into their sections", () => {
    const s = groupUpdates({ mods, rows, checkYourself: yourself, catalogReached: true });
    expect(s.ready.map((r) => r.techName)).toEqual(["A_Ready"]);
    expect(s.modhub.map((r) => r.techName)).toEqual(["B_Hub"]);
    expect(s.checkYourself.map((r) => r.techName)).toEqual(["E_Page"]);
    expect(s.failed.map((r) => r.techName)).toEqual(["D_Failed"]);
    expect(s.upToDate.map((r) => r.techName)).toEqual(["C_Current", "H_Done"]);
    expect(s.upToDate[1]).toMatchObject({ hasUpdate: false, current: "1.1.0.0" });
    // Not tracked: neither the catalog, a repo nor a saved page answered; by title.
    expect(s.notTracked).toEqual([
      { techName: "G_Unknown", title: "alpha" },
      { techName: "F_Unknown", title: "Zed" },
    ]);
  });

  it("lists mods with update checks off only under Not checked (#333)", () => {
    const skipped = new Set(["A_Ready", "E_Page", "F_Unknown"]);
    const s = groupUpdates({ mods, rows, checkYourself: yourself, catalogReached: true, skipped });
    // A row from an earlier check doesn't put a skipped mod in Ready, and it's never "not tracked".
    expect(s.ready).toEqual([]);
    expect(s.checkYourself).toEqual([]);
    expect(s.notTracked.map((m) => m.techName)).toEqual(["G_Unknown"]);
    expect(s.skipped).toEqual([
      { techName: "A_Ready", title: "A_Ready" },
      { techName: "E_Page", title: "E_Page" },
      { techName: "F_Unknown", title: "Zed" },
    ]);
    // The badge counts only what Silo would update: the ModHub row.
    expect(updatesBadge(s, true)).toBe(1);
  });

  it("calls nothing 'not tracked' when the catalog didn't answer", () => {
    const s = groupUpdates({ mods, rows, checkYourself: yourself, catalogReached: false });
    expect(s.notTracked).toEqual([]);
  });

  it("the badge counts Ready + ModHub, and only after a check", () => {
    const s = groupUpdates({ mods, rows, checkYourself: yourself, catalogReached: true });
    expect(availableRows(s).map((r) => r.techName)).toEqual(["A_Ready", "B_Hub"]);
    expect(updatesBadge(s, true)).toBe(2);
    expect(updatesBadge(s, false)).toBeUndefined();
    const none = groupUpdates({ mods, rows: [], checkYourself: [], catalogReached: true });
    expect(updatesBadge(none, true)).toBeUndefined();
  });

  it("the badge drops when a ModHub build was used (the library now has that version)", () => {
    const hubRow = rows[1];
    const before = groupUpdates({ mods, rows: [hubRow], checkYourself: [], catalogReached: true });
    expect(updatesBadge(before, true)).toBe(1);
    const adopted = mods.map((m) => (m.techName === "B_Hub" ? { ...m, version: "1.1.0.0" } : m));
    const after = groupUpdates({ mods: adopted, rows: [hubRow], checkYourself: [], catalogReached: true });
    expect(updatesBadge(after, true)).toBeUndefined();
    expect(after.upToDate.map((r) => r.techName)).toEqual(["B_Hub"]);
  });

  it("installs from where the file is now (Organize may have moved it since the check)", () => {
    const s = groupUpdates({
      mods: [{ ...mod("X"), path: "C:/mods/archive/Tractors/X.zip" }],
      rows: [row("X", { hasUpdate: true, assetUrl: "https://x/x.zip" })],
      checkYourself: [],
      catalogReached: true,
    });
    expect(s.ready[0].path).toBe("C:/mods/archive/Tractors/X.zip");
  });

  it("treats 'v1.1.0.0' as the same version as '1.1.0.0'", () => {
    const s = groupUpdates({
      mods: [mod("X", "v1.1.0.0")],
      rows: [row("X", { hasUpdate: true, assetUrl: "https://x/x.zip" })],
      checkYourself: [],
      catalogReached: true,
    });
    expect(s.ready).toEqual([]);
    expect(s.upToDate).toHaveLength(1);
  });

  it("stops offering a version the library already has in other words, or a newer one", () => {
    const offered = (version: string, latest: string) =>
      groupUpdates({
        mods: [mod("X", version)],
        rows: [row("X", { hasUpdate: true, latest, assetUrl: "https://x/x.zip" })],
        checkYourself: [],
        catalogReached: true,
      }).ready.length;
    // A tag "v1.2.0" is the 1.2.0.0 the modDesc says.
    expect(offered("1.2.0.0", "v1.2.0")).toBe(0);
    // A newer build was installed since the check: Update all must not put 1.2 back over it.
    expect(offered("1.3.0.0", "1.2.0.0")).toBe(0);
    // Still older: still offered. Versions that aren't plain numbers only match exactly.
    expect(offered("1.1.0.0", "1.2.0.0")).toBe(1);
    expect(offered("1.2-beta", "1.2")).toBe(1);
  });

  it("doesn't call a mod added since the check 'not tracked'", () => {
    const s = groupUpdates({
      mods: [mod("A"), mod("NEW")],
      rows: [row("A", { hasUpdate: false })],
      checkYourself: [],
      catalogReached: true,
      checkedTechs: new Set(["A", "OLD"]),
    });
    expect(s.notTracked).toEqual([]);
    const asked = groupUpdates({
      mods: [mod("A"), mod("NEW")],
      rows: [row("A", { hasUpdate: false })],
      checkYourself: [],
      catalogReached: true,
      checkedTechs: new Set(["A", "NEW"]),
    });
    expect(asked.notTracked.map((m) => m.techName)).toEqual(["NEW"]);
  });
});

describe("row words", () => {
  it("shows versions without a doubled v, and names the source", () => {
    expect(versionChange({ current: "v8.0.0.4", latest: "8.0.1.0" })).toBe("8.0.0.4 → 8.0.1.0");
    expect(versionChange({ latest: "2.0" })).toBe("2.0");
    // Catalog versions as authors write them (coordinator smoke C2: "1.5.0.0 → V.1.6.4.0").
    expect(versionChange({ current: "1.5.0.0", latest: "V.1.6.4.0" })).toBe("1.5.0.0 → 1.6.4.0");
    expect(normVersion("Ver 2.1")).toBe("2.1");
    expect(normVersion("version.3")).toBe("3");
    expect(normVersion("vast")).toBe("vast");
    expect(isNewerVersion("V.1.6.4.0", "1.5.0.0")).toBe(true);
    expect(sourceLabel({ source: "github" })).toBe("GitHub");
    expect(sourceLabel({ pageUrl: "https://www.farming-simulator.com/mod.php?mod_id=9" })).toBe("ModHub");
    expect(sourceLabel({})).toBeNull();
    expect(sourceLabel({ pageUrl: "https://example.com/?ref=farming-simulator.com" })).toBeNull();
  });

  it("calls a page ModHub only when its host is farming-simulator.com (#358)", () => {
    expect(isModHubPage("https://www.farming-simulator.com/mod.php?mod_id=9")).toBe(true);
    expect(isModHubPage("https://farming-simulator.com/mods")).toBe(true);
    expect(isModHubPage("HTTPS://WWW.Farming-Simulator.COM/mod.php")).toBe(true);
    expect(isModHubPage("https://farming-simulator.com.example.net/mod.php")).toBe(false);
    expect(isModHubPage("https://evil-farming-simulator.com/mod.php")).toBe(false);
    expect(isModHubPage("https://example.com/farming-simulator.com/mod.php")).toBe(false);
    expect(isModHubPage("https://example.com/?u=https://www.farming-simulator.com")).toBe(false);
    expect(isModHubPage("https://farming-simulator.com@example.com/")).toBe(false);
    expect(isModHubPage("http://www.farming-simulator.com/mod.php")).toBe(false);
    expect(isModHubPage("farming-simulator.com")).toBe(false);
    expect(isModHubPage(null)).toBe(false);
    expect(isModHubPage("")).toBe(false);
  });

  it("says when the last check ran", () => {
    const now = Date.parse("2026-10-18T12:00:00Z");
    expect(lastCheckedText(now - 10_000, now)).toBe("just now");
    expect(lastCheckedText(now - 2 * 60_000, now)).toBe("2 minutes ago");
    expect(lastCheckedText(now - 3 * 3_600_000, now)).toBe("3 hours ago");
  });
});

describe("ModHub dropped builds", () => {
  const hub = row("FS25_Lizard", { hasUpdate: true, assetUrl: null, title: "Lizard R-Series" });
  const file = (flat: string | null, managed: string | null = "1.0.0.0"): ForeignFile => ({
    techName: "FS25_Lizard",
    fileName: "FS25_Lizard.zip",
    kind: "zip",
    flatVersion: flat,
    managedVersion: managed,
    archiveChanged: false,
    newlyNoted: false,
  });

  it("finds the zip the user saved, once per build", () => {
    expect(droppedBuildFor(hub, [file("1.0.2.0")])?.flatVersion).toBe("1.0.2.0");
    const seen = new Set<string>();
    const first = newDroppedBuilds([hub], [file("1.0.2.0")], seen);
    expect(first).toHaveLength(1);
    seen.add(droppedKey(first[0].file));
    expect(newDroppedBuilds([hub], [file("1.0.2.0")], seen)).toEqual([]);
    // A later build is new again.
    expect(newDroppedBuilds([hub], [file("1.0.3.0")], seen)).toHaveLength(1);
  });

  it("ignores the same build as Silo's copy, an older one, and other mods", () => {
    expect(droppedBuildFor(hub, [file("1.0.0.0")])).toBeNull();
    // A rollback or a multiplayer server's older copy is never offered as "newer".
    expect(droppedBuildFor(hub, [file("0.9.0.0")])).toBeNull();
    expect(droppedBuildFor(hub, [file("1.0.0.0", "1.0.2.0")])).toBeNull();
    expect(droppedBuildFor(hub, [file(null)])).toBeNull();
    expect(droppedBuildFor(hub, [{ ...file("2.0"), techName: "Other" }])).toBeNull();
  });

  it("isn't a drop when the library's copy changed (#431)", () => {
    // The mods folder still has the build Silo put there; the library has a different one now.
    expect(droppedBuildFor(hub, [{ ...file("1.0.2.0"), archiveChanged: true }])).toBeNull();
  });
});

describe("isNewerVersion", () => {
  it("compares dotted numbers, ignoring a leading v", () => {
    expect(isNewerVersion("1.0.10.0", "1.0.9.0")).toBe(true);
    expect(isNewerVersion("v1.2", "1.1.9.9")).toBe(true);
    expect(isNewerVersion("1.0", "1.0.0.0")).toBe(false);
    expect(isNewerVersion("0.9", "1.0")).toBe(false);
    expect(isNewerVersion("1.0-beta", "0.1")).toBe(false);
  });
});

describe("guards", () => {
  it("a check waits while an update installs; installs wait while a check runs", () => {
    expect(checkBlockedReason({ updatingAll: false, rowStates: {} })).toBe("");
    expect(checkBlockedReason({ updatingAll: true, rowStates: {} })).toBe("Wait for the updates to finish.");
    expect(checkBlockedReason({ updatingAll: false, rowStates: { A: { phase: "installing" } } })).not.toBe("");
    expect(checkBlockedReason({ updatingAll: false, rowStates: { A: { phase: "error" } } })).toBe("");
    expect(installBlockedReason(true)).toBe("Wait for the check to finish.");
    expect(installBlockedReason(false)).toBe("");
  });
});

describe("recheckRow", () => {
  const explain = (e: unknown) => ({ message: `why: ${String(e)}`, detail: "d" });
  const failed = row("C", { hasUpdate: undefined, error: "old reason" });

  it("fills the row in when the repo answers", async () => {
    const next = await recheckRow(failed, mod("C"), { owner: "o", repo: "c" }, {
      checkRepo: async () => ({ hasUpdate: true, current: "1.0.0.0", release: { tag: "2.0", assetUrl: "https://gh/c.zip" } as never }),
      explain,
    });
    expect(next).toMatchObject({ hasUpdate: true, latest: "2.0", assetUrl: "https://gh/c.zip", error: undefined });
  });

  it("keeps it in Couldn't check with the new reason when it fails again", async () => {
    const next = await recheckRow(failed, mod("C"), { owner: "o", repo: "c" }, {
      checkRepo: async () => {
        throw "still down";
      },
      explain,
    });
    expect(next).toMatchObject({ error: "why: still down", errorDetail: "d" });
    expect(next.hasUpdate).toBeUndefined();
  });
});

describe("runUpdateCheck", () => {
  const explain = (e: unknown) => ({ message: `why: ${String(e)}`, detail: `detail: ${String(e)}` });

  it("merges the catalog with linked repos (the repo wins) and reports progress", async () => {
    const progress: [number, number][] = [];
    const res = await runUpdateCheck(
      [mod("A"), mod("B"), mod("C")],
      { B: { owner: "o", repo: "b" }, C: { owner: "o", repo: "c" }, Gone: { owner: "o", repo: "g" } },
      {
        catalog: async () => [
          { techName: "A", latest: "2.0", hasUpdate: true, downloadUrl: null, source: null, pageUrl: "https://hub/a" },
          { techName: "B", latest: "1.5", hasUpdate: true, downloadUrl: null, source: null },
        ],
        checkRepo: async (_o, repo) => {
          if (repo === "c") throw "rate limited";
          return {
            hasUpdate: true,
            current: "1.0.0.0",
            release: { tag: "1.6", assetUrl: "https://gh/b.zip" } as never,
          };
        },
        explain,
        onProgress: (d, t) => progress.push([d, t]),
      },
    );
    expect(res.catalogError).toBeNull();
    const by = Object.fromEntries(res.rows.map((r) => [r.techName, r]));
    expect(by.A).toMatchObject({ hasUpdate: true, pageUrl: "https://hub/a", assetUrl: null });
    expect(by.B).toMatchObject({ latest: "1.6", assetUrl: "https://gh/b.zip", source: "github" });
    expect(by.C).toMatchObject({ error: "why: rate limited", errorDetail: "detail: rate limited" });
    expect(by.C.hasUpdate).toBeUndefined();
    expect(progress).toEqual([
      [0, 2],
      [1, 2],
      [2, 2],
    ]);
  });

  it("gives the catalog each mod's file, so an install under another name finds its entry (#386)", async () => {
    let asked: unknown[] = [];
    await runUpdateCheck([mod("FS25_FieldToDoList")], {}, {
      catalog: async (mods) => {
        asked = mods;
        return [];
      },
      checkRepo: async () => {
        throw "unused";
      },
      explain,
    });
    expect(asked).toEqual([{ techName: "FS25_FieldToDoList", version: "1.0.0.0", path: mod("FS25_FieldToDoList").path }]);
  });

  it("shows a mod the catalog couldn't check as failed, and still answers the rest (#386)", async () => {
    const res = await runUpdateCheck([mod("A"), mod("B")], {}, {
      catalog: async () => [
        { techName: "A", latest: null, hasUpdate: false, downloadUrl: null, source: null, error: "Silo couldn't check this mod" },
        { techName: "B", latest: "2.0", hasUpdate: true, downloadUrl: "https://gh/b.zip", source: "github" },
      ],
      checkRepo: async () => {
        throw "unused";
      },
      explain,
    });
    const by = Object.fromEntries(res.rows.map((r) => [r.techName, r]));
    expect(by.A).toMatchObject({ error: "Silo couldn't check this mod" });
    expect(by.A.hasUpdate).toBeUndefined();
    expect(by.B).toMatchObject({ hasUpdate: true, assetUrl: "https://gh/b.zip" });
    expect(res.catalogError).toBeNull();
  });

  it("names the mod in the failure it logs and copies (#333)", async () => {
    const ops: string[] = [];
    const res = await runUpdateCheck([{ ...mod("A"), title: "Yield Tracker" }, mod("B")], {
      A: { owner: "o", repo: "a" },
      B: { owner: "o", repo: "b" },
    }, {
      catalog: async () => [],
      checkRepo: async () => {
        throw "gone";
      },
      explain: (e, op) => {
        ops.push(op);
        return explain(e);
      },
    });
    // A title when the mod has one, else its tech name.
    expect(ops).toEqual(["Checking Yield Tracker for an update", "Checking B for an update"]);
    expect(res.rows.map((r) => r.error)).toEqual(["why: gone", "why: gone"]);
  });

  it("keeps going with linked repos when the catalog is unreachable", async () => {
    const res = await runUpdateCheck([mod("A")], { A: { owner: "o", repo: "a" } }, {
      catalog: async () => {
        throw "offline";
      },
      checkRepo: async () => ({ hasUpdate: false, current: "1.0.0.0", release: { tag: "1.0.0.0", assetUrl: null } as never }),
      explain,
    });
    expect(res.catalogError?.message).toBe("why: offline");
    expect(res.rows).toEqual([expect.objectContaining({ techName: "A", hasUpdate: false })]);
  });

  it("a linked release with no .zip points at its release page, not the catalog's ModHub page", async () => {
    const res = await runUpdateCheck([mod("A")], { A: { owner: "o", repo: "a" } }, {
      catalog: async () => [
        { techName: "A", latest: "1.1", hasUpdate: true, downloadUrl: null, source: null, pageUrl: "https://hub/a" },
      ],
      checkRepo: async () => ({
        hasUpdate: true,
        current: "1.0.0.0",
        release: { tag: "v1.1", assetUrl: null, htmlUrl: "https://github.com/o/a/releases/tag/v1.1" } as never,
      }),
      explain,
    });
    expect(res.rows[0]).toMatchObject({
      source: "github",
      assetUrl: null,
      pageUrl: "https://github.com/o/a/releases/tag/v1.1",
    });
  });
});

describe("updateAll", () => {
  const ok: UpdateOutcome = { backup: "C:/mods/backups/x.zip", problems: [] };
  function token() {
    const t = { labels: [] as string[], progress: [] as [number, number][], ended: 0 };
    const tok: WriteToken = {
      setLabel: (l) => t.labels.push(l),
      progress: (d, n) => t.progress.push([d, n]),
      end: () => t.ended++,
    };
    return { t, tok };
  }
  const rows = [row("AutoDrive"), row("Courseplay"), row("Lizard")];
  const why = (e: unknown) => `stopped: ${String(e)}`;

  it("is refused when another library write runs, and installs nothing", async () => {
    const install = vi.fn();
    const res = await updateAll(rows, { begin: () => null, install, stops: () => false });
    expect(res.refused).toBe(true);
    expect(install).not.toHaveBeenCalled();
    expect(updateAllSummary(res, why)).toBeNull();
  });

  it("updates each mod in turn under one write, with progress, then one summary", async () => {
    const { t, tok } = token();
    const begin = vi.fn(() => tok);
    const done: string[] = [];
    const res = await updateAll(rows, {
      begin,
      install: async () => ok,
      stops: () => false,
      onRowDone: (r) => done.push(r.techName),
    });
    expect(begin).toHaveBeenCalledOnce();
    expect(begin).toHaveBeenCalledWith("Updating 3 mods");
    expect(t.labels).toEqual([
      "Updating 1 of 3: AutoDrive",
      "Updating 2 of 3: Courseplay",
      "Updating 3 of 3: Lizard",
    ]);
    expect(t.progress.at(-1)).toEqual([3, 3]);
    expect(t.ended).toBe(1);
    expect(done).toEqual(["AutoDrive", "Courseplay", "Lizard"]);
    expect(updateAllSummary(res, why)).toEqual({
      tone: "success",
      message: "Updated 3 mods. The previous versions are in backups.",
    });
  });

  it("a failed mod doesn't stop the rest: '2 updated, 1 failed'", async () => {
    const { t, tok } = token();
    const failed: string[] = [];
    const res = await updateAll(rows, {
      begin: () => tok,
      install: async (r) => {
        if (r.techName === "Courseplay") throw "download failed";
        return ok;
      },
      stops: () => false,
      onRowFailed: (r) => failed.push(r.techName),
    });
    expect(res.updated.map((u) => u.row.techName)).toEqual(["AutoDrive", "Lizard"]);
    expect(failed).toEqual(["Courseplay"]);
    expect(t.ended).toBe(1);
    expect(updateAllSummary(res, why)).toEqual({
      tone: "warn",
      message: "2 updated, 1 failed.",
      body: "Each failed mod shows why in its row.",
    });
  });

  it("stops at a failure that would fail the rest (the game is running)", async () => {
    const { t, tok } = token();
    const install = vi.fn(async () => {
      throw "GAME_RUNNING";
    });
    const res = await updateAll(rows, { begin: () => tok, install, stops: (e) => e === "GAME_RUNNING" });
    expect(install).toHaveBeenCalledOnce();
    expect(res.notTried.map((r) => r.techName)).toEqual(["Courseplay", "Lizard"]);
    expect(t.ended).toBe(1);
    expect(updateAllSummary(res, why)).toEqual({
      tone: "danger",
      message: "0 updated, 1 failed. 2 not tried.",
      body: "stopped: GAME_RUNNING",
    });
  });

  it("ends the write even when a callback throws", async () => {
    const { t, tok } = token();
    await expect(
      updateAll([row("A")], {
        begin: () => tok,
        install: async () => ok,
        stops: () => false,
        onRowStart: () => {
          throw new Error("boom");
        },
      }),
    ).rejects.toThrow("boom");
    expect(t.ended).toBe(1);
  });

  it("one mod's summary names it and the version", () => {
    const res = { refused: false, updated: [{ row: row("Courseplay", { latest: "8.0.1.0" }), outcome: ok }], failed: [], notTried: [], stoppedBy: null };
    expect(updateAllSummary(res, why)?.message).toBe(
      "Updated Courseplay to 8.0.1.0. The previous version is in backups.",
    );
  });
});

describe("automatic update checks (opt-in)", () => {
  beforeEach(() => {
    localStorage.clear();
    reloadAutoCheck();
  });

  it("is off by default", () => {
    expect(autoCheckEnabled()).toBe(false);
  });

  it("is offered once: the first visit takes the offer, later visits don't", () => {
    expect(takeAutoCheckOffer()).toBe(true);
    expect(localStorage.getItem(AUTO_CHECK_OFFERED_KEY)).toBe("1");
    expect(takeAutoCheckOffer()).toBe(false);
    // Unanswered, it's still off.
    expect(autoCheckEnabled()).toBe(false);
  });

  it("isn't offered when the setting was already chosen in Settings", () => {
    setAutoCheck(false);
    expect(takeAutoCheckOffer()).toBe(false);
  });

  it("answering persists the choice and closes the card", () => {
    autoCheckOffer.open = true;
    answerAutoCheckOffer(true);
    expect(autoCheckOffer.open).toBe(false);
    expect(autoCheck.on).toBe(true);
    expect(localStorage.getItem(AUTO_CHECK_KEY)).toBe("on");
    // A restart reads it back.
    reloadAutoCheck();
    expect(autoCheckEnabled()).toBe(true);
    setAutoCheck(false);
    reloadAutoCheck();
    expect(autoCheckEnabled()).toBe(false);
  });

  it("works without storage (private mode)", () => {
    const get = vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new Error("denied");
    });
    const set = vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("denied");
    });
    try {
      reloadAutoCheck();
      expect(autoCheckEnabled()).toBe(false);
      setAutoCheck(true);
      expect(autoCheckEnabled()).toBe(true);
    } finally {
      get.mockRestore();
      set.mockRestore();
    }
  });
});

describe("checking again when Silo links mods to the catalog (#402)", () => {
  const lib = new Set(["FS25_FieldToDoList", "FS25_Other"]);
  const at = (o: Partial<Parameters<typeof linkRefresh>[0]>) =>
    linkRefresh({
      linked: ["FS25_FieldToDoList"],
      gen: 1,
      running: null,
      lastAsked: lib,
      blocked: false,
      ...o,
    });

  it("refreshes just the linked mods a check of this folder asked about", () => {
    expect(at({})).toEqual({ when: "now", names: ["FS25_FieldToDoList"] });
    expect(
      at({ linked: ["FS25_FieldToDoList", "FS25_New", "FS25_FieldToDoList"] })
        .names,
    ).toEqual(["FS25_FieldToDoList"]);
    // An update installing: it waits, it isn't dropped.
    expect(at({ blocked: true }).when).toBe("after");
    // A check running in this folder: after it, for what it asks about.
    expect(
      at({ running: { gen: 1, asked: lib }, lastAsked: undefined }).when,
    ).toBe("after");
  });

  it("never for mods no allowed check of this folder asked about", () => {
    // No check of this folder yet: the first one sees the links.
    expect(at({ lastAsked: undefined }).when).toBe("no");
    // Mods added since the check, or of another folder.
    expect(at({ linked: ["FS25_New"] }).when).toBe("no");
    // A check of folder 1 still running after the switch to folder 2, though folder 2 has a
    // mod of that name: nothing is sent for folder 2.
    expect(
      at({ gen: 2, running: { gen: 1, asked: lib }, lastAsked: undefined })
        .when,
    ).toBe("no");
    // The running check's own snapshot, not the folder's mods now.
    expect(
      at({ running: { gen: 1, asked: new Set(["FS25_Other"]) } }).when,
    ).toBe("no");
  });

  it("holds refreshes per folder, drops one held for another, runs it when nothing is busy", () => {
    const held = holdRefresh(holdRefresh(null, 1, ["A"]), 1, ["B", "A"]);
    expect(held).toEqual({ gen: 1, names: ["A", "B"] });
    expect(holdRefresh(held, 2, ["C"])).toEqual({ gen: 2, names: ["C"] });
    expect(heldRefresh(null, 1, false)).toEqual({ run: null, held: null });
    expect(heldRefresh(held, 1, true)).toEqual({ run: null, held });
    expect(heldRefresh(held, 1, false)).toEqual({
      run: ["A", "B"],
      held: null,
    });
    expect(heldRefresh(held, 2, false)).toEqual({ run: null, held: null });
  });

  it("asks about, and compares, the same mods: never one left out meanwhile", () => {
    const lib = [
      { techName: "A", path: "A.zip", size: 1, mtimeMs: 1, version: "1" },
      { techName: "B", path: "B.zip", size: 1, mtimeMs: 1, version: "1" },
    ];
    let skipB = false;
    const leftOut = (t: string) => skipB && t === "B";
    // Held for A and B; B is left out before it runs.
    skipB = true;
    const asked = refreshTargets(["A", "B"], lib, leftOut);
    expect(asked.map((m) => m.techName)).toEqual(["A"]);
    // Nothing changed: the answer is put in (one request, no re-asking).
    const want = asked.map((m) => m.techName);
    expect(
      sameStamps(
        fileStamps(asked),
        fileStamps(refreshTargets(want, lib, leftOut)),
      ),
    ).toBe(true);
    // A mod updated since the last scan that worked is left out too.
    const installed = new Set(["A"]);
    expect(refreshTargets(["A", "B"], lib, (t) => installed.has(t))).toEqual([
      lib[1],
    ]);
  });

  it("keeps an update made while a scan ran protected from that scan's old files", () => {
    const installed = new Map<string, number>();
    let seq = 0;
    // A scan starts (nothing installed yet), then A is updated while it runs.
    const scanFrom = seq;
    installed.set("A", ++seq);
    // That scan ends with A's old file: A stays protected.
    releaseInstalled(installed, scanFrom);
    expect(installed.has("A")).toBe(true);
    // A scan started after the update reads the new file: A is released.
    releaseInstalled(installed, seq);
    expect(installed.has("A")).toBe(false);
  });

  it("tells when the library's files changed under a refresh", () => {
    const m = {
      techName: "A",
      path: "A.zip",
      size: 1,
      mtimeMs: 2,
      version: "1.0",
    };
    const before = fileStamps([m]);
    expect(sameStamps(before, fileStamps([{ ...m }]))).toBe(true);
    // An install's scan: a new version of the file.
    expect(
      sameStamps(before, fileStamps([{ ...m, version: "1.1", size: 3 }])),
    ).toBe(false);
    expect(sameStamps(before, fileStamps([{ ...m, mtimeMs: 9 }]))).toBe(false);
    // Gone, or another mod there.
    expect(sameStamps(before, fileStamps([]))).toBe(false);
    expect(sameStamps(before, fileStamps([{ ...m, techName: "B" }]))).toBe(
      false,
    );
  });

  it("puts a refresh's rows in place of those mods' rows only", () => {
    const row = (techName: string, latest: string) => ({
      techName,
      title: techName,
      path: `${techName}.zip`,
      latest,
    });
    const rows = [row("A", "1"), row("B", "1")];
    expect(mergeRows(rows, [row("B", "2")], new Set(["B"]))).toEqual([
      row("A", "1"),
      row("B", "2"),
    ]);
    // A refreshed mod the catalog no longer answers for loses its old row.
    expect(mergeRows(rows, [], new Set(["B"]))).toEqual([row("A", "1")]);
  });

  it("tells the catalog the size and time each mod's version was read at", async () => {
    let asked: unknown[] = [];
    const m = { ...mod("FS25_FieldToDoList"), size: 9, mtimeMs: 77 };
    await runUpdateCheck(
      [m],
      {},
      {
        catalog: async (mods) => {
          asked = mods;
          return [];
        },
        checkRepo: async () => {
          throw "unused";
        },
        explain: (e: unknown) => ({ message: String(e), detail: String(e) }),
      },
    );
    expect(asked).toEqual([
      {
        techName: "FS25_FieldToDoList",
        version: "1.0.0.0",
        path: m.path,
        size: 9,
        mtimeMs: 77,
      },
    ]);
  });
});

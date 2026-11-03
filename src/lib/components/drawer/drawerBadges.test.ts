// The mod drawer's header verdict badges and its ⋯ menu (pure). #245
import { describe, expect, it } from "vitest";
import { conflictPartners, drawerBadges, isProblem, modifiedFileCount } from "./drawerBadges";
import { drawerMenuItems } from "./drawerMenu";
import { conflictKindLabel, versionNewer } from "./drawerText";
import type { Conflict, LogReport, VerifyResult } from "../../types";

const mod = { error: null, ignoredDigitPrefix: false, kind: "zip" as const, isMap: true, scriptCount: 3 };
const named = { techName: "FS25_Thing", title: "Thing" };

function verify(status: VerifyResult["status"], files: Partial<Pick<VerifyResult, "changed" | "added" | "removed">> = {}): VerifyResult {
  return { status, how: null, changed: [], added: [], removed: [], matchedVersion: null, note: null, ...files };
}
function conflict(severity: Conflict["severity"], mods: string[]): Conflict {
  return { id: `uniqueType:slot:${mods.join(",")}`, severity, kind: "uniqueType", name: "slot", explanation: "", mods, techNames: mods };
}
function report(crashed: boolean, errors: number): LogReport {
  return {
    engineVersion: null,
    modCount: 1,
    cleanExit: !crashed,
    crashed,
    mods: [{ modName: "FS25_Thing", errors, warnings: 0, benign: 0, sample: "", sampleLine: 0 }],
    unattributed: 0,
    totalFindings: errors, groundPileCap: null, groundPileDisabled: [], fillTypeCap: null, droppedFillTypes: [], truncated: false, siloFolders: [],
  };
}

describe("drawerBadges", () => {
  it("shows nothing for an unchecked mod with no update or problem (and no info badges)", () => {
    expect(drawerBadges(mod)).toEqual([]);
    expect(drawerBadges(mod, { verify: verify("unverified") })).toEqual([]);
  });

  it("names a verified build", () => {
    const b = drawerBadges(mod, { verify: verify("verified") });
    expect(b.map((x) => [x.label, x.tone, x.icon])).toEqual([["Verified build", "success", "verifiedBuild"]]);
  });

  it("counts the files that differ from the published build", () => {
    const v = verify("modified", { changed: ["a.xml"], added: ["b.lua"] });
    expect(modifiedFileCount(v)).toBe(2);
    expect(drawerBadges(mod, { verify: v })[0].label).toBe("Modified (2 files)");
    expect(drawerBadges(mod, { verify: verify("modified", { removed: ["x"] }) })[0].label).toBe("Modified (1 file)");
  });

  it("orders problems before status, like the Library rows, and never has more than three", () => {
    const b = drawerBadges(mod, { verify: verify("modified", { changed: ["a"] }), hasUpdate: true, problem: true });
    expect(b.map((x) => x.id)).toEqual(["modified", "problem", "update"]);
    expect(b.find((x) => x.id === "update")!.label).toBe("Update available");
    const ok = drawerBadges(mod, { verify: verify("verified"), hasUpdate: true, problem: true });
    expect(ok.map((x) => x.id)).toEqual(["problem", "verified", "update"]);
    expect(ok.find((x) => x.id === "problem")).toMatchObject({ tone: "danger", icon: "problem" });
    expect(ok.length).toBeLessThanOrEqual(3);
  });
});

describe("isProblem", () => {
  it("is a critical conflict that names the mod by tech name or title", () => {
    expect(isProblem(named, [conflict("critical", ["Thing", "Other"])], null)).toBe(true);
    expect(isProblem(named, [conflict("critical", ["FS25_Thing", "Other"])], null)).toBe(true);
    expect(isProblem(named, [conflict("warning", ["FS25_Thing", "Other"])], null)).toBe(false);
    expect(isProblem(named, [conflict("critical", ["A", "B"])], null)).toBe(false);
  });

  it("is errors logged by a last run that crashed", () => {
    expect(isProblem(named, [], report(true, 3))).toBe(true);
    expect(isProblem(named, [], report(false, 3))).toBe(false);
    expect(isProblem(named, [], report(true, 0))).toBe(false);
  });
});

describe("conflictPartners", () => {
  it("lists the other mods once, skipping info conflicts and the mod's own names", () => {
    const cs = [
      conflict("critical", ["FS25_Thing", "A"]),
      conflict("warning", ["Thing", "A", "B"]),
      conflict("info", ["FS25_Thing", "C"]),
      conflict("critical", ["X", "Y"]),
    ];
    expect(conflictPartners(named, cs)).toEqual(["A", "B"]);
  });
});

describe("drawerMenuItems", () => {
  it("has every per-mod action, labelled for the current state", () => {
    const off = drawerMenuItems({ favorite: false, hidden: false, broken: false });
    expect(off.map((i) => i.id)).toEqual(["favorite", "broken", "hidden", "category", "showFile", "browse", "remove"]);
    expect(off.map((i) => i.label)).toEqual([
      "Favorite",
      "Flag",
      "Hide",
      "Change category…",
      "Show file",
      "Find in Browse",
      "Remove from library…",
    ]);
    // #429: a separate, dangerous item at the end.
    expect(off.at(-1)).toMatchObject({ danger: true, separatorBefore: true });
    const on = drawerMenuItems({ favorite: true, hidden: true, broken: true });
    expect(on.slice(0, 3).map((i) => i.label)).toEqual(["Remove favorite", "Remove flag", "Unhide"]);
  });
});

describe("drawerText", () => {
  it("names conflict kinds in plain words", () => {
    expect(conflictKindLabel("uniqueType")).toBe("Exclusive slot");
    expect(conflictKindLabel("fillType")).toBe("Fill type");
    expect(conflictKindLabel("somethingNew")).toBe("somethingNew");
  });
  it("compares versions segment by segment", () => {
    expect(versionNewer("1.10.0", "1.9.9")).toBe(true);
    expect(versionNewer("v1.0", "1.0.0")).toBe(false);
    expect(versionNewer("1.0.0", "1.0.1")).toBe(false);
  });
});

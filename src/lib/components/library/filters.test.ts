// The Library's filters (#242): the Needs attention predicate, each filter's predicate (the old
// toggles' and facets', unchanged), the chips and their removal, and what's remembered.
import { describe, expect, it } from "vitest";
import {
  conflictedTechNames,
  filterChips,
  filterMods,
  needsAttention,
  needsUpdateOutcome,
  NO_FILTERS,
  noFilters,
  popoverCount,
  withoutFilter,
  type FilterContext,
  type FilterMod,
} from "./filters";
import {
  LIBRARY_DENSITY_KEY,
  LIBRARY_STATE_KEY,
  loadLibraryDensity,
  loadLibraryState,
  saveLibraryDensity,
  saveLibraryState,
} from "./viewPref";

function mod(techName: string, over: Partial<FilterMod> = {}): FilterMod {
  return {
    techName,
    title: techName,
    error: null,
    ignoredDigitPrefix: false,
    kind: "zip",
    isMap: false,
    scriptCount: 0,
    uniqueType: null,
    hasSettings: false,
    ...over,
  } as FilterMod;
}

const mods = [
  mod("A_map", { isMap: true }),
  mod("B_script", { scriptCount: 2, hasSettings: true }),
  mod("C_broken", { error: "zip is corrupt" }),
  mod("D_unique", { uniqueType: "x" }),
  mod("E_hidden"),
  mod("F_plain"),
];

function ctx(over: Partial<FilterContext> = {}): FilterContext {
  const curation: Record<string, { favorite: boolean; hidden: boolean; broken: boolean }> = {
    A_map: { favorite: true, hidden: false, broken: false },
    E_hidden: { favorite: false, hidden: true, broken: false },
    F_plain: { favorite: false, hidden: false, broken: true },
  };
  return {
    curation: (t) => curation[t] ?? { favorite: false, hidden: false, broken: false },
    active: (m) => m.techName === "A_map" || m.techName === "B_script",
    conflicted: (m) => m.techName === "D_unique",
    hasUpdate: (t) => t === "B_script",
    tags: (t) => (t === "A_map" ? ["harvest"] : []),
    ...over,
  };
}
const names = (list: FilterMod[]) => list.map((m) => m.techName);

describe("needsAttention", () => {
  it("is true for a problem badge or an update, false otherwise", () => {
    expect(needsAttention(mod("x"), {})).toBe(false);
    expect(needsAttention(mod("x", { error: "bad" }), {})).toBe(true); // Can't load
    expect(needsAttention(mod("x"), { conflicted: true })).toBe(true);
    expect(needsAttention(mod("x"), { verdict: "modified" })).toBe(true);
    expect(needsAttention(mod("x"), { flagged: true })).toBe(true);
    expect(needsAttention(mod("x"), { hasUpdate: true })).toBe(true);
    expect(needsAttention(mod("x", { ignoredDigitPrefix: true }), {})).toBe(true);
    // Status and info badges aren't problems.
    expect(needsAttention(mod("x", { isMap: true, scriptCount: 3, kind: "dir" }), { verdict: "verified" })).toBe(false);
  });
});

describe("filterMods", () => {
  it("leaves hidden mods out unless Hidden is on", () => {
    expect(names(filterMods(mods, NO_FILTERS, ctx()))).not.toContain("E_hidden");
    expect(names(filterMods(mods, { ...NO_FILTERS, hidden: true }, ctx()))).toContain("E_hidden");
  });

  it("splits All / Active / Parked by the active set", () => {
    expect(names(filterMods(mods, { ...NO_FILTERS, status: "active" }, ctx()))).toEqual(["A_map", "B_script"]);
    expect(names(filterMods(mods, { ...NO_FILTERS, status: "parked" }, ctx()))).toEqual([
      "C_broken",
      "D_unique",
      "F_plain",
    ]);
  });

  it("applies each popover filter with the old predicate", () => {
    const run = (f: Partial<typeof NO_FILTERS>) => names(filterMods(mods, { ...NO_FILTERS, ...f }, ctx()));
    expect(run({ favorites: true })).toEqual(["A_map"]);
    expect(run({ flagged: true })).toEqual(["F_plain"]);
    expect(run({ conflicted: true })).toEqual(["D_unique"]);
    expect(run({ hasSettings: true })).toEqual(["B_script"]);
    expect(run({ needsUpdate: true })).toEqual(["B_script"]);
    expect(run({ maps: true })).toEqual(["A_map"]);
    expect(run({ scripts: true })).toEqual(["B_script"]);
    expect(run({ unique: true })).toEqual(["D_unique"]);
    expect(run({ tag: "harvest" })).toEqual(["A_map"]);
    // Filters combine (all must match).
    expect(run({ maps: true, scripts: true })).toEqual([]);
  });

  it("Needs attention keeps mods with a problem or an update", () => {
    const list = filterMods(mods, { ...NO_FILTERS, attention: true }, ctx({ verdict: (m) => (m.techName === "A_map" ? "modified" : null) }));
    expect(names(list)).toEqual(["A_map", "B_script", "C_broken", "D_unique", "F_plain"]);
  });
});

describe("filter chips", () => {
  it("lists the filters in effect, category last", () => {
    const f = { ...NO_FILTERS, status: "parked" as const, favorites: true, maps: true, tag: "harvest" };
    expect(filterChips(f, "Tractors", "Large")).toEqual([
      { id: "status", label: "Parked" },
      { id: "favorites", label: "Favorites" },
      { id: "maps", label: "Maps" },
      { id: "tag", label: "Tag: harvest" },
      { id: "category", label: "Category: Tractors · Large" },
    ]);
    expect(filterChips(NO_FILTERS)).toEqual([]);
    expect(noFilters(NO_FILTERS)).toBe(true);
  });

  it("removes one chip's filter", () => {
    const f = { ...NO_FILTERS, status: "active" as const, maps: true, tag: "harvest" };
    expect(withoutFilter(f, "maps")).toEqual({ ...f, maps: false });
    expect(withoutFilter(f, "status")).toEqual({ ...f, status: "all" });
    expect(withoutFilter(f, "tag")).toEqual({ ...f, tag: null });
    expect(withoutFilter(f, "category")).toBe(f);
  });

  it("counts the popover's filters for the Filters button", () => {
    expect(popoverCount({ ...NO_FILTERS, favorites: true })).toBe(0);
    expect(popoverCount({ ...NO_FILTERS, hidden: true, maps: true, tag: "x" })).toBe(3);
  });
});

describe("remembered Library state and density", () => {
  function memStorage() {
    const m = new Map<string, string>();
    return { getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v), m };
  }

  it("keeps the segment under silo.library.state", () => {
    const st = memStorage();
    expect(loadLibraryState(st)).toEqual({ status: "all" });
    saveLibraryState({ status: "parked" }, st);
    expect(JSON.parse(st.m.get(LIBRARY_STATE_KEY)!)).toEqual({ status: "parked" });
    expect(loadLibraryState(st)).toEqual({ status: "parked" });
    st.setItem(LIBRARY_STATE_KEY, "{not json");
    expect(loadLibraryState(st)).toEqual({ status: "all" });
  });

  it("keeps the density under silo.library.density", () => {
    const st = memStorage();
    expect(loadLibraryDensity(st)).toBe("comfortable");
    saveLibraryDensity("compact", st);
    expect(st.m.get(LIBRARY_DENSITY_KEY)).toBe("compact");
    expect(loadLibraryDensity(st)).toBe("compact");
    const throwing = { getItem: () => { throw new Error("off"); }, setItem: () => { throw new Error("off"); } };
    expect(loadLibraryDensity(throwing)).toBe("comfortable");
    expect(() => saveLibraryDensity("compact", throwing)).not.toThrow();
  });
});

describe("conflictedTechNames (badge, In conflict filter and its count agree)", () => {
  it("collects tech names, not display titles, and skips info notes", () => {
    const s = conflictedTechNames([
      { severity: "warning", mods: ["Courseplay"], techNames: ["FS25_Courseplay"] },
      { severity: "critical", mods: ["Big Tractor", "Other"], techNames: ["FS25_Big", "FS25_Other"] },
      { severity: "info", mods: ["Map A", "Map B"], techNames: ["FS25_MapA", "FS25_MapB"] },
    ]);
    expect(s.has("FS25_Courseplay")).toBe(true);
    expect(s.has("Courseplay")).toBe(false);
    expect([...s].sort()).toEqual(["FS25_Big", "FS25_Courseplay", "FS25_Other"]);
  });
});

describe("needsUpdateOutcome (switching Needs update on)", () => {
  const base = { available: 0, checking: false, checked: true, catalogError: null, blocked: "" };
  it("filters when updates are available", () => {
    expect(needsUpdateOutcome({ ...base, available: 3 })).toEqual({ filter: true });
  });
  it("says up to date only after a clean check", () => {
    expect(needsUpdateOutcome(base)).toEqual({ filter: false, say: "Every mod Silo can check is up to date.", tone: "success" });
  });
  it("doesn't say up to date when the catalog couldn't be reached", () => {
    const o = needsUpdateOutcome({ ...base, checked: false, catalogError: "Silo couldn't reach its catalog." });
    expect(o).toEqual({ filter: false, say: "Silo couldn't reach its catalog.", tone: "warn" });
  });
  it("doesn't say up to date while a check is still running", () => {
    expect(needsUpdateOutcome({ ...base, checked: false, checking: true })).toMatchObject({ filter: false, tone: "warn" });
  });
  it("doesn't say up to date when the check couldn't start", () => {
    const o = needsUpdateOutcome({ ...base, checked: false, blocked: "Wait for the updates to finish." });
    expect(o).toEqual({ filter: false, say: "Wait for the updates to finish.", tone: "warn" });
  });
});

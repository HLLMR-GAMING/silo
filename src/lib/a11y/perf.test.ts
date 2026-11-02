// @vitest-environment jsdom
// §5.5 perf smoke test on the synthetic 2,500-mod library (#258). Budgets are generous (CI
// runners, jsdom) — they catch an accidental O(n²), not a few ms. The real-app numbers come from
// the performance marks in the Library (see src/lib/perf.ts).
//   - filter + search + sort for one keystroke stays well under the 50 ms target's ballpark
//   - sorting matches the previous per-comparison localeCompare order exactly
//   - the virtual list and grid render only the rows on screen plus overscan, at 2,500 items
import { beforeAll, describe, expect, it } from "vitest";
import { fireEvent, render } from "@testing-library/svelte";
import { tick } from "svelte";
import ListHarness from "../components/library/__tests__/ListHarness.svelte";
import GridHarness from "../components/library/__tests__/GridHarness.svelte";
import { filterMods, NO_FILTERS, type FilterContext } from "../components/library/filters";
import { searchMods, sortMods, type SortKey } from "../components/library/libraryList";
import { rowBadges } from "../components/library/rowBadges";
import { groupUpdates } from "../updates/sections";
import type { ModEntry } from "../types";
import { syntheticLibrary } from "./fixtures";

beforeAll(() => {
  if (typeof globalThis.ResizeObserver === "undefined") {
    globalThis.ResizeObserver = class {
      observe() {}
      unobserve() {}
      disconnect() {}
    } as unknown as typeof ResizeObserver;
  }
});

const N = 2500;
const lib = syntheticLibrary(N);
const SORTS: SortKey[] = ["added", "name", "category", "size", "version", "rating"];
const rating = (t: string) => Number(t.slice(-1)) % 6;
const NONE = { favorite: false, hidden: false, broken: false };
const ctx: FilterContext = {
  curation: () => NONE,
  active: (m) => !m.techName.endsWith("0"),
  conflicted: (m) => m.techName.endsWith("7"),
  hasUpdate: (t) => t.endsWith("3"),
  tags: () => [],
};

/** Median wall time of `fn` over `runs` runs, after one warm-up. */
function median(fn: () => void, runs = 7): number {
  fn();
  const times: number[] = [];
  for (let i = 0; i < runs; i++) {
    const t0 = performance.now();
    fn();
    times.push(performance.now() - t0);
  }
  times.sort((a, b) => a - b);
  return times[Math.floor(runs / 2)];
}

/** The Library's derive chain for one state (what App's `filtered` and `visible` do). */
function derive(q: string, by: SortKey, filters = { ...NO_FILTERS }): ModEntry[] {
  return sortMods(searchMods(filterMods(lib, filters, ctx), q), by, "desc", rating);
}

/** The sort before #258: lowercase and localeCompare inside every comparison. */
function oldSort(list: ModEntry[], by: SortKey, dir: "asc" | "desc"): ModEntry[] {
  const arr = [...list];
  const mul = dir === "desc" ? -1 : 1;
  const name = (m: ModEntry) => (m.title ?? m.techName).toLowerCase();
  arr.sort((a, b) => {
    let r = 0;
    switch (by) {
      case "name":
        r = name(a).localeCompare(name(b));
        break;
      case "size":
        r = a.size - b.size;
        break;
      case "added":
        r = a.mtimeMs - b.mtimeMs;
        break;
      case "version":
        r = (a.version ?? "").localeCompare(b.version ?? "", undefined, { numeric: true });
        break;
      case "rating":
        r = rating(a.techName) - rating(b.techName);
        break;
      case "category":
        r =
          a.category.localeCompare(b.category) ||
          (a.subcategory ?? "").localeCompare(b.subcategory ?? "") ||
          name(a).localeCompare(name(b));
        break;
    }
    return r * mul || name(a).localeCompare(name(b));
  });
  return arr;
}

describe("2,500-mod library: derive", () => {
  it("the fixture is 2,500 distinct mods", () => {
    expect(lib).toHaveLength(N);
    expect(new Set(lib.map((m) => m.path)).size).toBe(N);
  });

  it("sorts exactly as before, for every key and direction", () => {
    for (const by of SORTS)
      for (const dir of ["asc", "desc"] as const)
        expect(sortMods(lib, by, dir, rating).map((m) => m.path)).toEqual(oldSort(lib, by, dir).map((m) => m.path));
  });

  it("searches title, tech name and author, never across them", () => {
    expect(searchMods(lib, "fs25_mod1234").map((m) => m.techName)).toEqual(["FS25_Mod1234"]);
    expect(searchMods(lib, "author 7").length).toBeGreaterThan(0);
    expect(searchMods(lib, "")).toBe(lib);
    // "…1234\nfs25" would only match if the fields were joined without a separator check.
    expect(searchMods(lib, "1234fs25")).toHaveLength(0);
  });

  it("one search keystroke (filter + search + sort) stays under budget", () => {
    const ms = median(() => derive("valley h", "added"));
    expect(ms).toBeLessThan(100);
  });

  it("every sort over the whole library stays under budget", () => {
    for (const by of SORTS) {
      const ms = median(() => derive("", by));
      expect(ms, `sort by ${by}`).toBeLessThan(150);
    }
  });

  it("chip filters over the whole library stay under budget", () => {
    const ms = median(() => derive("", "name", { ...NO_FILTERS, attention: true, status: "active" }));
    expect(ms).toBeLessThan(150);
  });

  it("grouping a check over the whole library (2,500 Not tracked) stays under budget", () => {
    const mods = lib.map((m) => ({ techName: m.techName, title: m.title ?? m.techName, path: m.path, version: m.version }));
    let notTracked = 0;
    const ms = median(() => {
      notTracked = groupUpdates({ mods, rows: [], checkYourself: [], catalogReached: true }).notTracked.length;
    });
    expect(notTracked).toBe(N);
    expect(ms).toBeLessThan(100);
  });

  it("badges for a screenful of rows are cheap", () => {
    const shown = derive("", "added").slice(0, 40);
    const ms = median(() => shown.forEach((m) => rowBadges(m, { hasUpdate: true, conflicted: true })));
    expect(ms).toBeLessThan(20);
  });
});

describe("2,500-mod library: virtual rendering", () => {
  const items = lib.map((m) => ({ id: m.path, name: m.title ?? m.techName }));
  // jsdom measures 0, so the list assumes a 600 px viewport.
  const VIEW = 600;
  const OVERSCAN = 6;

  it("the list renders the screenful plus overscan, not 2,500 rows", async () => {
    const { container } = render(ListHarness, { items, rowHeight: 56 });
    await tick();
    const max = Math.ceil(VIEW / 56) + 2 * OVERSCAN + 1;
    const count = () => container.querySelectorAll('[role="listitem"]').length;
    expect(count()).toBeGreaterThan(0);
    expect(count()).toBeLessThanOrEqual(max);
    const vp = container.querySelector<HTMLElement>(".vg-viewport")!;
    vp.scrollTop = 56 * 1200;
    await fireEvent.scroll(vp);
    await tick();
    expect(count()).toBeLessThanOrEqual(max);
    expect(container.querySelector('[data-key="C:/mods/FS25_Mod1200.zip"]')).not.toBeNull();
    expect(container.querySelector('[data-key="C:/mods/FS25_Mod0.zip"]')).toBeNull();
  });

  it("the grid renders whole rows of the screenful plus overscan", async () => {
    const cols = 4;
    const { container } = render(GridHarness, { items, cols });
    await tick();
    const count = container.querySelectorAll('[role="listitem"]').length;
    // itemHeight 100 + the default gap; rows on screen + overscan above and below, whole rows.
    expect(count).toBeGreaterThan(0);
    expect(count).toBeLessThanOrEqual((Math.ceil(VIEW / 100) + 2 * OVERSCAN + 1) * cols);
  });

  it("mounting the 2,500-row list stays under budget", async () => {
    const t0 = performance.now();
    const r = render(ListHarness, { items, rowHeight: 56 });
    await tick();
    const ms = performance.now() - t0;
    r.unmount();
    expect(ms).toBeLessThan(1000);
  });
});

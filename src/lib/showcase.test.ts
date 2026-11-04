// The showcase kit's pure parts (scripts/showcase, docs/SHOWCASE.md): the demo library has the one
// real conflict and the crash it promises, its canned answers never point anywhere real, and the
// safety checks refuse what they should.
import { describe, expect, it } from "vitest";
import type { CollectionList } from "./api";
import { buildCatalog } from "../../scripts/showcase/lib/catalog.mjs";
import { buildStubs } from "../../scripts/showcase/lib/stubs.mjs";
import { personalTextPattern } from "../../scripts/showcase/lib/guard.mjs";
import { checkDestination, cleanLog, crashLog, inputBindingXml } from "../../scripts/showcase/stage.mjs";

const lib = buildCatalog();

describe("showcase demo library", () => {
  it("has about 700 uniquely named mods, with no ampersand anywhere", () => {
    expect(lib.mods.length).toBeGreaterThan(650);
    expect(lib.mods.length).toBeLessThan(750);
    expect(new Set(lib.mods.map((m) => m.techName)).size).toBe(lib.mods.length);
    for (const m of lib.mods) {
      expect(m.techName).toMatch(/^FS25_[A-Za-z0-9]+$/);
      expect(`${m.title} ${m.author}`).not.toContain("&");
    }
  });

  it("is deterministic", () => {
    expect(buildCatalog().mods.map((m) => m.techName)).toEqual(lib.mods.map((m) => m.techName));
  });

  it("holds exactly one real conflict, a uniqueType clash between two authors", () => {
    const byType = new Map<string, typeof lib.mods>();
    for (const m of lib.mods) if (m.uniqueType) byType.set(m.uniqueType, [...(byType.get(m.uniqueType) ?? []), m]);
    const clashes = [...byType.values()].filter((ms) => ms.length > 1);
    expect(clashes).toHaveLength(1);
    const [a, b] = clashes[0];
    expect([a.techName, b.techName].sort()).toEqual([lib.roles.conflictA, lib.roles.conflictB].sort());
    expect(a.author).not.toBe(b.author);
    // Script mods each load their own script, so no script-name clash sneaks in.
    const scripts = lib.mods.filter((m) => !m.store && !m.isMap).map((m) => m.techName);
    expect(new Set(scripts).size).toBe(scripts.length);
  });

  it("starts with one map, the culprit and one conflict mod active", () => {
    const active = new Set(lib.mods.filter((m) => m.active).map((m) => m.techName));
    expect(lib.mods.filter((m) => m.active && m.isMap)).toHaveLength(1);
    expect(active.has(lib.roles.culprit)).toBe(true);
    expect(active.has(lib.roles.conflictA)).toBe(true);
    expect(active.has(lib.roles.conflictB)).toBe(false);
    expect(lib.sets[0].mods).toEqual([...active]);
    expect(lib.sets).toHaveLength(3);
  });

  it("writes a clean log and a crashed one that names the culprit", () => {
    expect(cleanLog(lib)).toContain("Application quit");
    const crash = crashLog(lib);
    expect(crash).not.toContain("Application quit");
    expect(crash).not.toContain("#End.");
    expect(crash).toContain(`mods/${lib.roles.culprit}/`);
    // No absolute paths at all, let alone a home folder.
    expect(crash).not.toMatch(/[A-Z]:[\\/]|\/Users\/|\/home\//);
  });

  it("writes a control map the bindings parser reads", () => {
    const xml = inputBindingXml();
    expect(xml).toMatch(/^<\?xml/);
    expect(xml.match(/<actionBinding /g)?.length).toBeGreaterThan(20);
    expect(xml).toContain('device="GAMEPAD"');
  });
});

describe("showcase stubs", () => {
  const stubs = buildStubs(lib);

  it("answer provenance for the two showcase mods", () => {
    const cases = stubs.verify_mod.cases as Record<string, { status: string }>;
    expect(cases[lib.roles.verified].status).toBe("verified");
    expect(cases[lib.roles.modified].status).toBe("modified");
  });

  it("never point the webview at the network or carry anything token-like", () => {
    const text = JSON.stringify(stubs);
    expect(text).not.toMatch(/gh[pousr]_[A-Za-z0-9]{20,}|github_pat_/);
    const browse = stubs.browse_mods.value as { mods: { imageUrl: string }[] };
    for (const m of browse.mods) expect(m.imageUrl).toMatch(/^data:image\/png;base64,/);
    // Share links are Silo's handoff page with a real-shaped gist id (collection::gist_page_url,
    // github::valid_gist_id), as the export returns.
    const page = /^https:\/\/silo\.hllmr\.com\/c\/\?g=[0-9a-f]{20,40}$/;
    expect((stubs.collection_export.value as { url: string }).url).toMatch(page);
    // The real command's shape (CollectionList): a bare array crashed My sets in the demo (#448).
    const list = stubs.collections_list.value as CollectionList;
    expect(Array.isArray(list.items) && list.items.length > 0).toBe(true);
    expect(list.warning).toBeNull();
    for (const c of list.items) {
      expect(c.pageUrl).toMatch(page);
      expect(c.reference).toMatch(/^[0-9a-f]{20,40}$/);
    }
  });

  it("fail every command that would download or sign in", () => {
    for (const cmd of ["install_remote_mod", "download_update", "gh_device_start", "gh_set_pat", "collection_apply"]) {
      expect(stubs[cmd].error).toBeTruthy();
    }
  });
});

describe("showcase safety checks", () => {
  it("spot a home folder or user name, but not the user name inside a web address", () => {
    const re = personalTextPattern("C:\\Users\\jo", "jo");
    expect(re.test("Mods: C:\\Users\\jo\\Documents")).toBe(true);
    expect(re.test("C:/Users/jo/Documents")).toBe(true);
    expect(re.test("Connected as jo")).toBe(true);
    expect(re.test("https://jo.com/x and joke and F:\\silo-demo")).toBe(false);
  });

  it("refuse a profile inside the home folder", () => {
    const home = "/nonexistent/showcase-test-home";
    expect(() => checkDestination(`${home}/Documents/demo`, home)).toThrow(/home folder/);
    expect(() => checkDestination(home, home)).toThrow(/home folder/);
    // A folder that doesn't exist yet, outside home, is fine.
    expect(() => checkDestination("/nonexistent/silo-demo", home)).not.toThrow();
  });

  it("refuse a non-empty folder that isn't a staged profile", () => {
    // The repo's docs folder (tests run from the repo root): files, no profile marker.
    expect(() => checkDestination("docs", "/nonexistent/home")).toThrow(/isn't empty/);
  });
});

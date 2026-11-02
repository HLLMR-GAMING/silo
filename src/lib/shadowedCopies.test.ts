// Two library rows with one tech name (#421): only the copy in the mods folder shows active.
import { describe, expect, it } from "vitest";
import { activeTechNames, copiesMayHaveMoved, rowActive, shadowedCopies, shadowedText } from "./shadowedCopies";

const row = (techName: string, path: string, active: boolean) => ({ techName, path, active });

// What the scan returns for an archived zip that is parked beside an unzipped folder of the same
// name in the mods folder (scan::the_other_kind_of_an_archived_name_is_its_own_row).
const archived = row("FS25_A", "C:/mods/archive/Tools/FS25_A.zip", false);
const loose = row("FS25_A", "C:/mods/FS25_A", true);
const other = row("FS25_B", "C:/mods/archive/Tools/FS25_B.zip", true);

describe("rowActive", () => {
  it("shows a parked copy parked while another copy of its tech name is in the mods folder", () => {
    const mods = [archived, loose, other];
    const active = activeTechNames(mods);
    const shadowed = shadowedCopies(mods);
    expect(rowActive(loose, active, shadowed)).toBe(true);
    expect(rowActive(archived, active, shadowed)).toBe(false);
    expect(rowActive(other, active, shadowed)).toBe(true);
    expect(shadowed.get(archived.path)).toBe(loose);
    expect(shadowed.size).toBe(1);
  });

  it("follows the optimistic set for a row with no other copy", () => {
    const mods = [other, row("FS25_C", "C:/mods/archive/Tools/FS25_C.zip", false)];
    const shadowed = shadowedCopies(mods);
    expect(shadowed.size).toBe(0);
    // A switch flips the tech name before any rescan: the row follows it either way.
    expect(rowActive(mods[1], new Set(["FS25_B", "FS25_C"]), shadowed)).toBe(true);
    expect(rowActive(mods[0], new Set(), shadowed)).toBe(false);
  });

  it("parks every copy with the tech name; two copies loaded, or two parked, shadow nothing", () => {
    const mods = [archived, loose];
    const shadowed = shadowedCopies(mods);
    expect(rowActive(loose, new Set(), shadowed)).toBe(false);
    expect(rowActive(archived, new Set(), shadowed)).toBe(false);
    // Two copies both in the mods folder: neither is shadowed (the scan lists both as loaded).
    const both = [{ ...archived, active: true }, loose];
    expect(shadowedCopies(both).size).toBe(0);
    // Two parked copies: neither is shadowed, so turning the name on shows both until a rescan.
    const parked = [archived, { ...loose, path: "C:/mods/archive/Misc/FS25_A.zip", active: false }];
    expect(shadowedCopies(parked).size).toBe(0);
  });
});

describe("activeTechNames", () => {
  it("is the tech names the scan found in the mods folder, once each", () => {
    expect([...activeTechNames([archived, loose, other])].sort()).toEqual(["FS25_A", "FS25_B"]);
  });
});

describe("copiesMayHaveMoved", () => {
  it("is false only with no shadowed row and nothing set_active kept parked for another copy", () => {
    const plain = [archived, other];
    expect(copiesMayHaveMoved(undefined, shadowedCopies(plain))).toBe(false);
    expect(copiesMayHaveMoved([], shadowedCopies(plain))).toBe(false);
    // The other copy came since the last scan: set_active kept the archive parked for it.
    expect(copiesMayHaveMoved(["FS25_A"], shadowedCopies(plain))).toBe(true);
    // A row is shown shadowed: its other copy may have gone, and set_active then linked it.
    const dup = [archived, loose, other];
    expect(copiesMayHaveMoved([], shadowedCopies(dup))).toBe(true);
  });
});

describe("shadowedText", () => {
  it("names the copy the game loads and what to do", () => {
    expect(shadowedText("Big Tractor", "FS25_A")).toBe(
      "Big Tractor is already on from another copy in your mods folder, FS25_A. The game loads only one copy of a mod. To use this one, move FS25_A out of your mods folder.",
    );
  });
});

// Two copies of one mod (#423): which tech names Problems lists.
import { describe, expect, it } from "vitest";
import { copyPlace, duplicateCopies, loadedCopy } from "./duplicates";

const row = (techName: string, path: string) => ({ techName, path });

describe("duplicateCopies", () => {
  it("groups every tech name found more than once, compared exactly as the scan does", () => {
    const zip = row("FS25_Foo", "C:/mods/archive/Vehicles/FS25_Foo.zip");
    const folder = row("FS25_Foo", "C:/mods/FS25_Foo");
    const single = row("FS25_Bar", "C:/mods/FS25_Bar.zip");
    expect(duplicateCopies([zip, single, folder])).toEqual([{ techName: "FS25_Foo", copies: [zip, folder] }]);
    // Differing only in case: possibly Silo's own link to the copy, not a second copy.
    expect(duplicateCopies([zip, row("fs25_foo", "C:/mods/fs25_foo.zip")])).toEqual([]);
  });

  it("says where each copy is", () => {
    expect(copyPlace({ path: "C:/mods/archive/Vehicles/FS25_Foo.zip", organized: true })).toBe(
      "in your library, Vehicles",
    );
    expect(copyPlace({ path: "C:\\mods\\FS25_Foo", organized: false })).toBe("in your mods folder");
  });

  it("names the loaded copy only when exactly one is on", () => {
    const a = row("FS25_Foo", "a");
    const b = row("FS25_Foo", "b");
    expect(loadedCopy([a, b], (m) => m === b)).toBe(b);
    expect(loadedCopy([a, b], () => true)).toBeNull();
    expect(loadedCopy([a, b], () => false)).toBeNull();
  });

  it("finds two library copies in different categories", () => {
    const a = row("FS25_Foo", "C:/mods/archive/Vehicles/FS25_Foo.zip");
    const b = row("FS25_Foo", "C:/mods/archive/Tools/FS25_Foo.zip");
    expect(duplicateCopies([a, b])).toHaveLength(1);
  });

  it("leaves a different build at the mod's name to its own section", () => {
    const archived = row("FS25_Foo", "C:/mods/archive/Vehicles/FS25_Foo.zip");
    const theirs = row("FS25_Foo", "C:/mods/FS25_Foo.zip");
    expect(duplicateCopies([archived, theirs], ["FS25_Foo.zip"])).toEqual([]);
    expect(duplicateCopies([archived, theirs])).toHaveLength(1);
  });

  it("is empty with one copy of each", () => {
    expect(duplicateCopies([row("FS25_A", "a"), row("FS25_B", "b")])).toEqual([]);
  });
});

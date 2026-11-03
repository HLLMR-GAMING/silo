// Two copies of one mod (#423): which tech names Problems lists.
import { describe, expect, it } from "vitest";
import { duplicateCopies } from "./duplicates";

const row = (techName: string, path: string) => ({ techName, path });

describe("duplicateCopies", () => {
  it("groups every tech name found more than once, ignoring case", () => {
    const zip = row("FS25_Foo", "C:/mods/archive/Vehicles/FS25_Foo.zip");
    const folder = row("fs25_foo", "C:/mods/fs25_foo");
    const single = row("FS25_Bar", "C:/mods/FS25_Bar.zip");
    expect(duplicateCopies([zip, single, folder])).toEqual([{ techName: "FS25_Foo", copies: [zip, folder] }]);
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

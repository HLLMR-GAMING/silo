// Remove from library (#429): the confirmation and result wording.
import { describe, expect, it } from "vitest";
import {
  binName,
  removeConfirmText,
  removedText,
  removeVerb,
  type RemovingMod,
} from "./removeMods";

const zip = (t: string, on = false): RemovingMod => ({
  path: `C:/mods/archive/Vehicles/FS25_${t}.zip`,
  techName: `FS25_${t}`,
  title: t,
  fileName: `FS25_${t}.zip`,
  kind: "zip",
  on,
});

describe("removeConfirmText", () => {
  it("names one mod, its file, where it goes, and parks it first when it's on", () => {
    expect(removeConfirmText([zip("Tractor", true)], true)).toBe(
      "Remove Tractor from your library? Its file, FS25_Tractor.zip, goes to the Recycle Bin, so you can restore it from there. It's on now, so Silo parks it first.",
    );
    expect(removeConfirmText([zip("Tractor")], false)).toBe(
      "Remove Tractor from your library? Its file, FS25_Tractor.zip, goes to the Trash, so you can restore it from there.",
    );
  });

  it("names a folder mod's folder", () => {
    const folder: RemovingMod = {
      path: "C:/mods/FS25_Shed",
      techName: "FS25_Shed",
      title: null,
      fileName: "FS25_Shed",
      kind: "dir",
      on: false,
    };
    expect(removeConfirmText([folder], true)).toContain(
      "Remove FS25_Shed from your library? Its folder, FS25_Shed, goes",
    );
  });

  it("names up to three of several and counts the rest, with how many are on", () => {
    const mods = [zip("A", true), zip("B"), zip("C"), zip("D", true)];
    expect(removeConfirmText(mods, true)).toBe(
      "Remove 4 mods from your library? Their files and folders go to the Recycle Bin, so you can restore them from there: A (FS25_A.zip), B (FS25_B.zip), C (FS25_C.zip), and 1 more. 2 of them are on now, so Silo parks them first.",
    );
    expect(removeConfirmText([zip("A", true), zip("B", true)], true)).toContain(
      "They're on now, so Silo parks them first.",
    );
    expect(removeConfirmText([zip("A", true), zip("B")], true)).toContain(
      "1 of them is on now, so Silo parks it first.",
    );
  });
});

describe("removedText", () => {
  it("says what went where, or nothing when none went", () => {
    const mods = [zip("A"), zip("B")];
    const went = (m: RemovingMod) => ({ techName: m.techName, path: m.path });
    expect(removedText([went(mods[0])], mods, true)).toBe("Moved A to the Recycle Bin.");
    expect(removedText(mods.map(went), mods, false)).toBe("Moved 2 mods to the Trash.");
    expect(removedText([], mods, true)).toBeNull();
  });

  it("names the copy that went, by path, when two copies share a tech name (#421)", () => {
    const parked = { ...zip("A"), title: "A (library copy)" };
    const loose = { ...zip("A"), title: "A (folder)", path: "C:/mods/FS25_A", kind: "dir" as const };
    expect(removedText([{ techName: "FS25_A", path: loose.path }], [parked, loose], true)).toBe(
      "Moved A (folder) to the Recycle Bin.",
    );
  });

  it("labels the bin and the button by platform", () => {
    expect(binName(true)).toBe("Recycle Bin");
    expect(removeVerb(false)).toBe("Move to Trash");
  });
});

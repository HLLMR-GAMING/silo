// Pure helpers behind the library list: badge order, the key table, scroll-to-reveal and the
// remembered view. #242
import { describe, expect, it } from "vitest";
import { rowBadges } from "./rowBadges";
import { keyAction, scrollToReveal } from "./roving";
import { LIBRARY_VIEW_KEY, loadLibraryView, saveLibraryView } from "./viewPref";

const base = { error: null, ignoredDigitPrefix: false, kind: "zip" as const, isMap: false, scriptCount: 0 };

describe("rowBadges", () => {
  it("is empty for a plain mod", () => {
    expect(rowBadges(base)).toEqual([]);
  });

  it("orders problems, then status, then info", () => {
    const all = rowBadges(
      { error: "boom", ignoredDigitPrefix: true, kind: "dir", isMap: true, scriptCount: 3 },
      { verdict: "modified", hasUpdate: true, hasSettings: true, conflicted: true, flagged: true, hidden: true, shadowedBy: "FS25_A" },
    );
    expect(all.map((b) => b.id)).toEqual([
      "modified",
      "error",
      "ignored",
      "conflict",
      "flagged",
      "shadowed",
      "update",
      "folder",
      "hidden",
      "map",
      "settings",
      "scripts",
    ]);
    const groups = all.map((b) => b.group);
    expect(groups.indexOf("status")).toBeGreaterThan(groups.lastIndexOf("problem"));
    expect(groups.indexOf("info")).toBeGreaterThan(groups.lastIndexOf("status"));
  });

  it("names the copy the game loads on a shadowed copy (#421)", () => {
    const [b] = rowBadges(base, { shadowedBy: "FS25_A" });
    expect(b).toMatchObject({ id: "shadowed", label: "Other copy loads", group: "status" });
    expect(b.title).toBe("The game loads another copy of this mod, FS25_A in your mods folder, so this one stays parked.");
  });

  it("puts Verified build first among status badges and words the script count", () => {
    expect(rowBadges({ ...base, scriptCount: 1 }, { verdict: "verified", hasUpdate: true }).map((b) => b.label)).toEqual([
      "Verified build",
      "Update",
      "1 script",
    ]);
    expect(rowBadges({ ...base, scriptCount: 4 })[0].label).toBe("4 scripts");
  });

  it("never shows Verified and Modified together, and ignores unverified", () => {
    expect(rowBadges(base, { verdict: "unverified" })).toEqual([]);
    expect(rowBadges(base, { verdict: "modified" }).map((b) => b.id)).toEqual(["modified"]);
  });

  it("gives every badge an icon and a tooltip", () => {
    for (const b of rowBadges(
      { error: "x", ignoredDigitPrefix: true, kind: "dir", isMap: true, scriptCount: 2 },
      { verdict: "verified", hasUpdate: true, hasSettings: true },
    )) {
      expect(b.icon).toBeTruthy();
      expect(b.title).toBeTruthy();
    }
  });
});

describe("keyAction", () => {
  it("Ctrl+A / Cmd+A selects all, on the item or a control in it (C14)", () => {
    const ctx = { index: 0, count: 3, columns: 1, pageRows: 1, onItem: false };
    expect(keyAction({ key: "a", ctrlKey: true }, ctx)).toEqual({ kind: "selectAll" });
    expect(keyAction({ key: "A", metaKey: true }, { ...ctx, onItem: true })).toEqual({ kind: "selectAll" });
    expect(keyAction({ key: "a" }, ctx)).toBeNull();
    expect(keyAction({ key: "a", ctrlKey: true, shiftKey: true }, ctx)).toBeNull();
  });
  const ctx = { index: 5, count: 20, columns: 1, pageRows: 4, onItem: true };
  it("moves with arrows, Home, End and the page keys, clamped", () => {
    expect(keyAction({ key: "ArrowDown" }, ctx)).toEqual({ kind: "move", to: 6, extend: false });
    expect(keyAction({ key: "ArrowUp" }, ctx)).toEqual({ kind: "move", to: 4, extend: false });
    expect(keyAction({ key: "Home" }, ctx)).toEqual({ kind: "move", to: 0, extend: false });
    expect(keyAction({ key: "End" }, ctx)).toEqual({ kind: "move", to: 19, extend: false });
    expect(keyAction({ key: "PageDown" }, ctx)).toEqual({ kind: "move", to: 9, extend: false });
    expect(keyAction({ key: "PageUp" }, ctx)).toEqual({ kind: "move", to: 1, extend: false });
    expect(keyAction({ key: "PageDown" }, { ...ctx, index: 18 })).toEqual({ kind: "move", to: 19, extend: false });
    expect(keyAction({ key: "ArrowUp" }, { ...ctx, index: 0 })).toEqual({ kind: "move", to: 0, extend: false });
  });

  it("leaves ←/→ to lists and moves by a row in grids", () => {
    expect(keyAction({ key: "ArrowRight" }, ctx)).toBeNull();
    const grid = { ...ctx, columns: 3, twoD: true };
    expect(keyAction({ key: "ArrowRight" }, grid)).toEqual({ kind: "move", to: 6, extend: false });
    expect(keyAction({ key: "ArrowLeft" }, grid)).toEqual({ kind: "move", to: 4, extend: false });
    expect(keyAction({ key: "ArrowDown" }, grid)).toEqual({ kind: "move", to: 8, extend: false });
    expect(keyAction({ key: "PageDown" }, grid)).toEqual({ kind: "move", to: 17, extend: false });
  });

  it("Enter and Space act only on the item itself", () => {
    expect(keyAction({ key: "Enter" }, ctx)).toEqual({ kind: "open" });
    expect(keyAction({ key: " " }, ctx)).toEqual({ kind: "toggle" });
    expect(keyAction({ key: "Enter" }, { ...ctx, onItem: false })).toBeNull();
    expect(keyAction({ key: " " }, { ...ctx, onItem: false })).toBeNull();
    expect(keyAction({ key: "ArrowDown" }, { ...ctx, onItem: false })).toEqual({ kind: "move", to: 6, extend: false });
  });

  it("maps the selection and menu gestures", () => {
    expect(keyAction({ key: "ArrowDown", shiftKey: true }, ctx)).toEqual({ kind: "move", to: 6, extend: true });
    expect(keyAction({ key: " ", ctrlKey: true }, ctx)).toEqual({ kind: "select" });
    expect(keyAction({ key: " ", metaKey: true }, ctx)).toEqual({ kind: "select" });
    expect(keyAction({ key: "F10", shiftKey: true }, ctx)).toEqual({ kind: "menu" });
    expect(keyAction({ key: "F10", shiftKey: true }, { ...ctx, onItem: false })).toEqual({ kind: "menu" });
    expect(keyAction({ key: "ContextMenu" }, ctx)).toEqual({ kind: "menu" });
    expect(keyAction({ key: "F10" }, ctx)).toBeNull();
  });

  it("lets other keys and Alt combinations through", () => {
    expect(keyAction({ key: "a" }, ctx)).toBeNull();
    expect(keyAction({ key: "ArrowDown", altKey: true }, ctx)).toBeNull();
    expect(keyAction({ key: "ArrowDown", ctrlKey: true }, ctx)).toBeNull();
    expect(keyAction({ key: "ArrowDown" }, { ...ctx, count: 0 })).toBeNull();
  });
});

describe("scrollToReveal", () => {
  it("returns null when the row is already in view", () => {
    expect(scrollToReveal(0, 56, 56, 0, 600)).toBeNull();
    expect(scrollToReveal(5, 56, 56, 0, 600)).toBeNull();
  });
  it("scrolls up to a row above the viewport and down to one below it", () => {
    expect(scrollToReveal(2, 56, 56, 500, 600)).toBe(104);
    expect(scrollToReveal(20, 56, 56, 0, 600)).toBe(20 * 56 + 56 + 8 - 600);
  });
});

describe("library view preference", () => {
  function mem(init: Record<string, string> = {}) {
    const data = { ...init };
    return {
      data,
      getItem: (k: string) => data[k] ?? null,
      setItem: (k: string, v: string) => {
        data[k] = v;
      },
    };
  }
  it("defaults to the list and remembers cards", () => {
    const s = mem();
    expect(loadLibraryView(s)).toBe("list");
    saveLibraryView("cards", s);
    expect(s.data[LIBRARY_VIEW_KEY]).toBe("cards");
    expect(loadLibraryView(s)).toBe("cards");
  });
  it("ignores junk, missing storage and storage that throws", () => {
    expect(loadLibraryView(mem({ [LIBRARY_VIEW_KEY]: "tiles" }))).toBe("list");
    expect(loadLibraryView(null)).toBe("list");
    const broken = {
      getItem: () => {
        throw new Error("denied");
      },
      setItem: () => {
        throw new Error("denied");
      },
    };
    expect(loadLibraryView(broken)).toBe("list");
    expect(() => saveLibraryView("cards", broken)).not.toThrow();
  });
});

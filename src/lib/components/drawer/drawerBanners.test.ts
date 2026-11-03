// @vitest-environment jsdom
// The drawer's "a different build" banner: a build dropped into the mods folder offers to use it;
// a library copy changed since Silo put the mods-folder one there offers the library's (#431).
import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import DrawerBanners from "./DrawerBanners.svelte";
import type { ForeignFile } from "../../api";

function setup(foreign: ForeignFile, foreignIsNewer = false) {
  const onAdoptForeign = vi.fn();
  const onRestoreForeign = vi.fn();
  render(DrawerBanners, {
    foreign,
    foreignIsNewer,
    onAdoptForeign,
    onRestoreForeign,
    logErrors: null,
    crashed: false,
    partners: [],
    conflicts: [],
    selfNames: [],
    onSeeLastRun: () => {},
    onSeeConflicts: () => {},
  });
  return { user: userEvent.setup(), onAdoptForeign, onRestoreForeign };
}

const file = (archiveChanged: boolean): ForeignFile => ({
  techName: "FS25_Plow",
  fileName: "FS25_Plow.zip",
  kind: "zip",
  flatVersion: "1.0.0.0",
  managedVersion: "1.1.0.0",
  archiveChanged,
  newlyNoted: false,
});

describe("DrawerBanners: a different build", () => {
  it("a build dropped into the mods folder offers to use it", async () => {
    const h = setup({ ...file(false), flatVersion: "1.2.0.0" }, true);
    expect(screen.getByText("A newer build is in your mods folder")).toBeInTheDocument();
    await h.user.click(screen.getByRole("button", { name: "Use this version" }));
    expect(h.onAdoptForeign).toHaveBeenCalledOnce();
    await h.user.click(screen.getByRole("button", { name: "Keep Silo's 1.1.0.0" }));
    expect(h.onRestoreForeign).toHaveBeenCalledOnce();
  });

  it("a library copy that changed offers the library's, or keeping the mods-folder one", async () => {
    const h = setup(file(true));
    expect(screen.getByText("The copy in your library changed")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Use this version" })).toBeNull();
    await h.user.click(screen.getByRole("button", { name: "Use the library's 1.1.0.0" }));
    expect(h.onRestoreForeign).toHaveBeenCalledOnce();
    await h.user.click(screen.getByRole("button", { name: "Keep 1.0.0.0" }));
    expect(h.onAdoptForeign).toHaveBeenCalledOnce();
  });
});

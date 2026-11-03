// @vitest-environment jsdom
// Browse card, Browse drawer and library drawer pictures (#428): a picture that fails to load is
// dropped (the card shows its letter placeholder), and a late error from an earlier element,
// even one with the same URL, never drops the picture shown now.
import { describe, expect, it, vi } from "vitest";
import { fireEvent, render } from "@testing-library/svelte";
import { tick } from "svelte";
import ModCard from "./ModCard.svelte";
import BrowseDrawer from "./BrowseDrawer.svelte";
import DrawerAbout from "./drawer/DrawerAbout.svelte";
import type { BrowseMod, CatalogModDetail } from "../types";

vi.mock("../api", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../api")>()),
  catalogImage: vi.fn(() => Promise.resolve("")),
  openExternal: vi.fn(() => Promise.resolve()),
}));

const URL_A = "https://silo-api.hllmr.com/images/1.png";
const mod = (id: string, title: string, imageUrl = URL_A) =>
  ({
    id,
    techName: `FS25_${title}`,
    title,
    author: null,
    category: null,
    description: null,
    imageUrl,
    latestVersion: null,
    trustScore: null,
    updatedAt: null,
    sources: [],
    pageUrl: null,
  }) as unknown as BrowseMod & CatalogModDetail;

async function settle() {
  for (let i = 0; i < 4; i++) await tick();
  await new Promise((r) => setTimeout(r, 0));
  await tick();
}

const cases = [
  {
    name: "Browse card",
    sel: ".thumb img",
    mount: (m: ReturnType<typeof mod>) => ({
      m,
      state: { kind: "installed" as const },
      badges: [],
      onPrimary: () => {},
      onOpenDetail: () => {},
    }),
    comp: ModCard,
  },
  {
    name: "Browse drawer",
    sel: "img.drawer-img",
    mount: (m: ReturnType<typeof mod>) => ({
      detail: m,
      detailLoading: false,
      installingId: null,
      gh: { connected: false, canWrite: false },
      installed: new Set<string>(),
      onClose: () => {},
      onUseSource: () => {},
      onOpenDesc: () => {},
    }),
    comp: BrowseDrawer,
  },
  {
    name: "library drawer About",
    sel: "img.cover",
    mount: (m: ReturnType<typeof mod>) => ({ catalog: m, title: m.title }),
    comp: DrawerAbout,
  },
] as const;

describe.each(cases)("$name picture", ({ sel, mount, comp }) => {
  it("is dropped when it fails to load", async () => {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const { container } = render(comp as any, mount(mod("1", "Tractor")));
    await settle();
    const img = container.querySelector(sel);
    expect(img).not.toBeNull();
    await fireEvent.error(img!);
    expect(container.querySelector(sel)).toBeNull();
  });

  it("ignores a late error from an earlier element, same URL or not", async () => {
    for (const nextUrl of [URL_A, "https://silo-api.hllmr.com/images/2.png"]) {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const { container, rerender, unmount } = render(
        comp as any,
        mount(mod("1", "Tractor")),
      );
      await settle();
      const old = container.querySelector(sel)!;
      await rerender(mount(mod("2", "Baler", nextUrl)));
      await settle();
      const now = container.querySelector(sel);
      expect(now?.getAttribute("src")).toBe(nextUrl);
      if (now !== old) {
        await fireEvent.error(old);
        expect(container.querySelector(sel)?.getAttribute("src")).toBe(nextUrl);
      }
      // The picture shown now still drops on its own failure.
      await fireEvent.error(container.querySelector(sel)!);
      expect(container.querySelector(sel)).toBeNull();
      unmount();
    }
  });
});

describe("Browse card placeholder", () => {
  it("shows the letter when the picture fails", async () => {
    const { container } = render(ModCard, cases[0].mount(mod("1", "Tractor")));
    await settle();
    await fireEvent.error(container.querySelector(".thumb img")!);
    expect(container.querySelector(".thumb-fallback")?.textContent).toBe("T");
  });
});

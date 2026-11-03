// @vitest-environment jsdom
// A Browse card whose thumbnail can't load (the image host is down) shows the letter
// placeholder, not the browser's broken-image icon (#428).
import { describe, expect, it } from "vitest";
import { fireEvent, render } from "@testing-library/svelte";
import { tick } from "svelte";
import ModCard from "./ModCard.svelte";
import { failedIsCurrent } from "../browse";
import type { BrowseMod } from "../types";

const mod = {
  id: "1",
  techName: "FS25_Tractor",
  title: "Tractor",
  author: null,
  category: null,
  description: null,
  imageUrl: "https://silo-api.hllmr.com/images/1.png",
  latestVersion: null,
  trustScore: null,
  updatedAt: null,
  sources: [],
} as unknown as BrowseMod;

describe("Browse card thumbnail", () => {
  it("falls back to the letter placeholder when the image fails to load", async () => {
    const { container } = render(ModCard, {
      m: mod,
      state: { kind: "installed" },
      badges: [],
      onPrimary: () => {},
      onOpenDetail: () => {},
    });
    await tick();
    await tick();
    const img = container.querySelector(".thumb img");
    expect(img).not.toBeNull();
    await fireEvent.error(img!);
    expect(container.querySelector(".thumb img")).toBeNull();
    expect(container.querySelector(".thumb-fallback")?.textContent).toBe("T");
  });

  it("ignores a late error from the previous mod's picture", async () => {
    const props = {
      m: mod,
      state: { kind: "installed" as const },
      badges: [],
      onPrimary: () => {},
      onOpenDetail: () => {},
    };
    const { container, rerender } = render(ModCard, props);
    await tick();
    const old = container.querySelector(".thumb img")!;
    const next = {
      ...mod,
      id: "2",
      title: "Baler",
      imageUrl: "https://silo-api.hllmr.com/images/2.png",
    } as BrowseMod;
    await rerender({ ...props, m: next });
    await tick();
    await tick();
    await fireEvent.error(old);
    const img = container.querySelector(".thumb img");
    expect(img?.getAttribute("src")).toBe(
      "https://silo-api.hllmr.com/images/2.png",
    );
    // Its own failure still falls back.
    await fireEvent.error(img!);
    expect(container.querySelector(".thumb-fallback")?.textContent).toBe("B");
  });
});

describe("failedIsCurrent", () => {
  it("is true only for an error on the element showing the current picture", () => {
    const img = document.createElement("img");
    img.setAttribute("src", "https://a/1.png");
    const ev = new Event("error");
    img.addEventListener("error", (e) => {
      expect(failedIsCurrent(e, "https://a/1.png")).toBe(true);
      expect(failedIsCurrent(e, "https://a/2.png")).toBe(false);
    });
    img.dispatchEvent(ev);
  });
});

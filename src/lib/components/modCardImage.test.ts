// @vitest-environment jsdom
// A Browse card whose thumbnail can't load (the image host is down) shows the letter
// placeholder, not the browser's broken-image icon (#428).
import { describe, expect, it } from "vitest";
import { fireEvent, render } from "@testing-library/svelte";
import { tick } from "svelte";
import ModCard from "./ModCard.svelte";
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
});

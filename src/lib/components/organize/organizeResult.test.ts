// @vitest-environment jsdom
// The Organize result after a mod vanished before confirm (#427): the failure says it's gone, and
// the summary doesn't claim the failed ones are still in the mods folder.
import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/svelte";
import { tick } from "svelte";
import OrganizePreview from "../OrganizePreview.svelte";
import type { PlannedMove } from "../../types";

const mv = (techName: string, category: string): PlannedMove => ({
  techName,
  fileName: `${techName}.zip`,
  category,
  relFrom: `${techName}.zip`,
  relTo: `archive/${category}/${techName}.zip`,
});
const gone = (t: string) => ({
  techName: t,
  fileName: `${t}.zip`,
  message: "is no longer there, so Silo didn't file it.",
});

function show(failures: ReturnType<typeof gone>[], filed: number) {
  render(OrganizePreview, {
    plan: [mv("FS25_A", "Tractors"), mv("FS25_B", "Maps")],
    loading: false,
    applying: false,
    failures,
    filed,
    onConfirm: vi.fn(),
    onCancel: vi.fn(),
    onCopyDetails: vi.fn(),
  });
}

describe("Organize result with a vanished mod", () => {
  it("one of two gone: names it, and doesn't say it's still in the mods folder", async () => {
    show([gone("FS25_B")], 1);
    await tick();
    const alert = screen.getByRole("alert");
    expect(alert).toHaveTextContent("Silo filed 1 of 2 mods.");
    expect(alert).toHaveTextContent(
      "FS25_B.zip: is no longer there, so Silo didn't file it.",
    );
    expect(alert).toHaveTextContent("Silo moved none of the ones above.");
    expect(alert).not.toHaveTextContent("still in your mods folder");
  });

  it("both gone: names both", async () => {
    show([gone("FS25_A"), gone("FS25_B")], 0);
    await tick();
    const alert = screen.getByRole("alert");
    expect(alert).toHaveTextContent("Silo filed 0 of 2 mods.");
    expect(alert).toHaveTextContent("FS25_A.zip: is no longer there");
    expect(alert).toHaveTextContent("FS25_B.zip: is no longer there");
  });
});

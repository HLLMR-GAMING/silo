// Colour-vision check of the notice and badge set (#258, design §4.1): meaning is never colour
// alone. Under simulated protanopia and deuteranopia (Machado et al. 2009, severity 1.0) every
// tone's text still reads at 4.5:1 on its soft tint, and every badge and notice carries a word and
// an icon, because some tones (warn vs danger) stop being tellable apart by hue.
import { describe, expect, it } from "vitest";
import { contrast, readTokens, resolve, themes, type Rgb } from "../../../scripts/check-contrast.mjs";
import { rowBadges } from "../components/library/rowBadges";

/** Machado, Oliveira & Fernandes (2009), full severity, applied in linear RGB. */
const MACHADO: Record<string, number[][]> = {
  protanopia: [
    [0.152286, 1.052583, -0.204868],
    [0.114503, 0.786281, 0.099216],
    [-0.003882, -0.048116, 1.051998],
  ],
  deuteranopia: [
    [0.367322, 0.860646, -0.227968],
    [0.280085, 0.672501, 0.047413],
    [-0.01182, 0.04294, 0.968881],
  ],
};

const toLin = (c: number) => {
  const s = c / 255;
  return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
};
const toSrgb = (l: number) => {
  const v = Math.min(1, Math.max(0, l));
  return Math.round(255 * (v <= 0.0031308 ? v * 12.92 : 1.055 * v ** (1 / 2.4) - 0.055));
};

/** How `rgb` looks to someone with the given colour-vision deficiency. */
function simulate(rgb: Rgb, kind: keyof typeof MACHADO): Rgb {
  const lin = rgb.map(toLin);
  return MACHADO[kind].map((row) => toSrgb(row[0] * lin[0] + row[1] * lin[1] + row[2] * lin[2])) as Rgb;
}

const TONES: [string, string][] = [
  ["--info", "--info-soft"],
  ["--success", "--success-soft"],
  ["--warn-text", "--warn-soft"],
  ["--danger", "--danger-soft"],
  ["--accent-text", "--accent-soft"],
];

describe("colour-vision simulation", () => {
  const t = themes(readTokens());
  for (const [theme, vars] of Object.entries(t)) {
    for (const kind of Object.keys(MACHADO)) {
      it(`${theme}, ${kind}: each tone's text stays readable on its tint`, () => {
        for (const [fg, bg] of TONES) {
          const ratio = contrast(simulate(resolve(fg, vars), kind), simulate(resolve(bg, vars), kind));
          expect(ratio, `${fg} on ${bg}`).toBeGreaterThanOrEqual(4.5);
        }
      });
    }
  }

  it("leaves greys alone (sanity)", () => {
    expect(simulate([128, 128, 128], "deuteranopia")).toEqual([128, 128, 128]);
  });
});

describe("never colour alone", () => {
  it("every library badge has a word and an icon", () => {
    const all = rowBadges(
      { error: "bad zip", ignoredDigitPrefix: true, kind: "dir", isMap: true, scriptCount: 2 },
      { verdict: "modified", hasUpdate: true, hasSettings: true, conflicted: true, flagged: true, hidden: true, shadowedBy: "FS25_A" },
    );
    const verified = rowBadges({ error: null, ignoredDigitPrefix: false, kind: "zip", isMap: false, scriptCount: 0 }, { verdict: "verified" });
    for (const b of [...all, ...verified]) {
      expect(b.label.trim(), b.id).not.toBe("");
      expect(b.icon, b.id).toBeTruthy();
    }
    // Same tone, different meaning: the words tell them apart.
    const warn = all.filter((b) => b.tone === "warn").map((b) => b.label);
    expect(new Set(warn).size).toBe(warn.length);
  });
});

import { describe, expect, it, vi } from "vitest";
import { foreignChecker, noteRescanner } from "./noteRescan";

describe("noteRescanner (#436)", () => {
  it("rescans once when a check noted a file, not when it found nothing new", () => {
    const rescan = vi.fn();
    const r = noteRescanner(() => false, rescan);
    r.checked([{ newlyNoted: false }]);
    r.checked([]);
    expect(rescan).not.toHaveBeenCalled();
    r.checked([{ newlyNoted: false }, { newlyNoted: true }]);
    expect(rescan).toHaveBeenCalledOnce();
    r.scanEnded();
    expect(rescan).toHaveBeenCalledOnce();
  });

  it("follows a scan still running with one more", () => {
    let scanning = true;
    const rescan = vi.fn();
    const r = noteRescanner(() => scanning, rescan);
    r.checked([{ newlyNoted: true }]);
    r.checked([{ newlyNoted: true }]);
    expect(rescan).not.toHaveBeenCalled();
    scanning = false;
    r.scanEnded();
    expect(rescan).toHaveBeenCalledOnce();
    r.scanEnded();
    expect(rescan).toHaveBeenCalledOnce();
  });
});

describe("foreignChecker (#358, #436)", () => {
  type Found = { fileName: string; newlyNoted: boolean };
  /** A check per call, answered later by hand, in any order. */
  function harness() {
    const pending: { root: string | undefined; answer: (f: Found[]) => void }[] = [];
    let root: string | undefined = "C:/mods";
    let scanning = false;
    const rescan = vi.fn();
    const answered = vi.fn();
    const check = foreignChecker<Found>({
      detect: (r) => new Promise((resolve) => pending.push({ root: r, answer: resolve })),
      root: () => root,
      isScanning: () => scanning,
      rescan,
      started: () => {},
      answered,
    });
    return {
      check,
      pending,
      rescan,
      answered,
      setRoot: (r: string) => (root = r),
      setScanning: (v: boolean) => (scanning = v),
    };
  }
  const noted = (fileName: string, newlyNoted: boolean): Found => ({ fileName, newlyNoted });

  it("a superseded check that wrote a note still gets its rescan; only the latest answer shows", async () => {
    const h = harness();
    const older = h.check.refresh();
    const newer = h.check.refresh();
    // The older check writes the note; the newer one then finds it already there.
    h.pending[0].answer([noted("FS25_A.zip", true)]);
    await older;
    h.pending[1].answer([noted("FS25_A.zip", false)]);
    await newer;
    expect(h.rescan).toHaveBeenCalledOnce();
    expect(h.answered).toHaveBeenCalledOnce();
    expect(h.answered).toHaveBeenCalledWith([noted("FS25_A.zip", false)], true);
  });

  it("a check of a folder switched away from answers nothing and asks for no rescan", async () => {
    const h = harness();
    const run = h.check.refresh();
    h.setRoot("D:/other");
    h.check.drop();
    h.pending[0].answer([noted("FS25_A.zip", true)]);
    await run;
    expect(h.rescan).not.toHaveBeenCalled();
    expect(h.answered).not.toHaveBeenCalled();
  });

  it("a note found while a scan runs waits for it to end", async () => {
    const h = harness();
    h.setScanning(true);
    const run = h.check.refresh();
    h.pending[0].answer([noted("FS25_A.zip", true)]);
    await run;
    expect(h.rescan).not.toHaveBeenCalled();
    h.setScanning(false);
    h.check.scanEnded();
    expect(h.rescan).toHaveBeenCalledOnce();
  });
});

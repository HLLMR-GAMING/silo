import { describe, expect, it, vi } from "vitest";
import { noteRescanner } from "./noteRescan";

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

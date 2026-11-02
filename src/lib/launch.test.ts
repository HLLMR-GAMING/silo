// Play while a paused library move left mods unlinked (#415): ask, then start only on "Play anyway".
import { beforeEach, describe, expect, it, vi } from "vitest";
import { launchGame } from "./api";
import { AppError } from "./errors";
import { launchAsking } from "./launch";

vi.mock("./api", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./api")>()),
  launchGame: vi.fn(),
}));
const ui = vi.hoisted(() => ({ ask: vi.fn() }));
vi.mock("./ui", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./ui")>()),
  ask: ui.ask,
}));

const broken = { code: "MOVE_LINKS_BROKEN", message: "The paused library move left FS25_A without a working link." };

describe("launchAsking", () => {
  beforeEach(() => {
    vi.mocked(launchGame).mockReset();
    ui.ask.mockReset();
  });

  it("starts the game at once when nothing is in the way, asking nothing", async () => {
    vi.mocked(launchGame).mockResolvedValue(undefined);
    expect(await launchAsking("/f/prof")).toBe(true);
    expect(launchGame).toHaveBeenCalledExactlyOnceWith("/f/prof");
    expect(ui.ask).not.toHaveBeenCalled();
  });

  it("asks with the backend's words, Cancel focused, and starts only on Play anyway", async () => {
    vi.mocked(launchGame).mockRejectedValueOnce(broken).mockResolvedValueOnce(undefined);
    ui.ask.mockResolvedValue(true);
    expect(await launchAsking(null)).toBe(true);
    expect(ui.ask).toHaveBeenCalledOnce();
    expect(ui.ask.mock.calls[0][0]).toMatchObject({ body: broken.message, confirm: "Play anyway", focus: "cancel" });
    expect(launchGame).toHaveBeenNthCalledWith(2, null, { playAnyway: true });
  });

  it("doesn't start the game when the user says no", async () => {
    vi.mocked(launchGame).mockRejectedValue(broken);
    ui.ask.mockResolvedValue(false);
    expect(await launchAsking(undefined)).toBe(false);
    expect(launchGame).toHaveBeenCalledOnce();
  });

  it("passes any other refusal on, without asking", async () => {
    const running = new AppError("MOVE_RUNNING", "Silo is moving your library archive. Pause the move to play.");
    vi.mocked(launchGame).mockRejectedValue(running);
    await expect(launchAsking(null)).rejects.toBe(running);
    expect(ui.ask).not.toHaveBeenCalled();
  });

  it("a refusal of the second try (the move started meanwhile) is passed on", async () => {
    const running = { code: "MOVE_RUNNING", message: "Pause the move to play." };
    vi.mocked(launchGame).mockRejectedValueOnce(broken).mockRejectedValueOnce(running);
    ui.ask.mockResolvedValue(true);
    await expect(launchAsking(null)).rejects.toBe(running);
  });
});

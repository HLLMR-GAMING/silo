// @vitest-environment jsdom
// Diagnose (#260): Conflicts (labels, hidden notes, Park one), Problems (actions per row), Last
// run (actions on a named mod, the crash notice's Find the problem mod) and the bisection finish
// that puts the mods back and parks the culprit.
import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import ConflictsPanel from "./panels/ConflictsPanel.svelte";
import ProblemsPanel from "./panels/ProblemsPanel.svelte";
import LogTriage from "./LogTriage.svelte";
import BisectFlow from "./BisectFlow.svelte";
import {
  bisectNarrow,
  bisectPlan,
  bisectSnapshotClear,
  bisectSnapshotGet,
  bisectSnapshotSave,
  copyText,
  launchGame,
  logStatus,
  scanLog,
} from "../api";
import type { Conflict, LogReport, ModEntry } from "../types";

vi.mock("../api", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../api")>()),
  scanLog: vi.fn(),
  logStatus: vi.fn(),
  copyText: vi.fn(() => Promise.resolve()),
  bisectPlan: vi.fn(),
  bisectNarrow: vi.fn(),
  bisectSnapshotSave: vi.fn(() => Promise.resolve()),
  bisectSnapshotGet: vi.fn(),
  bisectSnapshotClear: vi.fn(() => Promise.resolve()),
  launchGame: vi.fn(() => Promise.resolve()),
  revealInFolder: vi.fn(() => Promise.resolve()),
  userDirPath: vi.fn(() => Promise.resolve("C:/fs25")),
}));

const ui = vi.hoisted(() => ({ ask: vi.fn(() => Promise.resolve(true)) }));
vi.mock("../ui", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../ui")>()),
  ask: ui.ask,
}));

function conflict(over: Partial<Conflict>): Conflict {
  return {
    id: "x",
    severity: "critical",
    kind: "uniqueType",
    name: "seedSlot",
    explanation: "These mods use the same exclusive slot (\"seedSlot\"). The game loads only one mod per slot, so the others won't take effect.",
    mods: ["Seeder A", "Seeder B"],
    techNames: ["FS25_SeederA", "FS25_SeederB"],
    ...over,
  };
}

describe("Conflicts", () => {
  const list: Conflict[] = [
    conflict({ id: "u" }),
    // A shared script file name is only ever a note now (#352: each mod's scripts load apart),
    // so the Review row is a fill type.
    conflict({ id: "s", severity: "warning", kind: "fillType", name: "ROCKS" }),
    conflict({
      id: "m",
      severity: "info",
      kind: "map",
      name: "4 maps active",
      explanation: "Only the map your savegame uses loads.",
      mods: ["Map 1", "Map 2", "Map 3", "Map 4"],
      techNames: ["M1", "M2", "M3", "M4"],
    }),
  ];

  it("plain headline and labels; notes hidden until Show notes", async () => {
    const user = userEvent.setup();
    render(ConflictsPanel, { conflicts: list, activeCount: 40 });
    const head = screen.getByRole("heading", { level: 3 });
    expect(head).toHaveTextContent("1 problem · 1 to review · 1 note hidden");
    expect(screen.getByText("Problem")).toBeInTheDocument();
    expect(screen.getByText("Review")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Two mods claim the same exclusive slot" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Two mods define the same fill type" })).toBeInTheDocument();
    // Raw wire words never show.
    expect(screen.queryByText("critical")).toBeNull();
    expect(screen.queryByText("More than one map is active")).toBeNull();

    await user.click(screen.getByRole("switch", { name: "Show notes" }));
    expect(head).toHaveTextContent("1 note");
    expect(head).not.toHaveTextContent("hidden");
    expect(screen.getByText("Note")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "More than one map is active" })).toBeInTheDocument();
    expect(screen.getByText("Only the map your savegame uses loads.")).toBeInTheDocument();
  });

  it("mods open the drawer; Park one… parks the chosen mod; the maps note has no Park", async () => {
    const user = userEvent.setup();
    const onOpen = vi.fn();
    const onPark = vi.fn();
    render(ConflictsPanel, { conflicts: list, activeCount: 40, onOpen, onPark, showNotes: true });
    await user.click(screen.getAllByRole("button", { name: "Seeder B" })[0]);
    expect(onOpen).toHaveBeenCalledWith("FS25_SeederB");
    // Two clashes (exclusive slot, fill type) get a menu; the maps note doesn't.
    const menus = screen.getAllByRole("button", { name: "Park one…" });
    expect(menus).toHaveLength(2);
    await user.click(menus[0]);
    await user.click(await screen.findByRole("menuitem", { name: "Park Seeder A" }));
    expect(onPark).toHaveBeenCalledWith("FS25_SeederA");
  });

  it("a shared script name is a note: hidden until Show notes, not counted to review (#352)", async () => {
    const user = userEvent.setup();
    const script = conflict({ id: "lua", severity: "info", kind: "script", name: "main.lua" });
    render(ConflictsPanel, { conflicts: [list[0], script], activeCount: 40 });
    const head = screen.getByRole("heading", { level: 3 });
    expect(head).toHaveTextContent("1 problem · 0 to review · 1 note hidden");
    expect(screen.queryByRole("heading", { name: "Two mods ship a script with the same file name" })).toBeNull();
    await user.click(screen.getByRole("switch", { name: "Show notes" }));
    expect(screen.getByRole("heading", { name: "Two mods ship a script with the same file name" })).toBeInTheDocument();
  });

  it("only notes: says nothing needs doing", () => {
    render(ConflictsPanel, { conflicts: [list[2]], activeCount: 4 });
    expect(screen.getByText(/No problems and nothing to review/)).toBeInTheDocument();
  });
});

function entry(techName: string, over: Partial<ModEntry> = {}): ModEntry {
  return { techName, title: techName.replace("FS25_", ""), path: `C:/lib/${techName}.zip`, ...over } as ModEntry;
}

describe("Problems", () => {
  it("lists a mod found twice, and Keep this one keeps that copy and removes the other (#423)", async () => {
    const user = userEvent.setup();
    const parked = entry("FS25_Foo", { path: "C:/mods/archive/Vehicles/FS25_Foo.zip", organized: true, version: "1.0.0.0" });
    const loose = entry("FS25_Foo", { path: "C:/mods/FS25_Foo", organized: false, version: "1.1.0.0" });
    const onKeepCopy = vi.fn();
    render(ProblemsPanel, {
      health: { missingDeps: [], corrupt: [], ignored: [], foreign: [], duplicates: [{ techName: "FS25_Foo", copies: [parked, loose] }] },
      onResolveForeign: vi.fn(),
      loadsNow: (m: ModEntry) => m.path === loose.path,
      onKeepCopy,
    });
    expect(screen.getByRole("heading", { level: 3 })).toHaveTextContent("1 mod needs attention");
    expect(screen.getByRole("heading", { level: 4 })).toHaveTextContent("More than one copy of a mod (1)");
    const copies = screen.getAllByRole("listitem").filter((li) => li.classList.contains("copy"));
    expect(copies[0]).toHaveTextContent("FS25_Foo.zip, in your library, Vehicles, 1.0");
    expect(copies[1]).toHaveTextContent("FS25_Foo, in your mods folder");
    expect(copies[1]).toHaveTextContent("the game loads this one");
    expect(copies[0]).not.toHaveTextContent("the game loads this one");
    const keep = screen.getAllByRole("button", { name: "Keep this one" });
    await user.click(keep[0]);
    expect(onKeepCopy).toHaveBeenCalledWith(parked, [loose]);
  });

  it("Keep this one waits while another library change runs", () => {
    const a = entry("FS25_Foo", { path: "a/FS25_Foo.zip" });
    const b = entry("FS25_Foo", { path: "b/FS25_Foo.zip" });
    render(ProblemsPanel, {
      health: { missingDeps: [], corrupt: [], ignored: [], foreign: [], duplicates: [{ techName: "FS25_Foo", copies: [a, b] }] },
      onResolveForeign: vi.fn(),
      onKeepCopy: vi.fn(),
      lockReason: "Silo is moving mods.",
    });
    for (const b2 of screen.getAllByRole("button", { name: "Keep this one" })) expect(b2).toBeDisabled();
  });

  it("each group has its actions", async () => {
    const user = userEvent.setup();
    const onFindInBrowse = vi.fn();
    const onShowFile = vi.fn();
    const onPark = vi.fn();
    const onResolveForeign = vi.fn(() => Promise.resolve(true));
    render(ProblemsPanel, {
      health: {
        missingDeps: [{ mod: entry("FS25_Trailer"), missing: ["FS25_BaseLib"] }],
        corrupt: [entry("FS25_Broken", { error: "invalid zip" })],
        ignored: [entry("1FS25_Digit")],
        foreign: [
          { techName: "FS25_Plow", fileName: "FS25_Plow.zip", kind: "zip", flatVersion: "1.1.0.0", managedVersion: "1.0.0.0", archiveChanged: false },
        ],
      },
      isActive: (t: string) => t === "FS25_Broken",
      onFindInBrowse,
      onShowFile,
      onPark,
      onResolveForeign,
    });
    expect(screen.getByRole("heading", { level: 3 })).toHaveTextContent("4 mods need attention");
    expect(screen.getByRole("heading", { name: /Missing a required mod/ })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: /Can't be read/ })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: /Ignored by the game: the name starts with a number/ })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: /A different build is in your mods folder/ })).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Find it in Browse" }));
    expect(onFindInBrowse).toHaveBeenCalledWith("FS25_BaseLib");
    await user.click(screen.getAllByRole("button", { name: "Show file" })[0]);
    expect(onShowFile).toHaveBeenCalledWith("C:/lib/FS25_Broken.zip");
    await user.click(screen.getByRole("button", { name: "Park" }));
    expect(onPark).toHaveBeenCalledWith("FS25_Broken");
    await user.click(screen.getByRole("button", { name: "Use this version" }));
    expect(onResolveForeign).toHaveBeenCalledWith("FS25_Plow.zip", "adopt");
    await user.click(screen.getByRole("button", { name: "Keep Silo's copy" }));
    expect(onResolveForeign).toHaveBeenCalledWith("FS25_Plow.zip", "restore");
    // "Foreign files" is gone from the words.
    expect(screen.queryByText(/foreign/i)).toBeNull();
  });

  it("a library copy changed since it was put in the mods folder offers the library's (#431)", async () => {
    const user = userEvent.setup();
    const onResolveForeign = vi.fn(() => Promise.resolve(true));
    render(ProblemsPanel, {
      health: {
        missingDeps: [],
        corrupt: [],
        ignored: [],
        foreign: [
          { techName: "FS25_Plow", fileName: "FS25_Plow.zip", kind: "zip", flatVersion: "1.0.0.0", managedVersion: "1.1.0.0", archiveChanged: true },
        ],
      },
      onResolveForeign,
    });
    expect(screen.getByText(/The copy in your library changed \(1\.1\.0\.0\) since Silo put 1\.0\.0\.0/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Use this version" })).toBeNull();
    await user.click(screen.getByRole("button", { name: "Use the library's copy" }));
    expect(onResolveForeign).toHaveBeenCalledWith("FS25_Plow.zip", "restore");
    await user.click(screen.getByRole("button", { name: "Keep the mods-folder copy" }));
    expect(onResolveForeign).toHaveBeenCalledWith("FS25_Plow.zip", "adopt");
  });
});

function report(crashed: boolean, mods: LogReport["mods"]): LogReport {
  return { engineVersion: "1.4.0.0", modCount: 3, cleanExit: !crashed, crashed, mods, unattributed: 2, totalFindings: 9, groundPileCap: null, groundPileDisabled: [], fillTypeCap: null, droppedFillTypes: [], truncated: false, siloFolders: [] };
}

const lookup = (t: string) =>
  t === "FS25_Mulcher" ? { title: "Field Mulcher Extended", active: true } : null;

describe("Last run", () => {
  beforeEach(() => {
    vi.mocked(logStatus).mockResolvedValue({ path: "log.txt", modifiedMs: Date.now(), gameRunning: false, lastSeenMs: null });
  });

  it("a named mod has Park, Show in library and Copy details", async () => {
    const user = userEvent.setup();
    vi.mocked(scanLog).mockResolvedValue(
      report(true, [
        { modName: "FS25_Mulcher", errors: 14, warnings: 3, benign: 0, sample: "Error: nil value", sampleLine: 812 },
      ]),
    );
    const onPark = vi.fn();
    const onShow = vi.fn();
    render(LogTriage, { activeMods: ["FS25_Mulcher", "FS25_X"], activeMaps: [], onApply: vi.fn(), onBisecting: vi.fn(), lookup, onPark, onShow });
    expect(await screen.findByText("Your last game ended unexpectedly.")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Most likely causes" })).toBeInTheDocument();
    expect(screen.getByText("Field Mulcher Extended")).toBeInTheDocument();
    expect(screen.getByText("14 errors · 3 warnings")).toBeInTheDocument();
    expect(screen.getByText(/2 findings aren't tied to a mod/)).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Park" }));
    expect(onPark).toHaveBeenCalledWith("FS25_Mulcher");
    await user.click(screen.getByRole("button", { name: "Show in library" }));
    expect(onShow).toHaveBeenCalledWith("FS25_Mulcher");
    await user.click(screen.getByRole("button", { name: "Copy details" }));
    expect(copyText).toHaveBeenCalledWith(expect.stringContaining("Field Mulcher Extended (FS25_Mulcher)"));
    // The first log line expands.
    const line = screen.getByRole("button", { name: /log line for Field Mulcher Extended/ });
    expect(line).toHaveAttribute("aria-expanded", "false");
    await user.click(line);
    expect(line).toHaveAttribute("aria-expanded", "true");
  });

  it("the crash notice's Find the problem mod opens the flow, once", async () => {
    vi.mocked(scanLog).mockResolvedValue(report(true, []));
    const onBisectRequestTaken = vi.fn();
    render(LogTriage, {
      activeMods: ["A", "B", "C"],
      activeMaps: [],
      onApply: vi.fn(),
      onBisecting: vi.fn(),
      bisectRequested: true,
      onBisectRequestTaken,
    });
    expect(await screen.findByRole("heading", { name: "Find the problem mod" })).toBeInTheDocument();
    expect(onBisectRequestTaken).toHaveBeenCalledTimes(1);
  });
});

describe("Find the problem mod: mods Silo can't park yet", () => {
  beforeEach(() => {
    vi.mocked(bisectSnapshotGet).mockReset().mockResolvedValue(null);
    vi.mocked(bisectSnapshotSave).mockReset().mockResolvedValue(undefined);
    vi.mocked(bisectPlan).mockReset().mockResolvedValue({ kind: "culprit", modName: "FS25_B" });
    vi.mocked(scanLog).mockReset().mockResolvedValue(report(false, []));
    vi.mocked(logStatus).mockReset().mockResolvedValue({ path: "log.txt", modifiedMs: 1, gameRunning: false, lastSeenMs: null });
  });

  it("asks to file loose mods first; declining starts nothing and saves nothing", async () => {
    const user = userEvent.setup();
    const onPrepare = vi.fn(() => Promise.resolve(false));
    const onApply = vi.fn(() => Promise.resolve());
    render(BisectFlow, { pool: ["FS25_A", "FS25_B"], alwaysOn: [], onApply, onBisecting: vi.fn(), onExit: vi.fn(), onPrepare });
    await user.click(screen.getByRole("button", { name: "Start" }));
    await waitFor(() => expect(onPrepare).toHaveBeenCalledOnce());
    expect(bisectSnapshotSave).not.toHaveBeenCalled();
    expect(onApply).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Start" })).toBeEnabled();
  });

  it("a round testing one mod says 'Testing 1 mod'", async () => {
    const user = userEvent.setup();
    vi.mocked(bisectPlan).mockResolvedValue({ kind: "split", test: ["FS25_A"], rest: ["FS25_B", "FS25_C"], roundsLeft: 1 });
    render(BisectFlow, { pool: ["FS25_A", "FS25_B", "FS25_C"], alwaysOn: [], onApply: vi.fn(() => Promise.resolve()), onBisecting: vi.fn(), onExit: vi.fn() });
    await user.click(screen.getByRole("button", { name: "Start" }));
    expect(await screen.findByText(/this round/)).toHaveTextContent("Testing 1 mod this round (2 parked for now).");
  });

  it("the intro says only maps stay on (required mods are tested too), with a real plural", () => {
    render(BisectFlow, { pool: ["FS25_A", "FS25_B"], alwaysOn: [], onApply: vi.fn(() => Promise.resolve()), onBisecting: vi.fn(), onExit: vi.fn() });
    const intro = screen.getByText(/mods to test/);
    expect(intro).toHaveTextContent("about 1 launch. Maps stay on the whole time.");
    expect(intro.textContent).not.toMatch(/required/);
  });

  it("once they're filed, the run saves the set as it is then", async () => {
    const user = userEvent.setup();
    const onPrepare = vi.fn(() => Promise.resolve(true));
    render(BisectFlow, { pool: ["FS25_A", "FS25_B"], alwaysOn: ["MAP"], onApply: vi.fn(() => Promise.resolve()), onBisecting: vi.fn(), onExit: vi.fn(), onPrepare });
    await user.click(screen.getByRole("button", { name: "Start" }));
    expect(await screen.findByText(/Most likely:/)).toBeInTheDocument();
    expect(onPrepare).toHaveBeenCalledOnce();
    expect(bisectSnapshotSave).toHaveBeenCalledWith(["MAP", "FS25_A", "FS25_B"]);
  });

  it("unzipped folder mods stay on every round (Silo can't park them), and it says so", async () => {
    const user = userEvent.setup();
    render(LogTriage, {
      activeMods: ["MAP", "FS25_A", "FS25_B", "FS25_Folder"],
      activeMaps: ["MAP"],
      activeFixed: ["FS25_Folder"],
      onApply: vi.fn(() => Promise.resolve()),
      onBisecting: vi.fn(),
    });
    await user.click(await screen.findByRole("button", { name: "Find the problem mod" }));
    expect(screen.getByText(/1 unzipped folder mod stays on every round/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Start" }));
    await waitFor(() => expect(bisectPlan).toHaveBeenCalledWith(["FS25_A", "FS25_B"]));
    expect(bisectSnapshotSave).toHaveBeenCalledWith(["MAP", "FS25_Folder", "FS25_A", "FS25_B"]);
  });
});

describe("Find the problem mod: the finish", () => {
  it("Put my mods back and park X restores the saved set without the culprit", async () => {
    const user = userEvent.setup();
    const saved = ["MAP", "FS25_A", "FS25_Mulcher", "FS25_C"];
    let snapshot: string[] | null = null;
    vi.mocked(bisectSnapshotGet).mockImplementation(async () => snapshot);
    vi.mocked(bisectSnapshotSave).mockImplementation(async (s: string[]) => void (snapshot = s));
    vi.mocked(bisectSnapshotClear).mockImplementation(async () => void (snapshot = null));
    vi.mocked(bisectPlan).mockResolvedValue({ kind: "culprit", modName: "FS25_Mulcher" });
    const onApply = vi.fn(() => Promise.resolve());
    const onBisecting = vi.fn();
    const onFinished = vi.fn();
    render(BisectFlow, {
      pool: ["FS25_A", "FS25_Mulcher", "FS25_C"],
      alwaysOn: ["MAP"],
      onApply,
      onBisecting,
      onExit: vi.fn(),
      onFinished,
      titleOf: (t: string) => (t === "FS25_Mulcher" ? "Field Mulcher Extended" : t),
    });
    await user.click(screen.getByRole("button", { name: "Start" }));
    expect(await screen.findByText(/Most likely:/)).toHaveTextContent("Most likely: Field Mulcher Extended");
    expect(snapshot).toEqual(saved);
    await user.click(screen.getByRole("button", { name: "Put my mods back and park Field Mulcher Extended" }));
    await waitFor(() => expect(onFinished).toHaveBeenCalledWith("FS25_Mulcher"));
    expect(onApply).toHaveBeenLastCalledWith(["MAP", "FS25_A", "FS25_C"]);
    expect(snapshot).toBeNull();
    expect(onBisecting).toHaveBeenLastCalledWith(false);
  });

  it("Put my mods back restores everything", async () => {
    const user = userEvent.setup();
    let snapshot: string[] | null = null;
    vi.mocked(bisectSnapshotGet).mockImplementation(async () => snapshot);
    vi.mocked(bisectSnapshotSave).mockImplementation(async (s: string[]) => void (snapshot = s));
    vi.mocked(bisectSnapshotClear).mockImplementation(async () => void (snapshot = null));
    vi.mocked(bisectPlan).mockResolvedValue({ kind: "culprit", modName: "FS25_C" });
    const onApply = vi.fn(() => Promise.resolve());
    const onFinished = vi.fn();
    render(BisectFlow, { pool: ["FS25_A", "FS25_C"], alwaysOn: [], onApply, onBisecting: vi.fn(), onExit: vi.fn(), onFinished });
    await user.click(screen.getByRole("button", { name: "Start" }));
    await user.click(await screen.findByRole("button", { name: "Put my mods back" }));
    await waitFor(() => expect(onFinished).toHaveBeenCalledWith(null));
    expect(onApply).toHaveBeenLastCalledWith(["FS25_A", "FS25_C"]);
  });
});

describe("Find the problem mod: a run survives a hiccup", () => {
  let snapshot: string[] | null;
  const split = { kind: "split" as const, test: ["FS25_A"], rest: ["FS25_B"], roundsLeft: 1 };
  beforeEach(() => {
    snapshot = null;
    vi.mocked(bisectSnapshotGet).mockImplementation(async () => snapshot);
    vi.mocked(bisectSnapshotSave).mockImplementation(async (s: string[]) => void (snapshot = s));
    vi.mocked(bisectSnapshotClear).mockReset().mockImplementation(async () => void (snapshot = null));
    vi.mocked(bisectPlan).mockReset().mockResolvedValue(split);
    vi.mocked(bisectNarrow).mockReset().mockResolvedValue(["FS25_A"]);
    vi.mocked(scanLog).mockReset().mockResolvedValue({ crashed: true } as never);
    vi.mocked(logStatus).mockReset().mockResolvedValue({ path: "log.txt", modifiedMs: 1, gameRunning: false, lastSeenMs: null });
    vi.mocked(launchGame).mockReset().mockResolvedValue(undefined as never);
  });
  const start = async (onApply = vi.fn(() => Promise.resolve()), onBisecting = vi.fn()) => {
    const user = userEvent.setup();
    render(BisectFlow, { pool: ["FS25_A", "FS25_B"], alwaysOn: [], onApply, onBisecting, onExit: vi.fn() });
    await user.click(screen.getByRole("button", { name: "Start" }));
    await screen.findByRole("button", { name: "I've quit the game" });
    return { user, onApply, onBisecting };
  };

  it("I've quit the game while FS25 still runs: no crash guess from a log still being written", async () => {
    vi.mocked(logStatus).mockResolvedValue({ path: "log.txt", modifiedMs: 1, gameRunning: true, lastSeenMs: null });
    const { user } = await start();
    await user.click(screen.getByRole("button", { name: "I've quit the game" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(/still running/);
    expect(scanLog).not.toHaveBeenCalled();
    expect(screen.queryByText(/crashed again/)).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "I've quit the game" })).toBeInTheDocument();
  });

  it("a failed launch keeps the round: the user can still come back and answer", async () => {
    vi.mocked(launchGame).mockRejectedValue(new Error("Steam isn't running."));
    const { user } = await start();
    await user.click(screen.getByRole("button", { name: /Launch test round/ }));
    expect(await screen.findByRole("alert")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "I've quit the game" }));
    expect(await screen.findByRole("button", { name: "It still happened" })).toBeInTheDocument();
  });

  it("a round that couldn't be applied offers Try again instead of ending the run", async () => {
    let fail = false;
    const onApply = vi.fn(async () => {
      if (fail) {
        fail = false;
        throw "FS25_A: in use";
      }
    });
    const { user } = await start(onApply);
    await user.click(screen.getByRole("button", { name: "I've quit the game" }));
    fail = true;
    await user.click(await screen.findByRole("button", { name: "It still happened" }));
    await user.click(await screen.findByRole("button", { name: "Try again" }));
    expect(await screen.findByText(/Round 2 of about/)).toBeInTheDocument();
    expect(bisectNarrow).toHaveBeenCalledOnce();
  });

  it("the ✕ asks first once a round was applied; No keeps the run", async () => {
    const onApply = vi.fn(() => Promise.resolve());
    const { user } = await start(onApply);
    ui.ask.mockResolvedValueOnce(false);
    await user.click(screen.getByRole("button", { name: "Stop and put my mods back" }));
    expect(ui.ask).toHaveBeenCalledWith(expect.objectContaining({ title: "Stop finding the problem mod?", confirm: "Stop" }));
    expect(onApply).toHaveBeenCalledTimes(1); // the round only: nothing was restored
    expect(screen.getByRole("button", { name: "I've quit the game" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Stop and put my mods back" }));
    await waitFor(() => expect(onApply).toHaveBeenCalledTimes(2));
    expect(onApply).toHaveBeenLastCalledWith(["FS25_A", "FS25_B"]);
  });

  it("when putting the mods back fails, Close and restore later stops the run and keeps the saved set", async () => {
    let restoring = false;
    const onApply = vi.fn(async () => {
      if (restoring) throw "FS25_B: the archive drive isn't connected";
    });
    const onExit = vi.fn();
    const onBisecting = vi.fn();
    const user = userEvent.setup();
    render(BisectFlow, { pool: ["FS25_A", "FS25_B"], alwaysOn: [], onApply, onBisecting, onExit });
    await user.click(screen.getByRole("button", { name: "Start" }));
    await screen.findByRole("button", { name: "I've quit the game" });
    restoring = true;
    await user.click(screen.getByRole("button", { name: "Stop and put my mods back" }));
    await user.click(await screen.findByRole("button", { name: "Close and restore later" }));
    expect(onBisecting).toHaveBeenLastCalledWith(false);
    expect(onExit).toHaveBeenCalled();
    expect(bisectSnapshotClear).not.toHaveBeenCalled();
    expect(snapshot).toEqual(["FS25_A", "FS25_B"]);
  });
});

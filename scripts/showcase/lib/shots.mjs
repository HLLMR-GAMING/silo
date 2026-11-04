// The showcase shot list: how to put the app in each state, and each shot's alt text. Ids match
// the site's shot list (`<id>@2x.png`). `prime` runs first, every time: it finishes setup on a
// freshly staged profile and returns the app to a clean Library otherwise.
//   for (const id of ORDER) await SHOTS[id].run(driver);
import fs from "node:fs";
import path from "node:path";
import { sleep } from "./cdp.mjs";

const titleOf = (d, tech) => d.lib.mods.find((m) => m.techName === tech).title;

/** Back to a calm Library: no drawer, no dialog, no search. */
async function toLibrary(d) {
  await d.page.eval(`document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true })); true`);
  await d.go("Library");
  await d.search("");
}

/** Scroll every list back to its top (opening the drawer can leave a half-cut row). */
async function scrollTop(d) {
  await d.page.eval(`[...document.querySelectorAll("main *, aside *")].forEach((e) => { if (e.scrollTop > 0) e.scrollTop = 0; }); true`);
  await sleep(400);
}

/** Open a Settings section. */
async function settingsTab(d, name) {
  await toLibrary(d);
  await d.go("Settings");
  await d.click(name, { sel: "[role=tab]" });
  await sleep(500);
}

/** Close the mod drawer if it's open. */
async function closeDrawer(d) {
  await d.helpers();
  await d.page.eval(`(() => { const b = document.querySelector('aside [aria-label="Close"], [role=complementary] [aria-label="Close"], [aria-label="Close details"]'); b?.click(); return true; })()`);
  await sleep(400);
}

/** Switch a mod on or off from its Library row, and wait until the switch has really flipped
 *  (a click while Silo is still writing the library is turned away). */
async function toggleMod(d, title) {
  await toLibrary(d);
  await d.search(title);
  const sel = JSON.stringify(`button[role=switch][aria-label="Active: ${title}"]`);
  await d.page.waitFor(`!!document.querySelector(${sel})`, 10_000, `switch for ${title}`);
  const was = await d.page.eval(`document.querySelector(${sel}).getAttribute("aria-checked")`);
  for (let i = 0; i < 5; i++) {
    await d.page.eval(`document.querySelector(${sel}).click(); true`);
    try {
      await d.page.waitFor(`document.querySelector(${sel})?.getAttribute("aria-checked") !== ${JSON.stringify(was)}`, 4000);
      break;
    } catch {
      if (i === 4) throw new Error(`couldn't switch ${title}`);
    }
  }
  await sleep(1500);
  await d.search("");
}

export const SHOTS = {
  prime: {
    async run(d) {
      await d.helpers();
      const onboarding = await d.page.eval(`document.body.innerText.includes("Silo found your mods folder")`);
      if (onboarding) {
        console.log("first run: onboarding, organizing and mod sets…");
        await d.click("This is right");
        await d.click("Continue");
        await d.click("Organize", { timeout: 30_000 });
        await d.waitText("Your library is organized", 180_000);
        await d.click("Go to my library");
        const active = d.lib.mods.filter((m) => m.active).map((m) => m.techName);
        await d.invoke("set_active", { root: null, active });
        await d.reload();
        await d.waitText("Ready to play", 60_000);
        // The first set through the UI, so it's the active one; the others straight in.
        await d.go("Mod sets");
        await d.click("Save current mods as a set");
        await d.page.eval(`__sc.fill('[role=dialog] input', ${JSON.stringify(d.lib.sets[0].name)})`);
        await d.click("Save", { within: "[role=dialog]", sel: "button" });
        await d.waitText("Active");
        for (const s of d.lib.sets.slice(1)) await d.invoke("save_loadout", { id: null, name: s.name, mods: s.mods });
        await d.reload();
      }
      await toLibrary(d);
      await d.waitText("Ready to play", 60_000);
    },
  },

  library: {
    alt: "Silo's Library: a list of about 700 Farming Simulator 25 mods with icons, authors, versions and categories, each switched Active or Parked. The status line reads Ready to play, 189 of 693 active, Harvest 2026.",
    async run(d) {
      await toLibrary(d);
      await d.waitText("Ready to play");
      await d.shot("library");
    },
  },

  "verified-build": {
    alt: "The mod drawer for a tractor mod, with a Verified build badge: its files match the build Silo's catalog hashed from the mod's source.",
    async run(d) {
      const title = titleOf(d, d.lib.roles.verified);
      await toLibrary(d);
      await d.search(title.split(" ")[1]); // "7R": a few tractors, the verified one among them
      await d.openMod(title);
      await d.click("Verify integrity");
      await d.page.waitFor(`!!document.querySelector(".prov .verdict.ok")`, 20_000, "Verified build verdict");
      await scrollTop(d);
      await d.shot("verified-build");
      await closeDrawer(d);
    },
  },

  "modified-build": {
    alt: "The mod drawer for a baler mod flagged Modified: Silo lists the files that differ from the published build.",
    async run(d) {
      const title = titleOf(d, d.lib.roles.modified);
      await toLibrary(d);
      await d.search("Baler");
      await d.openMod(title);
      await d.click("Verify integrity");
      await d.page.waitFor(`!!document.querySelector(".prov .verdict.warn")`, 20_000, "Modified build verdict");
      // Show the changed files themselves.
      await d.page.eval(`[...document.querySelectorAll("summary")].filter((e) => /^(Changed|Added)/.test(e.textContent.trim())).forEach((e) => e.click()); true`);
      await scrollTop(d);
      await d.shot("modified-build");
      await closeDrawer(d);
    },
  },

  "mod-sets": {
    alt: "My sets: three saved mod sets with their mod counts and how each differs from the mods active now. Harvest 2026 is marked Active; the others offer Switch to this set.",
    async run(d) {
      await toLibrary(d);
      await d.go("Mod sets");
      await d.click("My sets", { sel: "[role=tab]" });
      await d.waitText("Switch to this set");
      await d.shot("mod-sets");
    },
  },

  "share-set": {
    alt: "The Share dialog for the Harvest 2026 mod set: a share link has just been created for its 189 mods, with a Copy link button.",
    async run(d) {
      await toLibrary(d);
      await d.go("Mod sets");
      await d.click("My sets", { sel: "[role=tab]" });
      await d.click(`More for ${d.lib.sets[0].name}`, { sel: "button" });
      await d.click("Share…", { sel: "[role=menuitem]" });
      await d.click("Create link", { within: "[role=dialog]", sel: "button" });
      await d.waitText("Link created");
      await d.shot("share-set");
      await d.click("Close", { within: "[role=dialog]", sel: "button" });
    },
  },

  updates: {
    alt: "Updates after a check: mods ready to install from GitHub with an Update all button, and newer ModHub versions to download from their pages.",
    async run(d) {
      await toLibrary(d);
      await d.go("Updates");
      await d.helpers();
      if (await d.page.eval(`__sc.has("Only when I ask")`)) await d.click("Only when I ask");
      await d.click("Check now", { sel: "button" });
      await d.waitText("Ready to install");
      await d.shot("updates");
    },
  },

  browse: {
    alt: "Browse: a grid of catalog mods with their ModHub and GitHub sources spelled out, ratings and downloads, and whether each is in your library, has an update, or can be installed.",
    async run(d) {
      await toLibrary(d);
      await d.go("Browse");
      await d.waitText("Showing");
      await d.page.waitFor(`(() => { const imgs = [...document.querySelectorAll("main img")].filter((i) => { const r = i.getBoundingClientRect(); return r.width > 0 && r.top < innerHeight && r.bottom > 0; }); return imgs.length > 0 && imgs.every((i) => i.complete && i.naturalWidth > 0); })()`, 15_000, "card images");
      await d.shot("browse");
    },
  },

  // ── Help (same kit) ──
  "help-mods-folder": {
    alt: "Settings, Mods folder: the folder Silo manages and Farming Simulator 25 loads mods from, with Open folder and Change, and where the game is installed.",
    async run(d) {
      await settingsTab(d, "Mods folder");
      await d.shot("help-mods-folder");
    },
  },

  "help-organize-preview": {
    alt: "The Organize dialog before anything moves: three new mods and the library category folder each will be filed into.",
    async run(d) {
      await toLibrary(d);
      const src = path.join(d.profile, "fixtures", "new-downloads");
      const mods = path.join(d.profile, d.lib.userDir, "mods");
      const files = fs.readdirSync(src);
      for (const f of files) fs.copyFileSync(path.join(src, f), path.join(mods, f));
      await d.reload();
      await d.waitText("new mods aren't filed", 30_000);
      await d.click("Review", { sel: "button" });
      await d.waitText("Organize", 10_000);
      await d.page.eval(`[...document.querySelectorAll("[role=dialog] summary, [role=dialog] button")].find((e) => /^See every file/.test(e.textContent.trim()))?.click(); true`);
      await sleep(400);
      await d.shot("help-organize-preview");
      await d.page.eval(`document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true })); true`);
      await sleep(400);
      // Take the downloads back out, so later shots show the library as it was.
      for (const f of files) fs.rmSync(path.join(mods, f), { force: true });
      await d.reload();
      await d.waitGone("new mods aren't filed", 30_000);
    },
  },

  "help-undo-organizing": {
    alt: "Settings, Maintenance: the Undo organizing confirmation, which puts your mods back in the mods folder and deletes nothing.",
    async run(d) {
      await settingsTab(d, "Maintenance");
      await d.click("Undo organizing", { sel: "main button" });
      await d.waitText("Put your mods back in your mods folder?");
      await d.shot("help-undo-organizing");
      // A danger confirm is an alertdialog.
      await d.click("Cancel", { within: "[role=alertdialog]", sel: "button" });
    },
  },

  "help-savegames": {
    alt: "Mod sets, Savegames: each savegame with its map, when it was last played, whether its mods are all in the library, and Use this save's mods.",
    async run(d) {
      await toLibrary(d);
      await d.go("Mod sets");
      await d.click("Savegames", { sel: "[role=tab], button" });
      await d.waitText("Use this save's mods");
      await d.shot("help-savegames");
    },
  },

  "help-mp-check-file": {
    alt: "The Share dialog with Multiplayer check file chosen: a file with each mod's exact fingerprint, so friends can check they match a server before joining.",
    async run(d) {
      await toLibrary(d);
      await d.go("Mod sets");
      await d.click("My sets", { sel: "[role=tab]" });
      await d.click(`More for ${d.lib.sets[0].name}`, { sel: "button" });
      await d.click("Share…", { sel: "[role=menuitem]" });
      await d.click("Multiplayer check file", { within: "[role=dialog]", sel: "label, [role=radio], input" });
      await d.shot("help-mp-check-file");
      await d.click("Close", { within: "[role=dialog]", sel: "button" });
    },
  },

  "help-filltype-bridge": {
    alt: "Tools, Fill type bridge: Silo builds a small companion mod that adds a map's fill type to the categories your trailers, silos and sell points accept.",
    async run(d) {
      await toLibrary(d);
      await d.go("Tools");
      await d.click("Fill type bridge", { sel: "[role=tab], button" });
      await d.waitText("Fill type compatibility bridge");
      await d.shot("help-filltype-bridge");
    },
  },

  "help-bindings": {
    alt: "Tools, Controls map: every key, mouse and gamepad binding from the game's control settings, grouped by device, with inputs used by more than one action highlighted.",
    async run(d) {
      await toLibrary(d);
      await d.go("Tools");
      await d.click("Controls map", { sel: "[role=tab], button" });
      await d.waitText("KEY", 10_000).catch(() => {});
      await sleep(600);
      await d.shot("help-bindings");
    },
  },

  "help-accounts": {
    alt: "Settings, Accounts: a connected GitHub account with star, watch and sharing links turned on, and a Disconnect button.",
    async run(d) {
      await settingsTab(d, "Accounts");
      await d.waitText("Connected as");
      await d.shot("help-accounts");
    },
  },

  "help-copy-diagnostics": {
    alt: "Settings, Diagnostics: Copy diagnostics and Export report give a summary of the library, conflicts and Silo's recent log, with the user name removed.",
    async run(d) {
      await settingsTab(d, "Diagnostics");
      await d.waitText("Copy diagnostics");
      await d.shot("help-copy-diagnostics");
    },
  },

  conflicts: {
    alt: "Diagnose, Conflicts: one problem. Two mods claim the same exclusive slot, so the game would load only one of them; a Park one button fixes it.",
    async run(d) {
      const title = titleOf(d, d.lib.roles.conflictB);
      await toggleMod(d, title);
      await d.go("Diagnose");
      await d.click("Conflicts", { sel: "[role=tab], button" });
      await d.waitText("exclusive slot");
      await d.shot("conflicts");
      await toggleMod(d, title);
      await d.waitText("Ready to play");
    },
  },

  "crash-notice": {
    alt: "Back from a crashed game, Silo's notice says the last game ended unexpectedly and names the most likely mod, with a Park button for it.",
    async run(d) {
      await toLibrary(d);
      // The game "crashed": its log stops mid-run with the culprit's Lua errors.
      const log = path.join(d.profile, d.lib.userDir, "log.txt");
      fs.copyFileSync(path.join(d.profile, "fixtures", "log-crash.txt"), log);
      const now = new Date();
      fs.utimesSync(log, now, now);
      await d.reload();
      await d.waitText("Your last game ended unexpectedly", 30_000);
      await toLibrary(d);
      await d.shot("crash-notice");
    },
  },

  "diagnose-last-run": {
    alt: "Diagnose, Last run: the crashed run's mods ranked by the errors they logged, the likely culprit first, each with Park and Show in library.",
    async run(d) {
      await d.click("See details", { sel: "button" });
      await d.waitText("Most likely causes", 15_000).catch(() => d.waitText("MOST LIKELY CAUSES"));
      await d.shot("diagnose-last-run");
    },
  },

  "find-problem-mod": {
    alt: "Find the problem mod, mid-run: round 1 of about 8, testing half of the active mods while the other half are parked, with steps to launch the game and report back.",
    async run(d) {
      await d.go("Diagnose");
      await d.click("Last run", { sel: "[role=tab], button" });
      await d.click("Find the problem mod");
      await d.click("Start", { sel: "button" });
      await d.waitText("Round 1 of about");
      await sleep(2500);
      await d.shot("find-problem-mod");
    },
  },
};

/** The Home shots the site asked for first. */
export const HOME = ["library", "crash-notice", "diagnose-last-run", "find-problem-mod", "conflicts", "verified-build", "modified-build", "mod-sets", "share-set", "updates", "browse"];

/** Help shots (the Library archive move in progress isn't staged: it needs a slow live move). */
export const HELP = ["help-mods-folder", "help-organize-preview", "help-undo-organizing", "help-savegames", "help-mp-check-file", "help-filltype-bridge", "help-bindings", "help-accounts", "help-copy-diagnostics"];

/** Capture order: the conflict, crash and bisection shots last, since they change the library's state. */
export const ORDER = ["library", "verified-build", "modified-build", "mod-sets", "share-set", "updates", "browse", ...HELP, "conflicts", "crash-notice", "diagnose-last-run", "find-problem-mod"];

# Release-candidate testing protocol — hostile QA

**Read this before `SMOKE_TEST.md`.** `SMOKE_TEST.md` is the feature checklist (does each
thing work). This doc is the *gate*: how to test the RC so we ship an artifact that was
actually tested, and never lose a user's files. Cowork's job here is **hostile QA engineer,
not enthusiastic intern** — the goal is to break it on purpose, not to turn the checklist
green.

## The prime directive

> The release gate is **not** "the UI said success."
> The release gate is: **every byte on disk is present, absent, or changed for an explainable reason.**

Filesystem integrity is the supreme court. A cosmetic alignment glitch: log it and ship. Library
mutilation: the launch is cancelled.

## The one rule that makes the whole thing valid: freeze the crime scene

**Do NOT test, patch, and declare victory in one continuous run.** That produces a
Ship-of-Theseus build nobody actually tested. Any code change — even a "tiny harmless fix" —
kills the tested artifact. Tiny harmless fixes are where gremlins breed.

The loop is:

1. Commit everything; record the exact **SHA**.
2. Build the packaged RC (installer, not `tauri:dev`).
3. **Record the identity of the artifact — the right way.** Hash the **installer** (`*-setup.exe`
   / `*.msi`) — that's the distributable's fingerprint. Do **NOT** compare the installed
   `Silo.exe` against the raw `target/release/Silo.exe`: the NSIS bundler post-processes the exe,
   so the *installed* binary's hash legitimately differs from the raw cargo output. To confirm the
   *installed* app matches the RC, verify the **installer hash** and grep the installed binary for
   an **RC-specific code string** (e.g. a message added in the RC commit) — a hash-vs-raw-exe check
   will false-alarm.
4. Cowork gets **audit-only authority** for the first pass — find and *report*, never fix.
5. Collect every failure with **screenshots, logs, reproduction steps, and affected files.**
6. Fixes happen in a **separate pass** (dev, not Cowork mid-run).
7. Build a **new RC**, re-hash, and **rerun the critical battery** from scratch.

If a fix lands, the previously-tested hashes are dead. Everything Phase 2+ must run against the
new artifact.

---

## Phase 0 — freeze + forensic baseline

Record, for the RC:
- Git SHA, `installer` SHA-256, `Silo.exe` SHA-256.
- The environment (Windows build, user type, elevation state).

Then, on the **sacrificial cloned** FS25 library (never the real one for destructive tests),
capture a baseline you can diff against after every destructive workflow:

```powershell
# BEFORE any Silo action. Run again as after.csv later, then Compare-Object.
$root = "C:\...\sacrificial\mods"        # the cloned mods folder
$out  = "C:\...\rc-baseline"
New-Item -ItemType Directory -Force $out | Out-Null
Get-ChildItem -Recurse -File $root | ForEach-Object {
  [pscustomobject]@{
    Rel  = $_.FullName.Substring($root.Length).TrimStart('\')
    Size = $_.Length
    Sha  = (Get-FileHash $_.FullName -Algorithm SHA256).Hash
  }
} | Sort-Object Rel | Export-Csv "$out\baseline.csv" -NoTypeInformation
"files=$((Get-ChildItem -Recurse -File $root).Count) dirs=$((Get-ChildItem -Recurse -Directory $root).Count)" | Tee-Object "$out\counts.txt"
# Also snapshot: the active-set list, mods/archive/ contents, savegame + config file hashes,
# and a copy of the Silo SQLite DB from %APPDATA%\com.hllmr.silo (or app_data_dir)\silo.db
```

Compare after each destructive op:

```powershell
Get-ChildItem -Recurse -File $root | ForEach-Object { [pscustomobject]@{
  Rel=$_.FullName.Substring($root.Length).TrimStart('\'); Size=$_.Length;
  Sha=(Get-FileHash $_.FullName -Algorithm SHA256).Hash } } |
  Sort-Object Rel | Export-Csv "$out\after.csv" -NoTypeInformation
Compare-Object (Import-Csv "$out\baseline.csv") (Import-Csv "$out\after.csv") -Property Rel,Size,Sha
```

Every line of diff must be explainable (this mod moved into `archive/`, this projection link
appeared/vanished, this `.bak` was written). An **unexplained** deletion or change is a **hard
stop**.

---

## Phase 1 — test the PACKAGED binary (not dev)

`tauri:dev` is for *diagnosing* a failure only. The gate is the installed app across this matrix:

- [ ] Clean packaged install (NSIS `.exe` and MSI).
- [ ] **Standard Windows user, no elevation.** (Installer is code-signed — confirm the publisher
      reads "David Hellmer," not "Unknown publisher"; SmartScreen may still prompt on a fresh build.)
- [ ] Fresh app-data state (no prior `silo.db`).
- [ ] **Existing app-data state from the previous release** (upgrade — see Phase 2F).
- [ ] Real FS25 library (read-only workflows only).
- [ ] **Sacrificial cloned** FS25 library (all destructive workflows).
- [ ] Paths containing **spaces and non-ASCII** characters (e.g. `C:\Über Mods (test)\`).

Run the full `SMOKE_TEST.md` feature battery on the packaged build — every panel, drawer,
toggle, modal, with the console open, watching for uncaught exceptions. Spend disproportionate
effort on **state transitions and hostile conditions** (below), not clicking each button once.

---

## Phase 2 — the abuse batteries (the point of this whole exercise)

### A. Kill it mid-operation (crash recovery)

Force-close Silo (End Task) at **different stages** of each op, relaunch, verify recovery:
Organize · Undo organizing · Activate · Park · Mod set switch · Mod update · Update all ·
Direct install · Find the problem mod · Library archive move · Savegame backup · Config save.

The organizer has explicit protection for interrupted operations (manifest-first, archive-must-
exist-before-removal, `.part`/`.bak`/temp handling). Prove it with real filesystem state. After
each: run the forensic diff. **Recovery must always reach a safe state; the only copy is never
lost.**

### B. Manufacture filesystem failures

Deliberately induce each, then verify the invariants:
- Destination read-only · source read-only · file locked by another process · archive folder
  locked · disk nearly full · backup destination unwritable · existing `.bak` · existing `.part`
  · existing destination filename · **source deleted between preview and confirm** · AV-style
  transient lock · cross-volume / network drive where supported.

Every failure must leave: **original usable · no truncated destination · no orphaned manifest
row · no unrecognized file silently deleted · a useful error message** saying what happened.

### C. Abuse the download paths

Feed both the updater and the direct catalog installer:
- Empty response · HTML renamed `.zip` · valid ZIP with **no `modDesc.xml`** · truncated ZIP ·
  corrupt central directory · oversized · very slow · connection loss mid-download · redirect
  chain · server 404 / 429 / 500 · **valid FS25 ZIP that is the WRONG mod** · already-installed
  version · locally-modified build.

Direct catalog installs now validate the full archive + require a root `modDesc.xml`, **and**
identity-check the bytes: when the catalog has a canonical hash for the mod (GitHub-source), a
download whose bytes don't match is refused. Verify: **the `.part` temp disappears after every
failure**, **no bad archive ever enters the library**, and the **wrong-valid-mod** case is
**rejected** for a hashed (GitHub) mod (swap the asset → "doesn't match the catalog's known
build"). For an *unhashed* source (ModHub/not-yet-hashed) identity can't be proven — only
archive validity — so confirm that path degrades to the validity check, not a false rejection.

### D. User-ownership conflicts — the most important regression

1. Organize a mod → deactivate it (parked).
2. Drop a **different** ZIP at the same filename in the flat `mods/` folder.
3. Activate → park → Undo organizing → Re-read every mod → switch mod sets.
4. **Silo must refuse to delete the user replacement** (reports "isn't the one Silo linked" or
   "isn't Silo's copy of this mod").

Repeat across every identity branch the code checks:
- Same size, different bytes · exact byte-identical copy · hardlink · symlink/junction · folder
  mod · copied directory **with** and **without** the `.silo-projection` marker.

Forensic-diff after each. Expected per branch:
- **Different bytes** (same size or not), a folder without the marker, a link that resolves
  somewhere else: the user's file must survive every step.
- **Exact byte-identical copy**, a hardlink of the archived file, a link that resolves to it, a
  copied directory **with** the marker whose files haven't changed: Silo treats these as its own
  projection and may remove them when parking, because the archive holds the same bytes. That is by design: copy-mode
  projections (another drive, exFAT, cloud-synced folders) are exactly such copies. Pass if the
  archived copy is still there, byte-identical, afterwards. Nothing may be removed when the
  archived copy is missing or differs (#399).

### E. Degraded network

- No internet at startup · SiloAPI unreachable · SiloAPI returns malformed JSON · catalog image
  host down · GitHub down · requests time out · rapid repeated searches · app closed
  mid-request.

The **library and diagnostics must stay usable.** Network failure degrades *features*, never
destabilizes the app or corrupts state.

### F. Fresh install AND upgrade/migration

Do **not** test only a clean profile — early adopters are the ones upgrading.
- Clean install of the RC · **in-app update from the previous release** (its updater, then
  Restart to update) · the previous release's SQLite DB (a schema bump keeps
  `db-snapshots/silo-before-v<N>.db` and passes `PRAGMA integrity_check`) · existing organized
  library · existing mod sets + curation · existing `.bak` / `.part` / archive /
  `.silo-projection` files · uninstall→reinstall **without deleting user data** · manual
  app-data wipe then first launch. For 1.0.0 specifically: from 0.11.0, DB `user_version` 1 →
  2, every mod set kept as a user set, *What's new in Silo 1.0* shown once, and the first-run
  setup never shown to the upgraded library.

Migration bugs are first-impression assassins.

---

## Hard stop conditions — fail the RC immediately

Any one of these **cancels the release** (no "known issue, probably fine" for data safety,
credentials, migration, install/update, or crash recovery):

- Missing mod or save file.
- Original overwritten without a **verified** backup.
- A **user-owned** file deleted.
- A corrupt/bad download admitted to the library.
- Crash recovery can't determine a safe state.
- Credentials appear in logs or `silo.db`.
- Update/install writes **outside** authorized roots.
- Packaged build behaves differently from dev.
- The previous release's data becomes unreadable.
- A critical workflow reports **success despite partial failure**.
- A reproducible uncaught frontend exception.
- Any CI security or test gate fails.

Cosmetic glitch → log and ship. Library mutilation → launch cancelled.

## What Cowork reports (audit-only, pass 1)

For each finding: title · severity (hard-stop vs cosmetic) · exact steps to reproduce · the
packaged-build SHA it was found on · screenshots · relevant `log.txt` / console output · the
files/paths affected · the forensic diff if state changed. **No silent repairs.** Hand the list
back; fixes are a separate pass that produces a new RC.

---

## Release mechanics — only after the RC survives the abuse

On a `release/X.Y.Z` branch, bump **all** of these together so they never drift:
- `package.json` · `package-lock.json` (both `silo` entries)
- `src-tauri/Cargo.toml` · the `silo` entry in `src-tauri/Cargo.lock`
- `src-tauri/tauri.conf.json`
- `CHANGELOG.md`: `[Unreleased]` → `[X.Y.Z] - <date>`, then start a fresh empty `[Unreleased]`.

Then, against the **exact release commit**, all must be green (cargo commands from
`src-tauri/`; `cargo audit` also runs in CI):

```bash
npm ci
npm test
npm run build
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo audit
npm run tauri:build
```

Open the PR, merge it on green CI, then tag `vX.Y.Z` on that `main` commit. The tagged release
workflow does **not** re-run the test/audit battery; instead its `guard` job refuses the release
unless the tagged commit is on `main` and that commit's CI checks all passed, so **wait for main's
CI to finish green before tagging** (if the guard fails because CI was still running, re-run it
once CI is green). The build then pauses until the owner **approves the `release` environment**
in the Actions tab; that environment holds the signing keys. It builds and signs the platform
targets into a **draft, non-prerelease** release. Publishing the draft makes it *Latest*, and the in-app updater offers it
to every user straight away, so finish every check below first.

Before publishing the draft:
- [ ] Download **every** generated artifact.
- [ ] Verify filenames and the **embedded version** (X.Y.Z everywhere, including `latest.json`).
- [ ] Install the Windows artifact on a clean machine/VM; launch; scan a small fixture library;
      verify uninstall leaves user data intact.
- [ ] `SHA256SUMS.txt` lists every asset (including `latest.json` and `silo-sbom.cdx.json`) and
      matches the downloads; `gh attestation verify <file> --repo HLLMR-VENTURES/silo` passes.
- [ ] Release notes state: **Windows code-signed / macOS-Linux OS-unsigned, known
      limitations, upgrade guidance.**
- [ ] After publishing: the website's download links resolve to **vX.Y.Z**, and
      `releases/latest/download/latest.json` serves X.Y.Z.

**Bottom line:** freeze the RC, test the packaged artifact, preserve evidence, forbid silent
repairs, and make filesystem integrity the supreme court. If it survives the abuse above, ship
it.

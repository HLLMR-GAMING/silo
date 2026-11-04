# Release validation records

Per-release evidence that the **packaged** artifact was validated, per `RC_TESTING.md`. Each
entry records the exact artifact, environment, what was tested, and the outcome — closing the
gap between the RC protocol and visible proof it ran.

---

## v1.0.5 — 2026-11-04 (database schema v6, Remove from library)

Validated by hand (not an HLLMRD run). The release candidate was first tested on the owner's PC; that test found the silent permanent delete on a drive without a Recycle Bin (#442), fixed in #443 before tagging. Published 2026-11-04 12:45:16Z on the owner's instruction.

### Artifact identity
| Field | Value |
|---|---|
| Version | 1.0.5 |
| Source | Tag `v1.0.5` (annotated, pushed by the owner as HLLMR) on `00d79bc`, the merge of #445; main's CI green on it |
| Release run | [41826956347](https://github.com/HLLMR-VENTURES/silo/actions/runs/41826956347): guard passed, owner approved the `release` environment, all 4 builds + sbom + checksums green |
| Release | draft id 433881698, prerelease=false, published as Latest; `/releases/latest` → v1.0.5 |
| Assets | 19 (as 1.0.4) |
| Installers | `SHA256SUMS.txt` SHA-256 `226fc139b5d5ff54b3542e0ef852080c95c892faeba0d07f1a165fa4a3fed981`; `Silo_1.0.5_x64-setup.exe` SHA-256 `4c207f425cbf2c81761fbaffe877721a81deec6f2db124809194d3e843a7ba6b`; `latest.json` SHA-256 `a4affd0e35dca9b5b64fde757696ff006f6d56b19945d09723a02f58e62266a4` |

### Checks
| Check | Result |
|---|---|
| macOS signing and notarization | both mac legs: "Set up macOS signing and notarization" and "Verify macOS signature and notarization" passed |
| Windows Authenticode | `Get-AuthenticodeSignature` on the setup `.exe` and on the installed `silo.exe`: Valid, `CN=David Hellmer, O=David Hellmer, L=Grand Prairie, S=tx, C=US`, Microsoft timestamp |
| Updater manifest | `latest.json` version 1.0.5; 11/11 platform entries. Every bundle's minisign signature verified against the `pubkey` in `tauri.conf.json` (key id match, file and trusted-comment signatures): NSIS, MSI, AppImage, deb, rpm, both `.app.tar.gz`. After publish, both `HLLMR-VENTURES/silo` and the old `HLLMR/silo` `releases/latest/download/latest.json` serve 1.0.5 |
| Checksums | every downloadable bundle (setup `.exe`, `.msi`, both `.dmg`, AppImage, deb, rpm, both `.app.tar.gz`) matches `SHA256SUMS.txt` |
| Provenance | `gh attestation verify --repo HLLMR-VENTURES/silo --format json` on the setup `.exe`: 2 attestations, source `refs/tags/v1.0.5` at `00d79bc1dd614281e633691b851e4af02a660eb6`, signer `release.yml@refs/tags/v1.0.5` |
| Upgrade over 1.0.4 | silent per-user install (`/S`, exit 0) over 1.0.4 on the owner's PC; uninstall entry and `silo.exe` report 1.0.5; launched, logged "Silo 1.0.5 starting on windows" and "kept a copy before upgrading: …\db-snapshots\silo-before-v5.db", closed cleanly (exit 0); `mods/` size+mtime baseline (761 entries): 0 differences; `gameSettings.xml` and `game.xml` hashes unchanged; `silo://` handler registry export identical; DB `integrity_check` ok, **`user_version` 6**, new `foreign_mark` table, every other table's row count unchanged (739 cached mods, 736 organized); `silo-before-v5.db` written. DB copy kept outside the repo before upgrading |
| RC test (before tagging) | on a copy of the owner's real DB: migration to v6, integrity ok. In an isolated build: Keep this one, Remove from library (active and several at once), Enter = Cancel, Undo organizing. On a drive without a Recycle Bin (`subst`): Windows asked "permanently delete?" over Silo; No kept the mod with a notice, Yes deleted it. The owner's real mods, settings, DB and handler unchanged throughout |
| Not done | clean install; in-app update 1.0.4 → 1.0.5 not yet observed; macOS and Linux installs on real machines by us (#417 asks for testers) |

---

## v1.0.3 — 2026-10-29 (database schema v3)

Validated by hand (not an HLLMRD run). Published 2026-10-29 12:19:08Z on the owner's instruction.

### Artifact identity
| Field | Value |
|---|---|
| Version | 1.0.3 |
| Source | Tag `v1.0.3` (annotated, pushed by the owner as HLLMR) on `d831a59`, the merge of #336; main's CI green on it |
| Release run | [40869473660](https://github.com/HLLMR-VENTURES/silo/actions/runs/40869473660): guard passed, owner approved the `release` environment, all 4 builds + sbom + checksums green |
| Release | draft id 427816742, prerelease=false, published as Latest; `/releases/latest` → v1.0.3 |
| Assets | 19 (as 1.0.2) |
| Installers | `SHA256SUMS.txt` SHA-256 `6af02270b76a23d2e4c3848447a65e9750fe20d0e6ed502f852fad26104414d3`; `Silo_1.0.3_x64-setup.exe` SHA-256 `d4769d13823908768b5fbb4bf53cc2526ed67f8e6f5991009da3898452ea5910`; `latest.json` SHA-256 `2bb4b68953ec8d2a64b2f4cc90f1894417b5e899b032d74323fed56b209cf2cf` |

### Checks
| Check | Result |
|---|---|
| macOS signing (both mac legs) | `Authority=Developer ID Application: David Hellmer (554574H76C)`; the release step "Verify macOS signature and notarization" passed |
| macOS notarization | submissions `3b731640-7222-466c-86be-6f1fc55e4144` and `d10a50a7-acb3-4839-86ec-e2800a10c5c1`: **Accepted**; `spctl` accepted, source=Notarized Developer ID; ticket stapled |
| Windows Authenticode | `Get-AuthenticodeSignature` on the setup `.exe`: Valid, `CN=David Hellmer, O=David Hellmer, L=Grand Prairie, S=tx, C=US` |
| Updater manifest | `latest.json` version 1.0.3; 11/11 platform entries with signatures; URLs under `HLLMR-VENTURES/silo/releases/download/v1.0.3/`. After publish, both `HLLMR-VENTURES/silo` and the old `HLLMR/silo` `releases/latest/download/latest.json` serve 1.0.3 |
| Checksums | the setup `.exe`'s SHA-256 is in `SHA256SUMS.txt` |
| Provenance | `gh attestation verify --owner HLLMR-VENTURES` on the setup `.exe`: verified, signer `HLLMR-VENTURES/silo/.github/workflows/release.yml@refs/tags/v1.0.3` |
| Upgrade over 1.0.2 | silent per-user install (`/S`, exit 0) over 1.0.2 on the owner's PC; launched, logged "Silo 1.0.3 starting on windows" and "kept a copy before upgrading: …\db-snapshots\silo-before-v2.db", closed cleanly; `mods/` size+mtime baseline (743 files, 761 entries): 0 differences; DB `integrity_check` ok, **`user_version` 3**, `curation.skip_updates` present, curation rows kept; `silo-before-v2.db` is `user_version` 2 with integrity ok. DB copy kept outside the repo before upgrading |
| Not done | clean install; in-app update 1.0.2 → 1.0.3 (updater plugin 2.14) not yet observed; macOS install on a real Mac by us (the #322 tester confirmed 1.0.2 opens on Apple Silicon; asked to report the 1.0.3 self-update) |

## v1.0.2 — 2026-10-27 (first Developer ID signed and notarized macOS build)

Validated by hand (not an HLLMRD run). Published 2026-10-27 18:23:57Z on the owner's instruction.

### Artifact identity
| Field | Value |
|---|---|
| Version | 1.0.2 |
| Source | Tag `v1.0.2` (annotated, pushed by the owner as HLLMR) on `9c690d1`, the merge of #328; main's CI green on it |
| Release run | [40538456651](https://github.com/HLLMR-VENTURES/silo/actions/runs/40538456651): guard passed, owner approved the `release` environment, all 4 builds + sbom + checksums green |
| Release | draft id 425780605, prerelease=false, published as Latest; `/releases/latest` → v1.0.2 |
| Assets | 19 (as 1.0.1) |
| Installers | `SHA256SUMS.txt` SHA-256 `556d84f322eeb8d7c1450e1c06dfac9c835868267f61c85b3c799dd54d7a609c`; `Silo_1.0.2_x64-setup.exe` SHA-256 `eaf756a1e980a975b68bfed647f1ea1e39246019708b7a830a631cb51fea7389`; `latest.json` SHA-256 `c33db56f236708dd82c9399f639869c093bc6c71be39b6ff4c6012c89b8d8332` |

### Checks
| Check | Result |
|---|---|
| macOS signing (both mac legs) | `codesign --verify --deep --strict`: valid on disk, satisfies its Designated Requirement; `Authority=Developer ID Application: David Hellmer (554574H76C)` → Developer ID Certification Authority → Apple Root CA |
| macOS notarization | aarch64 submission `8591b2f3-f3c0-46c8-bd0a-a46d49965a05` and x86_64 `399105b0-cf16-4d64-b0e3-e27cf3e16f8f`: **Accepted**; `spctl` source=Notarized Developer ID; ticket stapled (the release step "Verify macOS signature and notarization" passed) |
| Windows Authenticode | `Get-AuthenticodeSignature` on the setup `.exe`: Valid, `CN=David Hellmer, O=David Hellmer, L=Grand Prairie, S=tx, C=US`, timestamped |
| Updater manifest | `latest.json` version 1.0.2; 11/11 platform entries with signatures; URLs under `HLLMR-VENTURES/silo/releases/download/v1.0.2/`. After publish, both `HLLMR-VENTURES/silo` and the old `HLLMR/silo` `releases/latest/download/latest.json` serve 1.0.2 |
| Checksums | the setup `.exe`'s SHA-256 is in `SHA256SUMS.txt` |
| Provenance | `gh attestation verify --owner HLLMR-VENTURES` on the setup `.exe`: verified, signer `HLLMR-VENTURES/silo/.github/workflows/release.yml@refs/tags/v1.0.2` |
| Upgrade over 1.0.1 | silent per-user install (`/S`, exit 0) over 1.0.1 on the owner's PC; launched, logged "Silo 1.0.2 starting on windows", closed cleanly; `mods/` size+mtime baseline of 743 files: 0 differences; DB `integrity_check` ok, `user_version` 2. DB snapshot kept outside the repo before upgrading |
| Not done | clean install; macOS install on a real Mac (no Mac available; #171 stays open for a Mac tester) |

## v1.0.1 — 2026-10-20 (first HLLMRD-run release)

> Generated by `hllmrd release-check` 0.3.0 (generated_by: **local**, stage: published, online) on Windows-11-10.0.26200-SP0.
> Result: **FAILED**: 9 pass, 1 fail, 0 skipped, 0 n/a, 0 error.
> A local run is not release evidence (review §7.3). The same facts are in `report.json`.

### Artifact identity
| Field | Value |
|---|---|
| Version | 1.0.1 |
| Source | Tag `v1.0.1` on `c319c7a`; c319c7a is on main and .github/workflows/ci.yml is green there (PASS: U-TAG-ON-MAIN) |
| Release | name 'Silo v1.0.1', draft=False, prerelease=False (PASS: T-DRAFT-FLAGS) |
| Assets | all 19 expected assets present, none extra (PASS: T-ASSETS-PRESENT) |
| Installers | `SHA256SUMS.txt` SHA-256 `60625e2765a960ed7e5e8647125ce2c15d1f36ac670b92ae76130f5cb7fb6939`; `Silo_1.0.1_x64-setup.exe` SHA-256 `e34ba4f38c20d88f8c3bbdfb50c17bff80da985c0e2ff707c985d98ea1c36cf5`; `Silo_1.0.1_x64_en-US.msi` SHA-256 `1781d45a2ffab7b6b8b19c79111c3cd0aa2bc5973c667dbe5b8dda8eee4468ee`; `latest.json` SHA-256 `c5f4d252514decb03b7c67b2fda1ba9b4e54a323618c4386a73226ec24cf7115` |
| Checksums | SHA256SUMS.txt covers all 18 other assets (PASS: T-CHECKSUMS-COVER-ALL) |
| Checksum match | all 18 listed checksums match (PASS: T-CHECKSUMS-MATCH) |
| Updater manifest | version 1.0.1; 11/11 platform keys present with minisign-form signatures equal to their .sig assets (not cryptographically verified); URLs under https://github.com/HLLMR/silo/releases/download/v1.0.1/ (PASS: T-UPDATER-MANIFEST) |
| Provenance | attestation verifies for 4 assets (refs/tags/v1.0.1) (PASS: T-ATTESTATION-VERIFIES) |
| Authenticode | Authenticode not valid for Silo_1.0.1_x64-setup.exe, Silo_1.0.1_x64_en-US.msi (**FAIL**: T-AUTHENTICODE) |

### Checks
| Check | Status | Result |
|---|---|---|
| `U-TAG-ON-MAIN` v1 | PASS | c319c7a is on main and .github/workflows/ci.yml is green there |
| `U-REVIEWS-GO` v1 | PASS | GO-IF: all 20 conditions hold at c319c7a |
| `T-ASSETS-PRESENT` v1 | PASS | all 19 expected assets present, none extra |
| `T-CHECKSUMS-COVER-ALL` v1 | PASS | SHA256SUMS.txt covers all 18 other assets |
| `T-CHECKSUMS-MATCH` v1 | PASS | all 18 listed checksums match |
| `T-UPDATER-MANIFEST` v1 | PASS | version 1.0.1; 11/11 platform keys present with minisign-form signatures equal to their .sig assets (not cryptographically verified); URLs under https://github.com/HLLMR/silo/releases/download/v1.0.1/ |
| `T-UPDATER-SIGNATURES` v1 | PASS | 11/11 entries verified (7 bundles): key E878CC86A773BF25, algorithm ED; pinned key matches git show c319c7a:src-tauri/tauri.conf.json |
| `T-ATTESTATION-VERIFIES` v1 | PASS | attestation verifies for 4 assets (refs/tags/v1.0.1) |
| `T-AUTHENTICODE` v1 | **FAIL** | Authenticode not valid for Silo_1.0.1_x64-setup.exe, Silo_1.0.1_x64_en-US.msi |
| `T-DRAFT-FLAGS` v1 | PASS | draft=False, prerelease=False, name and tag as expected |

### Review passes
Rollup of `_release-checks/v1.0.1`: **GO-IF**. Totals: BLOCKER 0, SHOULD-FIX 0, NIT 2, POST-1.0 1.

| Pass | Base | BLOCKER | SHOULD-FIX | NIT | POST-1.0 | Decision |
|---|---|---|---|---|---|---|
| coordinator-smoke | `c319c7a` | 0 | 0 | 0 | 0 | GO-IF |
| copy-polish | `c319c7a` | 0 | 0 | 1 | 0 | GO |
| frontend-a | `c319c7a` | 0 | 0 | 0 | 0 | GO |
| frontend-b | `c319c7a` | 0 | 0 | 0 | 0 | GO |
| release-eng | `c319c7a` | 0 | 0 | 1 | 1 | GO-IF |
| rust-backend | `c319c7a` | 0 | 0 | 0 | 0 | GO |
| security-api | `c319c7a` | 0 | 0 | 0 | 0 | GO-IF |

Conditions at the tag (PASS: U-REVIEWS-GO): GO-IF: all 20 conditions hold at c319c7a

| Condition | Ref | Scope | State |
|---|---|---|---|
| forbids | #301 | security-api | met |
| forbids | #309 | security-api | met |
| forbids | #232 | security-api | met |
| base in release | c319c7ab832d | coordinator-smoke | met |
| base in release | c319c7ab832d | copy-polish | met |
| base in release | c319c7ab832d | frontend-a | met |
| base in release | c319c7ab832d | frontend-b | met |
| base in release | c319c7ab832d | release-eng | met |
| base in release | c319c7ab832d | rust-backend | met |
| base in release | c319c7ab832d | security-api | met |
| manual | Install the v1.0.1 draft's setup.exe over the installed 1.0.0 on the owner's machine, launch it, confirm Settings › About shows "Silo 1.0.1" and the library opens, and confirm a 0-line mods/ diff before Publish | coordinator-smoke | attested |
| manual | Push tag v1.0.1 on c319c7ab832d16dd8b6bc96f32bde389db0edb33 (main tip, CI run 39441389551 green) and confirm the guard job prints six "success" lines and passes | release-eng | attested |
| manual | Approve the `release` environment for the Release run's build jobs | release-eng | attested |
| manual | Confirm the run is fully green and the draft holds all 19 expected assets named for 1.0.1, with SHA256SUMS.txt listing the other 18 | release-eng | attested |
| manual | Before publishing, verify the latest.json/.sig updater signatures against the pinned key E878CC86A773BF25 (release-check), plus attestations and Authenticode CN=David Hellmer on both Windows installers | release-eng | attested |
| manual | Clean install of Silo_1.0.1_x64-setup.exe on Windows | release-eng | attested |
| manual | Upgrade an existing 1.0.0 install to 1.0.1 (non-destructive, About shows 1.0.1, WebView2 autofill popup gone) | release-eng | attested |
| manual | Write the release body for v1.0.1 | release-eng | attested |
| manual | Publish the draft | release-eng | attested |
| manual | Confirm https://github.com/HLLMR/silo/releases/latest/download/latest.json serves version 1.0.1 | release-eng | attested |

Approval for the attested Manual conditions:
> Approved: 2026-10-20 by owner. Scope: Silo v1.0.1 release-check Manual steps (clean install waived by the owner for 1.0.1) and Publish. Via: Claude Code desktop chat with the Silo dev session, "Approved: I confirm the v1.0.1 manual steps as reported (tag and guard, release approval, green run and 19 assets, signatures, upgrade over 1.0.0, release body); skip the clean install for 1.0.1. Publish v1.0.1 and finish the release check.".

### Evidence

**U-TAG-ON-MAIN** (PASS): Tag commit is on main, with CI green there
- v1.0.1 -> c319c7ab832d16dd8b6bc96f32bde389db0edb33 (local tag refs/tags/v1.0.1)
- GitHub: v1.0.1 -> c319c7ab832d16dd8b6bc96f32bde389db0edb33
- local: git merge-base --is-ancestor c319c7a refs/remotes/origin/main -> yes (refs/remotes/origin/main at c319c7a; the clone is not fetched)
- GitHub: compare main...c319c7a -> identical (ahead 0, behind 0)
- CI: .github/workflows/ci.yml run 39441389551 on main: completed/success

**U-REVIEWS-GO** (PASS): Review rollup is GO, and its GO-IF conditions hold at the tag
- rollup of _release-checks/v1.0.1: GO-IF (7 passes)
- forbids #301 (security-api): met; PR #301 is open, not merged
- forbids #309 (security-api): met; PR #309 is open, not merged
- forbids #232 (security-api): met; PR #232 is closed, not merged
- base in release c319c7ab832d (coordinator-smoke): met; git merge-base --is-ancestor c319c7a c319c7a
- base in release c319c7ab832d (copy-polish): met; git merge-base --is-ancestor c319c7a c319c7a
- base in release c319c7ab832d (frontend-a): met; git merge-base --is-ancestor c319c7a c319c7a
- base in release c319c7ab832d (frontend-b): met; git merge-base --is-ancestor c319c7a c319c7a
- base in release c319c7ab832d (release-eng): met; git merge-base --is-ancestor c319c7a c319c7a
- base in release c319c7ab832d (rust-backend): met; git merge-base --is-ancestor c319c7a c319c7a
- base in release c319c7ab832d (security-api): met; git merge-base --is-ancestor c319c7a c319c7a
- manual Install the v1.0.1 draft's setup.exe over the installed 1.0.0 on the owner's machine, launch it, confirm Settings › About shows "Silo 1.0.1" and the library opens, and confirm a 0-line mods/ diff before Publish (coordinator-smoke): attested; --attest, under approval: Approved: 2026-10-20 by owner. Scope: Silo v1.0.1 release-check Manual steps (clean install waived by the owner for 1.0.1) and Publish. Via: Claude Code desktop chat with the Silo dev session, "Approved: I confirm the v1.0.1 manual steps as reported (tag and guard, release approval, green run and 19 assets, signatures, upgrade over 1.0.0, release body); skip the clean install for 1.0.1. Publish v1.0.1 and finish the release check.".
- manual Push tag v1.0.1 on c319c7ab832d16dd8b6bc96f32bde389db0edb33 (main tip, CI run 39441389551 green) and confirm the guard job prints six "success" lines and passes (release-eng): attested; --attest, under approval: Approved: 2026-10-20 by owner. Scope: Silo v1.0.1 release-check Manual steps (clean install waived by the owner for 1.0.1) and Publish. Via: Claude Code desktop chat with the Silo dev session, "Approved: I confirm the v1.0.1 manual steps as reported (tag and guard, release approval, green run and 19 assets, signatures, upgrade over 1.0.0, release body); skip the clean install for 1.0.1. Publish v1.0.1 and finish the release check.".
- manual Approve the `release` environment for the Release run's build jobs (release-eng): attested; --attest, under approval: Approved: 2026-10-20 by owner. Scope: Silo v1.0.1 release-check Manual steps (clean install waived by the owner for 1.0.1) and Publish. Via: Claude Code desktop chat with the Silo dev session, "Approved: I confirm the v1.0.1 manual steps as reported (tag and guard, release approval, green run and 19 assets, signatures, upgrade over 1.0.0, release body); skip the clean install for 1.0.1. Publish v1.0.1 and finish the release check.".
- manual Confirm the run is fully green and the draft holds all 19 expected assets named for 1.0.1, with SHA256SUMS.txt listing the other 18 (release-eng): attested; --attest, under approval: Approved: 2026-10-20 by owner. Scope: Silo v1.0.1 release-check Manual steps (clean install waived by the owner for 1.0.1) and Publish. Via: Claude Code desktop chat with the Silo dev session, "Approved: I confirm the v1.0.1 manual steps as reported (tag and guard, release approval, green run and 19 assets, signatures, upgrade over 1.0.0, release body); skip the clean install for 1.0.1. Publish v1.0.1 and finish the release check.".
- manual Before publishing, verify the latest.json/.sig updater signatures against the pinned key E878CC86A773BF25 (release-check), plus attestations and Authenticode CN=David Hellmer on both Windows installers (release-eng): attested; --attest, under approval: Approved: 2026-10-20 by owner. Scope: Silo v1.0.1 release-check Manual steps (clean install waived by the owner for 1.0.1) and Publish. Via: Claude Code desktop chat with the Silo dev session, "Approved: I confirm the v1.0.1 manual steps as reported (tag and guard, release approval, green run and 19 assets, signatures, upgrade over 1.0.0, release body); skip the clean install for 1.0.1. Publish v1.0.1 and finish the release check.".
- manual Clean install of Silo_1.0.1_x64-setup.exe on Windows (release-eng): attested; --attest, under approval: Approved: 2026-10-20 by owner. Scope: Silo v1.0.1 release-check Manual steps (clean install waived by the owner for 1.0.1) and Publish. Via: Claude Code desktop chat with the Silo dev session, "Approved: I confirm the v1.0.1 manual steps as reported (tag and guard, release approval, green run and 19 assets, signatures, upgrade over 1.0.0, release body); skip the clean install for 1.0.1. Publish v1.0.1 and finish the release check.".
- manual Upgrade an existing 1.0.0 install to 1.0.1 (non-destructive, About shows 1.0.1, WebView2 autofill popup gone) (release-eng): attested; --attest, under approval: Approved: 2026-10-20 by owner. Scope: Silo v1.0.1 release-check Manual steps (clean install waived by the owner for 1.0.1) and Publish. Via: Claude Code desktop chat with the Silo dev session, "Approved: I confirm the v1.0.1 manual steps as reported (tag and guard, release approval, green run and 19 assets, signatures, upgrade over 1.0.0, release body); skip the clean install for 1.0.1. Publish v1.0.1 and finish the release check.".
- manual Write the release body for v1.0.1 (release-eng): attested; --attest, under approval: Approved: 2026-10-20 by owner. Scope: Silo v1.0.1 release-check Manual steps (clean install waived by the owner for 1.0.1) and Publish. Via: Claude Code desktop chat with the Silo dev session, "Approved: I confirm the v1.0.1 manual steps as reported (tag and guard, release approval, green run and 19 assets, signatures, upgrade over 1.0.0, release body); skip the clean install for 1.0.1. Publish v1.0.1 and finish the release check.".
- manual Publish the draft (release-eng): attested; --attest, under approval: Approved: 2026-10-20 by owner. Scope: Silo v1.0.1 release-check Manual steps (clean install waived by the owner for 1.0.1) and Publish. Via: Claude Code desktop chat with the Silo dev session, "Approved: I confirm the v1.0.1 manual steps as reported (tag and guard, release approval, green run and 19 assets, signatures, upgrade over 1.0.0, release body); skip the clean install for 1.0.1. Publish v1.0.1 and finish the release check.".
- manual Confirm https://github.com/HLLMR/silo/releases/latest/download/latest.json serves version 1.0.1 (release-eng): attested; --attest, under approval: Approved: 2026-10-20 by owner. Scope: Silo v1.0.1 release-check Manual steps (clean install waived by the owner for 1.0.1) and Publish. Via: Claude Code desktop chat with the Silo dev session, "Approved: I confirm the v1.0.1 manual steps as reported (tag and guard, release approval, green run and 19 assets, signatures, upgrade over 1.0.0, release body); skip the clean install for 1.0.1. Publish v1.0.1 and finish the release check.".

**T-ASSETS-PRESENT** (PASS): Release assets are exactly the expected set
- 19 assets from release-metadata; 19 expected by the manifest

**T-CHECKSUMS-COVER-ALL** (PASS): The checksums file lists every other asset, including SBOM and updater manifest
- SHA256SUMS.txt lists 18 names; 18 other assets need an entry

**T-CHECKSUMS-MATCH** (PASS): Every checksum matches the asset's bytes or GitHub's digest
- 8 matched by hashing local bytes
- 18 matched GitHub's asset digest (from github)

**T-UPDATER-MANIFEST** (PASS): Updater manifest: version, platform keys, URLs and signatures
- latest.json: version 1.0.1, 11 platform keys (11 expected)
- 11 signatures equal their uploaded .sig asset (sha256)
- signatures are checked for presence, minisign form and version, not cryptographically

**T-UPDATER-SIGNATURES** (PASS): Updater signatures verify cryptographically against the pinned key
- pinned key: minisign key ID E878CC86A773BF25
- git show c319c7a:src-tauri/tauri.conf.json: holds the pinned key
- 11/11 entries verified over 7 bundles (7 bundles hashed with BLAKE2b-512)
- signed under another name (manifest signed_names): Silo_aarch64.app.tar.gz -> Silo.app.tar.gz
- signed under another name (manifest signed_names): Silo_x64.app.tar.gz -> Silo.app.tar.gz

**T-ATTESTATION-VERIFIES** (PASS): GitHub build-provenance attestation verifies
- gh attestation verify SHA256SUMS.txt --repo HLLMR/silo --source-ref refs/tags/v1.0.1 -> verified; signer https://github.com/HLLMR/silo/.github/workflows/release.yml@refs/tags/v1.0.1, run https://github.com/HLLMR/silo/actions/runs/39443737520/attempts/1
- gh attestation verify latest.json --repo HLLMR/silo --source-ref refs/tags/v1.0.1 -> verified; signer https://github.com/HLLMR/silo/.github/workflows/release.yml@refs/tags/v1.0.1, run https://github.com/HLLMR/silo/actions/runs/39443737520/attempts/1
- gh attestation verify Silo_1.0.1_x64-setup.exe --repo HLLMR/silo --source-ref refs/tags/v1.0.1 -> verified; signer https://github.com/HLLMR/silo/.github/workflows/release.yml@refs/tags/v1.0.1, run https://github.com/HLLMR/silo/actions/runs/39443737520/attempts/1
- gh attestation verify Silo_1.0.1_x64_en-US.msi --repo HLLMR/silo --source-ref refs/tags/v1.0.1 -> verified; signer https://github.com/HLLMR/silo/.github/workflows/release.yml@refs/tags/v1.0.1, run https://github.com/HLLMR/silo/actions/runs/39443737520/attempts/1

**T-AUTHENTICODE** (**FAIL**): Windows installers carry a valid, timestamped Authenticode signature
- Silo_1.0.1_x64-setup.exe: , signer None, timestamped False, file version 1.0.1
- Silo_1.0.1_x64_en-US.msi: , signer None, timestamped False, file version None

**T-DRAFT-FLAGS** (PASS): Release flags: draft for the stage, prerelease as the manifest says, tag and name
- isDraft=False, isPrerelease=False, name='Silo v1.0.1', tag=v1.0.1 (from github)

### Not covered
- Post-tag checklist sections C-E (clean install, upgrade from the previous version, after-publish updater checks) are manual and not covered.
- Checklist step 1 (release workflow run green) is not checked; U-TAG-ON-MAIN checks CI on main only.
- The release body text is not checked.
- A report generated locally is not release evidence (review §7.3); report attestation is out of scope.
- Updater signatures were verified cryptographically (T-UPDATER-SIGNATURES): key ID E878CC86A773BF25, algorithm ED, 11/11 entries (7 bundles). That does not prove the private key is uncompromised, and it says nothing about files the updater does not use (such as macOS .dmg).
- acceptance_assets_changed is not collected by release-check (null, not empty).

### Notes from the coordinator
- **T-AUTHENTICODE is a false FAIL** (HLLMRD 0.3.0 on Windows). `Get-AuthenticodeSignature` on the same downloaded installers reports `Valid`, signer `CN=David Hellmer`, timestamped, "Signature verified." for both `Silo_1.0.1_x64-setup.exe` and `Silo_1.0.1_x64_en-US.msi`, and the release workflow's own "Verify Windows signatures" step passed. Reported to HLLMRD.
- **Upgrade 1.0.0 → 1.0.1 on the owner's machine:** silent per-user `setup.exe /S` exit 0; one "Silo" entry (1.0.1); launches to the real library (739 mods, 3 active); Settings › About "Silo 1.0.1"; the WebView2 "Saved info" popup (#303) no longer appears in the real profile; `mods/` size+mtime diff over 761 entries: **0 lines**; `integrity_check` ok, `user_version` 2.
- **Clean install:** not run; waived by the owner for 1.0.1 (no clean machine available).
- **Release guard:** first real success path, run 39443737520 (tag pushed by the agent on the owner's instruction; the owner approved the `release` environment). Published as Latest 2026-10-20T16:07:30Z; `/releases/latest/download/latest.json` serves 1.0.1.

---
## v1.0.0 — 2026-10-19

### Artifact identity
| Field | Value |
|---|---|
| Version | 1.0.0 (exe FileVersion + ProductVersion + Apps & features `DisplayVersion` agree; Settings › About "Silo 1.0.0") |
| Source | Tag `v1.0.0` on `cc12c1b` (main CI green); release-workflow run 39267015886, all 6 jobs green (macOS arm64 + x64, Linux, Windows signed, SBOM, checksums) |
| Installers | `Silo_1.0.0_x64-setup.exe` SHA-256 `9c2b4bcc7c2a54c544f49499dcea90f4cf7e075392780ea5053c09bcd6ffa3c0`; `Silo_1.0.0_x64_en-US.msi` SHA-256 `0fa3af1b3e13c35b95e5f9d50a6d2b96cd34f3726d2b5acf7c95724a30916334` (both match `SHA256SUMS.txt`) |
| Authenticode | **Valid**, signer `CN=David Hellmer`, timestamped, on both installers |
| Provenance | `gh attestation verify` passes for setup.exe and msi (workflow ref `refs/tags/v1.0.0`) |
| Checksums | `SHA256SUMS.txt` lists all 18 other assets, **including `silo-sbom.cdx.json` and `latest.json`** (v0.11.0's list missed the SBOM; fixed by running `checksums` after `sbom`) |
| Updater manifest | `latest.json` v1.0.0, 11 platform keys each with a signature + a `/download/v1.0.0/` URL; `releases/latest/download/latest.json` serves 1.0.0 after publish |
| Install path | Silent per-user NSIS (`/S`) **over an existing 0.11.0** install at `%LOCALAPPDATA%\silo`; one "Silo" entry in Apps & features afterwards |
| Environment | Windows 11 Pro 26200, standard user, no elevation; real library of **739 mods** (736 organized, 3 active including a dev folder mod), cloud-synced Documents |

### Live tests
- **Upgrade 0.11.0 → 1.0.0 is non-destructive: ✅ PASS.** A size+mtime listing of the whole `mods/` tree (761 entries) before installing vs. after installing, launching, and closing 1.0.0: **0-line diff**. `integrity_check: ok`, `user_version` 2.
- **The release candidate on main, in the real app (dev build), overnight 2026-10-18/19: ✅ PASS after fixes.** A top-down click-through on the real library and on a throwaway demo profile (`SILO_PROFILE_DIR`, first run, organize, mod sets, bulk park + Undo, set switch, Find the problem mod, share `.silo`/`.silomp`, Undo organizing, settings editors). It found 4 blockers and ~30 smaller issues, all fixed before tagging (#268-#300; see Findings). Re-run on main after the fixes; records in the release-check reports.
- **Find the problem mod with unfiled mods: ✅ PASS** (demo profile, 693 loose mods): files them first on consent (Cancel moves nothing), parks the right half on disk (348 of 693 active), the Library shows the round's real set, Stop asks first and restores all 693.
- **Settings editors: ✅ PASS.** A number field (Max shadow lights 10 → 6) saves, with backups; Performance class shows the game's own `Medium`.
- **#266 on real data: ✅ PASS.** "Realistic Soil & Fertilizer" shows in full after the one-time re-parse (0.11 showed "Realistic Soil").

### Findings (all fixed before the tag)
- **Find the problem mod never parked unfiled (loose) mods**, so a newly added mod (the likely culprit) could never be found (#291).
- **Number fields in the settings editors couldn't be saved** (a JS number sent to a Rust `String`) (#278).
- **An integrity verdict, a GitHub update check or a typed note could land on the next mod clicked** (#276, #297).
- **XML text was cut at the first entity** (`Elm &amp; Ash` → "Elm") in modDesc, savegame, store and config reads (#266, #268).
- Plus: a move fallback that could leave two copies of an in-use mod (#279), a pasted GitHub token that could reach the log (#281), unbounded shared sets (#277), the 500 MB install cap (#282), false type/specialization conflicts (#275), the release body's "Beta" and the SBOM missing from the checksums (#269), and many UI and copy fixes.

### Not run live
- The in-app update offer from 0.11.0 (it reads `/releases/latest`, live only after publish).
- The 0.11 → 1.0 database migration on a v1 database: this machine's database was already migrated by the dev builds (with `db-snapshots/silo-before-v1.db` kept); the migration is covered by `db.rs` tests (`migrating_to_v2_keeps_every_set_as_a_user_set`, concurrent first opens).
- macOS and Linux packages: built and signed by CI, not run here (#171).

### Sign-off
- Artifact identity, signing, provenance, upgrade integrity and the release's headline flows: **PASS**. Published as Latest 2026-10-19T14:31:10Z.

---

## v0.8.0 — 2026-10-16

### Artifact identity
| Field | Value |
|---|---|
| Version | 0.8.0 (exe FileVersion + ProductVersion + registry `DisplayVersion` agree) |
| Source | GitHub release `v0.8.0` (non-prerelease "Latest"), release-workflow run 38976382678, all 6 jobs green (macOS arm64 + x64, Linux, Windows signed, SBOM, checksums) |
| Installer | `Silo_0.8.0_x64-setup.exe`, SHA-256 `3aa868c89d25172746b6fb7825d0454516e90cfc9965806ea46c3a3bb053fed4` (matches `SHA256SUMS.txt`) |
| Authenticode | **Valid**, signer `CN=David Hellmer`, Microsoft RSA timestamp; installed `silo.exe` also Valid |
| Updater manifest | `latest.json` v0.8.0, 11 platform keys each with a signature + v0.8.0 URL; Windows key layout identical to v0.7.3 (`windows-x86_64` → MSI, `-nsis` → setup.exe); `releases/latest/download/latest.json` serves 0.8.0 after publish |
| Install path | Silent per-user NSIS (`/S`) **over an existing 0.7.3** install at `%LOCALAPPDATA%\Silo` |
| Environment | Windows 11 Pro 26200, standard user, no elevation; real library of **737 mods** (736 organized, 1 dev folder mod active), cloud-synced Documents (copy projections) |

### Live tests
- **Upgrade 0.7.3 → 0.8.0 is non-destructive: ✅ PASS.** A size+mtime listing of the whole `mods/` tree (741 files) before installing vs. after installing, launching, and closing 0.8.0: **0-line diff**.
- **DB migration (organize/projection scoped per folder): ✅ PASS.** The first 0.8.0 launch rebuilt `organized`/`projection` with `(root, …)` primary keys and claimed all **736** rows for the default folder's key; `integrity_check: ok`; `mod_cache` 737. The library rendered as before (same active set, no banners). `silo.db` was backed up before the first migrating launch.
- **Mods-folder switch (#162) on a debug build of the release code: ✅ PASS** (operator-driven). Change… → empty scratch folder → "Only in Silo" → mismatch banner → "Use the game's folder" restored the library; `gameSettings.xml` MD5 unchanged (`dc8225f8…`).

### Findings
- **#177 (high, fixed in this release):** Cancel was ignored on every confirmation dialog since 2026-07-14. `tauri-plugin-dialog` makes `window.confirm` async, and the bare `if (!confirm(...))` guards got a truthy Promise. Found during the #162 test, when Cancel still rewrote `gameSettings.xml`; restored byte-for-byte from the `.bak`. Fixed in #178 with an awaited `confirmDialog()` and a build guard (`scripts/check-confirm.mjs`).

### Not run live
- The RC_TESTING Phase 2 abuse batteries (kill mid-op, manufactured FS failures) on a sacrificial library copy. Organize/flatten crash-safety is covered by its regression tests; the new per-folder isolation has a unit test (`records_from_one_folder_never_authorize_changes_in_another`).
- macOS and Linux packages: built and signed by CI, not run here (#171; Linux detection is with the #166 reporter).

### Sign-off
- Upgrade integrity, migration and the release's headline flow: **PASS**. Published as Latest.

---

## v0.2.4 — 2026-08-01

### Artifact identity
| Field | Value |
|---|---|
| Version | 0.2.4 (exe version resource + registry `DisplayVersion` agree) |
| Source | GitHub release `v0.2.4` (non-prerelease "Latest"), signed; `latest.json` resolves at the updater endpoint |
| Install path | reached via **in-app auto-update** from 0.2.3 (per-user NSIS at `%LOCALAPPDATA%\Silo`) |
| Environment | Windows 11 Pro 26100, standard user; real library of **728 mods** (organized) |

### Live tests — operator-driven (GUI + kills by owner, forensics scripted)

- **Auto-update 0.2.3 → 0.2.4 — ✅ PASS.** In-app "Update to 0.2.4" downloaded the signed
  release, verified the minisign signature, installed, and relaunched as 0.2.4 (confirmed by
  version resource + registry). Validates the whole updater pipeline end-to-end in the wild.
- **App-update is non-destructive — ✅ PASS.** Forensic diff of the entire `mods/` tree before
  vs. after the update: **byte-identical** (728 files unchanged); flat root unchanged; DB
  `integrity_check: ok`, `mod_cache` = 728. Updating the app touches no mod files.
- **User-ownership / projection guard (RC §D, the crown jewel) — ✅ PASS.** With a mod set
  Active, a *different* file was placed at that mod's managed name in the flat `mods/` root.
  On **launch** (when Silo projects the active set), Silo **did not overwrite or delete** the
  foreign file — it survived byte-for-byte (sha unchanged), and the archived original was
  intact. Whole-tree diff showed no unexplained deletion/change. Library restored to baseline
  after (byte-identical). Confirms the supreme invariant: *Silo never removes a file it can't
  prove it created*, even when that file occupies a projection target.

### Findings
- **#34 (low / enhancement) — silent conflict handling.** Silo preserves a foreign/mismatched
  file at a managed name (data-safe) but doesn't *flag* it in the library view or *report* that
  it skipped the projection. Visibility gap, not a data-safety bug. Tracked.

### Not run live (covered elsewhere)
- **Bad-download interception (RC §C).** Injecting a wrong/corrupt asset mid-download needs a
  network proxy — impractical through the real GUI. Covered by the identity-guard unit tests
  (#30: wrong-mod / non-mod / corrupt all refused) and the catalog installer's hash check.
- **Kill-mid-operation crash recovery (RC §A).** The organizer's interrupted-state safety is
  covered by its regression tests; a live kill-mid-Organize pass is deferred.

### Sign-off
- Hard-stop-class invariants exercised live (auto-update integrity, non-destructive update,
  ownership/projection guard): **PASS**. One low-severity visibility finding (#34).
- Verdict: **suitable for the controlled public beta.** Run the deferred live kill-mid-op pass
  before a broad promotional push if desired.

---

## v0.2.2 — 2026-08-01

### Artifact identity
| Field | Value |
|---|---|
| Version | 0.2.2 (FileVersion + ProductVersion + registry `DisplayVersion` all agree) |
| Source | GitHub release `v0.2.2` (non-prerelease "Latest"), signed; 9 platform assets + `latest.json` |
| Installed binary SHA-256 | `9aaebbdccb7ee7f34379e999fc34cfeeaf5c382ddb3aab40cf45693cc4f7e7ac` (`%LOCALAPPDATA%\Silo\Silo.exe`, 21,170,688 bytes) |
| Install type | Per-user NSIS → `%LOCALAPPDATA%\Silo` |
| Updater manifest | `releases/latest/download/latest.json` reachable (HTTP 200); version 0.2.2; a signature + correct download URL per platform |

> Note: identity confirmed via the Windows version resource + registry, **not** a naïve binary
> string grep — a `grep 0.2.x` on the exe returns a dependency's `0.2.5`, the exact false
> positive `RC_TESTING.md` warns about.

### Environment
- Windows 11 Pro 26100, standard user, no elevation.
- Real library: **728 mods**, organized layout (`mods/archive/<Category>/`), active set projected.

### Static filesystem-integrity audit — ✅ PASS
The "supreme court" check (`RC_TESTING.md`: every byte present, absent, or changed for an
explainable reason), run against the real 728-mod library after the full 0.1.0 → 0.2.2 cycle:

- **728 / 728** archives present; **0** zero-byte; **0** central-directory failures (all 728);
  full-CRC sample (~20 spread across the set) **0** failures.
- **DB `PRAGMA integrity_check: ok`** (snapshot copy).
- **DB ↔ disk consistent:** `mod_cache` = **728** rows == 728 disk archives (no drift).
- Projection in the flat `mods/` root resolves (the user's dev-mod symlink → target OK).
- No orphaned files, no losses, no unexplained deletions.

**Verdict:** no data-loss path exercised in normal steady state; the reversible-projection
model held across a multi-version upgrade on a real library.

### Migration
- DB carries state accumulated across 0.1.0 → 0.2.2; `integrity_check` clean; scan cache
  consistent with disk. Credential migration (token → keychain, DB scrub + VACUUM) was
  validated during the 0.2.0 RC battery; not re-exercised here.

### Observation to investigate (not a hard stop)
- `organized` table has **0** rows while 728 mods are physically organized under `archive/`.
  Every file is present (no loss), but confirm whether Silo sources "Restore vanilla"
  reversibility from disk structure vs. this table, and whether an empty table degrades any
  restore path. Track as a follow-up.

### Pending — interactive Phase 2 battery (requires an operator at the GUI)
Not yet executed against 0.2.2; requires driving the app + force-kills on a **sacrificial**
cloned library (never the real one for destructive ops):
- Kill mid-operation (Organize / Flatten / Activate / Loadout apply / Update / Install / Bisection / Backup / Config save) → forensic diff after each.
- Manufactured filesystem failures (read-only dest/source, locked files, disk full, existing `.bak`/`.part`, source-deleted-between-preview-and-confirm).
- Bad-download abuse (empty / HTML-as-zip / no `modDesc.xml` / truncated / wrong-valid-mod / hash-mismatch).
- User-ownership conflicts (drop a different zip at a projected name; Silo must refuse to delete the user's file).
- Degraded network (SiloAPI down / malformed JSON / timeouts) → library + diagnostics stay usable.
- Fresh-install vs upgrade matrix.

### Sign-off
- Static integrity audit: **PASS** (automated, 2026-08-01).
- Interactive abuse battery: **PENDING** (operator-driven).
- Overall: suitable for **controlled beta**; run the interactive battery before a broad push.

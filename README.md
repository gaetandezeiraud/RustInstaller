# RustIInstaller

Local, single-file `.exe` installer in the style of InstallShield / MSI — but
written in Rust and built around the same BLAKE3 + HDiffPatch manifest format
used by the sibling RustUpdater project.

Each output `.exe` carries its own payload (a zip + a signed manifest) embedded
as Win32 `RT_RCDATA` resources. No network, no admin elevation, no MSI runtime.

## Workspace

| Crate | Type | Purpose |
|---|---|---|
| `common` | lib | Manifest types (`Manifest`, `FileEntry`, `PatchInfo`, `InstallerPayload`, `SignedPayload`), BLAKE3 hashing, file scan, HDiffPatch generation wrapper. |
| `installer_builder` | bin | Offline build tool. Generates Ed25519 keypairs and packs a directory (or a `from_dir`/`to_dir` pair) into a self-contained installer `.exe`. |
| `installer` | bin | The installer stub. Loads its own RCDATA payload, verifies the Ed25519 signature against a public key compiled in at build time, checks per-file BLAKE3 hashes, and either fresh-extracts the zip or applies HDiffPatch deltas in place. Modernized Win32 UI (Segoe UI, license page, progress, run-now checkbox). |
| `uninstaller` | bin | Built once by the builder and embedded as the 3rd RCDATA resource. The installer writes it to `<install_dir>\uninstall.exe` and registers it under HKCU `Software\Microsoft\Windows\CurrentVersion\Uninstall`, so the product shows up in Windows Apps. Reads the local manifest, removes every tracked file, removes the registry entry, and self-deletes via a detached `cmd /C` helper. |

## Security model

The installer carries four overlapping guarantees:

1. **Ed25519 signature** over the exact JSON bytes that describe the payload. The
   public key is **compiled** into the installer stub via the `INSTALLER_PUB_KEY`
   build-time env var — never embedded as a resource, so an attacker cannot swap
   key + payload together.
2. **BLAKE3 of the zip payload** is recorded in the signed manifest and
   re-verified at runtime before any byte is extracted.
3. **BLAKE3 per file** is recorded in `Manifest.files[...].hash` and checked
   after each write (full extract) or patch apply.
4. **Anti-rollback** via `min_installer_version`: if the running stub is older
   than the payload's minimum, install is refused.
5. **Patch from-version pinning**: a patch installer carries `from_version` and
   refuses to run unless the target directory's `version.json` matches.

Authenticode is **not** handled in code — sign the final `.exe` with
`signtool.exe sign /fd SHA256 /tr http://timestamp.digicert.com setup-...exe`
as a separate post-build step (the builder prints the exact command).

## Build

```pwsh
cargo build --release -p installer_builder
```

The installer stub is built on demand by `installer_builder pack` (with the
public key threaded through as an env var). You do not build `installer`
yourself.

If you need HDiffPatch deltas in patch installers, drop `hdiffz.exe` next to
`installer_builder.exe` (in `target\release\`). Without it the builder falls
back to shipping changed files in full and prints a warning.

## Workflow

### 1. Generate a keypair (once per product)

```pwsh
.\target\release\installer_builder.exe keygen --out .\keys
```

Produces `keys\priv.key` (keep secret) and `keys\pub.key`. Every installer you
ship must be signed with the same `priv.key`; the matching `pub.key` is
compiled into every stub.

### 2. Build a fresh installer

```pwsh
.\target\release\installer_builder.exe pack `
    --product myapp `
    --to-version 1.0 `
    --input .\build\myapp-1.0 `
    --exe myapp.exe `
    --priv-key .\keys\priv.key `
    --pub-key  .\keys\pub.key `
    --out .\dist\setup-myapp-1.0.exe
```

### 3. Build a patch installer

```pwsh
.\target\release\installer_builder.exe pack `
    --product myapp `
    --from-version 1.0 --from-dir .\build\myapp-1.0 `
    --to-version   1.1 --input    .\build\myapp-1.1 `
    --exe myapp.exe `
    --priv-key .\keys\priv.key `
    --pub-key  .\keys\pub.key `
    --out .\dist\patch-myapp-1.0-to-1.1.exe
```

A patch installer carries only the deltas (or the full bytes for files where
the delta would be bigger) plus the list of files to delete. Unchanged files
have no payload entry at all.

## Installation

### Interactive

Double-click the `.exe`. The wizard walks through:

1. **License** — lorem-ipsum EULA (placeholder), "I accept" checkbox gates the Next button.
2. **Choose install location** — default `%LOCALAPPDATA%\Programs\<product>`, with a native `IFileOpenDialog` folder picker.
3. **Progress** — Win11 progress bar + per-file status. Cancel-safe.
4. **Done** — "Run program now" checkbox (defaults to checked when `manifest.exe` is set). Finish launches the installed program via `ShellExecuteW`.

No admin elevation (manifest declares `asInvoker`). Segoe UI font, Common
Controls v6 visual styles, DPI-aware (`PerMonitorV2`).

### Minimal (app-triggered self-update)

```pwsh
.\setup-myapp-1.1.exe --minimal "C:\path\to\install"
.\setup-myapp-1.1.exe --minimal "C:\path\to\install" --launch
```

Compact windowed UI for updates the app launches itself. **No license page, no
folder picker, no Install button** — it starts the moment it opens and just
shows progress:

```text
┌────────────────────────────────────────────┐
│  ██      Applying update                    │
│  ██      MyApp 1.1                          │
│          [██████████░░░░░░░]  62%           │
│          Updating bin/app.exe               │
└────────────────────────────────────────────┘
```

App icon on the left (extracted from the installer's own embedded icon), title
+ version + progress bar + current-file status on the right. Closes itself
~0.9 s after reaching 100 %; on error it stays open with the message. Same
data-safe pre-flight as every install (closes the running app first, disk
check, etc.). Path resolves from the argument, `RUSTINSTALLER_PATH`, or the
default install dir. Implementation: [installer/src/ui_minimal.rs](installer/src/ui_minimal.rs).

### Silent (`/S` style, IT-friendly)

```pwsh
.\setup-myapp-1.0.exe --silent "C:\path\to\install"
.\setup-myapp-1.0.exe --silent "C:\path\to\install" --launch
```

`--launch` runs the installed `manifest.exe` after install (interactive UI
exposes this as the "Run program now" checkbox on the Done page).

Progress is printed to stderr, exit code is `0` on success, `1` on any failure
(bad signature, wrong from-version, anti-rollback, hash mismatch, etc.).

### Uninstall

The product appears in **Windows Settings → Apps → Installed apps** (and
classic Add/Remove Programs). Removing it from there launches
`<install_dir>\uninstall.exe`, which:

1. Walks `installer_manifest.json` and removes every tracked file.
2. Removes `version.json`, `installer_manifest.json`, `installer_info.json`.
3. Removes empty subdirectories.
4. Deletes the HKCU Uninstall registry entry.
5. Schedules `uninstall.exe` + `install_dir` cleanup via a detached
   `cmd /C ping … & del & rd` so the running process can exit first.

`uninstall.exe --silent` skips the confirmation dialog (used by the registry
`QuietUninstallString`).

### Inspect without installing

```pwsh
.\setup-myapp-1.0.exe --verify
```

Verifies the embedded payload and prints kind / versions / payload size.

## Runtime behavior

For every file in the manifest:

1. If the destination file already exists **and** its BLAKE3 matches the
   manifest — skip. This means a re-run of an installer is effectively
   instant.
2. Else, if this is a patch installer and the destination file exists and the
   manifest has a `PatchInfo` for it — read the patch out of the embedded zip,
   apply HDiffPatch, verify BLAKE3, atomic rename. Fall through to full
   extract if anything fails.
3. Else — read `full/<rel>` out of the embedded zip, verify BLAKE3, atomic
   rename.

Files listed in `Manifest.deleted_files` are removed afterwards.

`version.json` and `installer_manifest.json` are written to the install root —
they are the canonical record of what got installed and double as
state-required by any subsequent patch installer.

## UI

Single modernized Win32 UI — no Tauri, no WebView2, no HTML runtime. Common
Controls v6 visual styles, Segoe UI, DPI-aware `PerMonitorV2`, `asInvoker`
manifest. Deliberate choice: zero runtime dependencies, every supported
Windows version works, ~860 KB stub.

## License text

Pass `--license <path>` to `pack` and the UTF-8 text in that file becomes the
EULA shown on the installer's License page. Omitting it falls back to a
built-in lorem-ipsum placeholder. The text rides inside the signed
`InstallerPayload`, so tampering invalidates the Ed25519 signature.

```pwsh
installer_builder.exe pack `
    --product myapp --to-version 1.0 `
    --input .\build\myapp `
    --exe myapp.exe `
    --license .\legal\EULA-myapp-en.txt `
    --priv-key .\keys\priv.key --pub-key .\keys\pub.key `
    --out .\dist\setup-myapp-1.0.exe
```

`--verify` prints `License: custom (<bytes>)` or `License: built-in placeholder`.

## Icon inheritance

At pack time the builder reads `RT_GROUP_ICON` + every referenced `RT_ICON`
from `<input>/<exe>` (the app being packaged) via
`LoadLibraryExW(LOAD_LIBRARY_AS_DATAFILE)` and stamps them into both
`setup-…exe` and the embedded `uninstall.exe` via
`BeginUpdateResourceW / UpdateResourceW / EndUpdateResourceW`. Result:
Windows Explorer shows the application's own icon on the installer and
uninstaller files, and on the Add/Remove Programs entry (the registry
`DisplayIcon` already points at `uninstall.exe`).

The uninstaller is stamped in a staging copy under `%TEMP%`, then read into
the installer payload — the cached `target/release/uninstall.exe` is left
untouched between pack runs. If the source exe has no icon resources, the
build prints a notice and falls back to the Rust default.

## File associations

Pass `--assoc ".ext:Description"` (repeatable) to register file types under
`HKCU\Software\Classes` — per-user, no admin. The shell `open` verb points at
the installed `manifest.exe` with `"%1"`.

```pwsh
installer_builder.exe pack `
    --product MyApp --to-version 1.0 `
    --input .\build\myapp --exe myapp.exe `
    --assoc ".myx:MyApp Document" `
    --assoc ".myz:MyApp Archive" `
    --priv-key .\keys\priv.key --pub-key .\keys\pub.key `
    --out .\dist\setup-myapp-1.0.exe
```

Keys written per association (ProgID = `<sanitized-product>.<ext>`):

```text
HKCU\Software\Classes\.myx                          (default) = MyApp.myx
HKCU\Software\Classes\MyApp.myx                      (default) = MyApp Document
HKCU\Software\Classes\MyApp.myx\DefaultIcon          (default) = "<exe>",0
HKCU\Software\Classes\MyApp.myx\shell\open\command   (default) = "<exe>" "%1"
```

`SHChangeNotify(SHCNE_ASSOCCHANGED)` fires so Explorer refreshes immediately.
The chosen associations are recorded in `installer_info.json`; the uninstaller
removes exactly those ProgID trees and clears each `.ext` default **only if it
still points at our ProgID** (never stomping an association the user later
re-pointed). Shared `progid_for` in `common::assoc` keeps installer and
uninstaller in lock-step. Implementation: [common/src/assoc.rs](common/src/assoc.rs).

## Shortcuts

If the payload `manifest.exe` is non-empty, the installer drops two `.lnk`
files per user (no admin needed) pointing at `<install_dir>\<exe>`:

- `%APPDATA%\Microsoft\Windows\Start Menu\Programs\<product>.lnk`
- `%USERPROFILE%\Desktop\<product>.lnk`

Same code path as the launcher (`mslnk::ShellLink`). Both are removed by the
uninstaller. Path logic lives in `common::shortcuts` so installer and
uninstaller never drift apart on file naming.

## Limitations / V1 scope

- Windows only.
- No GUI folder picker on Windows < 7 (we use modern `IFileOpenDialog`).
## Closing the running app

When installing over an existing version, the installer first makes sure no
copy of the target exe (`manifest.exe`, matched by full path inside the
install dir, file-name fallback) is still running — otherwise its files are
locked.

**Data-safe — the installer never force-kills.** It:

1. Focuses the app's main window and posts `WM_CLOSE`, so the app shows its
   own "save your work?" prompt.
2. Waits for the user to finish closing it, re-focusing + re-sending
   `WM_CLOSE` every 5 s so the prompt stays in view.
3. Proceeds the instant the process exits.

There is no timeout-then-terminate. If the user never closes the app the only
way out is the **Cancel** button (or `Ctrl+C` in silent mode), which aborts
the install with `"<app> is still running"`. Unsaved work is always the user's
to keep.

```
INFO  target app running (1 process(es)); requesting close (no force)
INFO  target app closed by user after 6s
```

Console / windowless processes have no window to message — the installer
simply waits for them to exit (or Cancel). Implementation: [installer/src/proc.rs](installer/src/proc.rs).

## Crash safety (two-phase commit)

Installs and patches are transactional. Nothing in the live install is touched
until every file is built and hash-verified.

**Phase 1 — Stage.** Each new/changed file is produced under
`.installer_tmp/staged/` (full extract, or `hdiff(existing, patch)` for
patches) and verified by BLAKE3. The existing install is untouched, so a
cancel or crash here leaves the old version fully intact.

**Phase 2 — Commit.** A `commit.journal` lists every path about to change.
Then, per file: the current version is moved to `.installer_tmp/backup/`, and
the staged file is moved into place. Each move retries for ~5 s to ride out
transient locks (AV scanner, Explorer, search indexer).

**Rollback.** If any commit step fails, every already-committed file is
restored from its backup (and brand-new files removed), returning the install
to its exact pre-install state, then the error is reported.

**Power-loss recovery.** On the next launch, if a `commit.journal` is found,
the previous run was interrupted mid-commit — the installer rolls back to the
pre-install state from the backups before doing anything else.

State files (`version.json`, `installer_manifest.json`) are written
`.tmp`-then-rename so a crash can't leave corrupt JSON. Commit order ensures a
crash between "files committed" and "state written" self-heals on re-run
(everything hash-skips, state is rewritten). Implementation:
[installer/src/extract.rs](installer/src/extract.rs).

This closes the classic installer failure modes: half-written installs, no-undo
patch failures, power loss, and locked/anti-virus-held files.

## Disk space pre-check

Before writing a single byte the installer queries free space on the chosen
install volume (`fs4::available_space`) and refuses to start if short.

Estimate = **total install size + 100 MB buffer**, for both full and patch.
With the two-phase commit, staging writes the *full* content of every changed
file into `.installer_tmp/staged/` and they coexist until commit; the commit
itself is rename-only (same volume) so it costs no extra space. A patch's
staged output is the reconstructed *full* file, not the small patch blob — so
patches need the same headroom as a full install (the old "patch = patch size"
estimate would under-count and is gone). The figure is conservative:
hash-skipped unchanged files are counted but never actually staged.

On failure the installer bails with a human-readable message (shown in the UI
/ printed in silent mode) and logs the figures:

```
INFO  disk space: required ~100.3 MB (full, staged worst-case), available 214.51 GB on C:\…\install_target
ERROR insufficient disk space: need 2.10 GB but only 512.0 MB free
```

## Log files

Every install and uninstall writes a timestamped UTC log so failures in the
field can be debugged without a debugger. Format: one line per event,
`YYYY-MM-DDTHH:MM:SS.mmmZ <LEVEL> <message>`. Logger flushes after every
write, so even a crashed process leaves a complete file.

| Operation | Path | Notes |
|---|---|---|
| **Install** (any mode) | `<install_dir>\install.log` | Removed by the uninstaller, so it lives exactly as long as the product. |
| **Uninstall — Stage 1 + Stage 2** | `%TEMP%\rustinst-uninstall-<stage1-pid>.log` | Single combined file. Stage 1's PID is the identifier; Stage 2 receives it as `parent_pid` and appends. Survives the `rmdir` of the install directory. |

Sample install log:
```
2026-05-30T06:40:54.599Z INFO  install start: product=testapp version=1.0 kind=Full install_dir=C:\…\install_target
2026-05-30T06:40:54.599Z INFO  payload 201745 bytes, 3 files, deleted 0
2026-05-30T06:40:54.602Z INFO  extracted: bin/app.exe (360448 bytes)
2026-05-30T06:40:54.604Z INFO  extracted: data/config.json (9 bytes)
2026-05-30T06:40:54.605Z INFO  extracted: data/readme.txt (10 bytes)
2026-05-30T06:40:54.607Z INFO  install complete in 8ms
```

Sample uninstall log:
```
2026-05-30T06:42:29.768Z INFO  stage1 start: product=testapp version=1.0 install_dir=C:\…\install_target silent=true
2026-05-30T06:42:29.770Z INFO  removed 3 payload files
2026-05-30T06:42:29.771Z INFO  removed shortcuts
2026-05-30T06:42:29.771Z INFO  removed 2 state files
2026-05-30T06:42:29.772Z INFO  unregistered HKCU\…\Uninstall\testapp
2026-05-30T06:42:29.822Z INFO  stage2 start: product=testapp install_dir=… parent_pid=Some(27068)
2026-05-30T06:42:29.845Z INFO  stage2 complete; self scheduled for delete-on-reboot
```

Implementation lives in [common/src/log.rs](common/src/log.rs) — global
`OnceLock<Logger>` with a `Mutex<File>`, three levels (`INFO`/`WARN`/`ERROR`),
calls before `init()` are no-ops.

## Languages

Installer + uninstaller pick the UI language in this order:

1. `--lang <code>` CLI flag (e.g. `--lang fr`)
2. `RUSTINSTALLER_LANG` env var
3. OS user locale via `GetUserDefaultLocaleName` (first 2 ISO-639 chars)
4. English fallback

Strings live in `common/locales/<code>.toml` and are embedded at compile time
via `include_str!`. Adding a language = drop a new TOML, add it to the
`SUPPORTED` slice in `common/src/i18n.rs`. Missing keys fall back to English
then to the key literal (never blank).

Bundled today: **en** (default), **fr**.

`Translator::detect(&args)` is called once at startup; both stages of the
uninstaller share the same lookup via a thread-local in `ui.rs`.

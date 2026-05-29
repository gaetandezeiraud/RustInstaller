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

## WebView2

The installer detects the WebView2 Evergreen Runtime (registry GUID
`{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}`) at startup. `--verify` prints the
detected version. The HTML/WebView2 UI front-end itself is **scaffolded but
not implemented in V1** — the modernized Win32 UI is used in all cases. The
detection hook is in place so the WebView2 front-end can drop in cleanly
without touching the rest of the install pipeline.

## Limitations / V1 scope

- Windows only.
- No automatic shortcut / start-menu entry yet (V2).
- No GUI folder picker on Windows < 7 (we use modern `IFileOpenDialog`).
- WebView2 front-end not yet wired (detection only). Modernized Win32 native
  UI is the only renderer in V1.
- English-only UI.
- License text is a placeholder (lorem ipsum); the build tool does not yet
  accept a `--license` flag to inject a real EULA.

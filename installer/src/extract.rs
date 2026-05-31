use anyhow::{Context, Result, bail};
use common::models::{InstallerPayload, Manifest, PayloadKind};
use hdiffpatch_rs::patchers::HDiff;
use std::fs::{self, File};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use zip::ZipArchive;

const PATCHES_PREFIX: &str = "patches/";
const FULL_PREFIX: &str = "full/";

pub struct InstallCtx<'a> {
    pub install_dir: PathBuf,
    pub payload: &'a InstallerPayload,
    pub zip_bytes: &'a [u8],
    pub cancel: Arc<AtomicBool>,
    pub on_progress: Arc<dyn Fn(u64, u64, &str) + Send + Sync>,
}

pub fn install(ctx: InstallCtx<'_>) -> Result<()> {
    let manifest = &ctx.payload.manifest;

    // Logger goes into install_dir; create the dir up-front so the file open
    // succeeds even on a fresh install where install_dir didn't exist yet.
    let _ = fs::create_dir_all(&ctx.install_dir);
    common::log::init(common::log::log_path_for_install(&ctx.install_dir));
    let started = std::time::Instant::now();
    common::log::info(format!(
        "install start: product={} version={} kind={:?} install_dir={}",
        ctx.payload.product,
        ctx.payload.to_version,
        ctx.payload.kind,
        ctx.install_dir.display()
    ));
    common::log::info(format!(
        "payload {} bytes, {} files, deleted {}",
        ctx.zip_bytes.len(),
        manifest.files.len(),
        manifest.deleted_files.len()
    ));

    if ctx.payload.kind == PayloadKind::Patch {
        let expected_from = ctx
            .payload
            .from_version
            .as_deref()
            .context("patch payload missing from_version")?;
        let current = read_local_version(&ctx.install_dir);
        let current_ref = current.as_deref().unwrap_or("");
        if current_ref != expected_from {
            common::log::error(format!(
                "patch refused: expected from_version={} found={}",
                expected_from, current_ref
            ));
            bail!(
                "patch expects installed version {} but found {}",
                expected_from,
                if current_ref.is_empty() { "(none)" } else { current_ref }
            );
        }
    }

    fs::create_dir_all(&ctx.install_dir)
        .with_context(|| format!("create {}", ctx.install_dir.display()))?;

    check_disk_space(&ctx.install_dir, manifest, ctx.payload.kind)?;

    // Close any running copy of the target app before touching its files.
    // Data-safe: focus window + WM_CLOSE so the app can prompt to save, then
    // wait for the user to actually close it. Never force-killed. No-op on a
    // fresh install. Cancelling the install aborts the wait.
    #[cfg(windows)]
    {
        let pcb = ctx.on_progress.clone();
        crate::proc::ensure_closed(
            &ctx.install_dir,
            &manifest.exe,
            &ctx.cancel,
            &move |msg| pcb(0, 0, msg),
        )?;
    }

    let temp_dir = ctx.install_dir.join(".installer_tmp");

    // If a previous run was interrupted mid-commit, roll the install back to
    // its pre-install state before doing anything else.
    recover_if_interrupted(&temp_dir, &ctx.install_dir);

    // Fresh staging + backup areas.
    let staged_dir = temp_dir.join("staged");
    let backup_dir = temp_dir.join("backup");
    let _ = fs::remove_dir_all(&temp_dir);
    fs::create_dir_all(&staged_dir).context("create staging dir")?;
    fs::create_dir_all(&backup_dir).context("create backup dir")?;

    let total_bytes: u64 = manifest.files.values().map(|e| e.size).sum();
    let done = Arc::new(AtomicU64::new(0));

    let mut archive =
        ZipArchive::new(Cursor::new(ctx.zip_bytes)).context("open embedded zip")?;

    // Deterministic order — easier UX and reproducible.
    let mut entries: Vec<(&String, &common::models::FileEntry)> =
        manifest.files.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));

    // ---- PHASE 1: STAGE ------------------------------------------------
    // Build every new/changed file in `staged/`, verified by hash. The live
    // install is NOT touched here, so cancelling or crashing during staging
    // leaves the existing install fully intact.
    let mut to_commit: Vec<String> = Vec::new();

    for (rel, entry) in entries {
        if ctx.cancel.load(Ordering::Relaxed) {
            common::log::warn("install cancelled by user during staging");
            cleanup(&temp_dir);
            bail!("cancelled by user");
        }

        let dest = ctx.install_dir.join(rel);
        (ctx.on_progress)(done.load(Ordering::Relaxed), total_bytes, rel);

        // Hash-skip if already correct.
        if dest.exists() {
            if let Ok(h) = hash_file(&dest) {
                if h == entry.hash {
                    common::log::info(format!("skip (hash match): {}", rel));
                    done.fetch_add(entry.size, Ordering::Relaxed);
                    (ctx.on_progress)(done.load(Ordering::Relaxed), total_bytes, rel);
                    continue;
                }
            }
        }

        let staged_path = staged_dir.join(staged_name(rel));
        stage_file(
            &mut archive,
            ctx.payload.kind,
            rel,
            entry,
            &dest,
            &staged_path,
        )?;
        to_commit.push(rel.clone());

        done.fetch_add(entry.size, Ordering::Relaxed);
        (ctx.on_progress)(done.load(Ordering::Relaxed), total_bytes, rel);
    }

    // ---- PHASE 2: COMMIT ----------------------------------------------
    // Everything is staged + verified. Now swap files into place. A journal
    // records every path we touch so an interruption can be rolled back.
    let deleted: Vec<String> = manifest
        .deleted_files
        .iter()
        .filter(|rel| ctx.install_dir.join(rel).exists())
        .cloned()
        .collect();

    if to_commit.is_empty() && deleted.is_empty() {
        common::log::info("nothing to commit (already up to date)");
    } else {
        common::log::info(format!(
            "committing {} file(s), deleting {}",
            to_commit.len(),
            deleted.len()
        ));
        (ctx.on_progress)(total_bytes, total_bytes, "Finalizing…");
        write_journal(&temp_dir, &to_commit, &deleted)?;

        let commit_result = (|| -> Result<()> {
            for rel in &to_commit {
                commit_one(&ctx.install_dir, &staged_dir, &backup_dir, rel)?;
            }
            for rel in &deleted {
                backup_then_remove(&ctx.install_dir, &backup_dir, rel)?;
            }
            Ok(())
        })();

        if let Err(e) = commit_result {
            common::log::error(format!("commit failed: {e:#} — rolling back"));
            rollback(&temp_dir, &ctx.install_dir, &to_commit, &deleted);
            cleanup(&temp_dir);
            return Err(e).context("install failed and was rolled back");
        }

        // Commit done — drop the journal so recovery won't fire, then persist
        // state and clean up. (A crash past this point self-heals on re-run.)
        let _ = fs::remove_file(journal_path(&temp_dir));
    }

    write_local_state(&ctx.install_dir, &ctx.payload.to_version, manifest)?;
    cleanup(&temp_dir);

    common::log::info(format!(
        "install complete in {}ms",
        started.elapsed().as_millis()
    ));

    (ctx.on_progress)(total_bytes, total_bytes, "done");
    Ok(())
}

/// Build the final content for `rel` into `staged_path`, verified by BLAKE3.
/// Tries an in-place patch (against the existing `dest`) first, falls back to
/// the full file from the zip. Does not touch `dest`.
fn stage_file(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    kind: PayloadKind,
    rel: &str,
    entry: &common::models::FileEntry,
    dest: &Path,
    staged_path: &Path,
) -> Result<()> {
    // Patch path: apply hdiff(old=dest, patch) → staged_path.
    if kind == PayloadKind::Patch {
        if let Some(patch_info) = &entry.patch {
            if dest.exists() {
                let patch_rel = strip_prefix(&patch_info.file, PATCHES_PREFIX)
                    .map(|s| format!("{}{}", PATCHES_PREFIX, s))
                    .unwrap_or_else(|| patch_info.file.clone());
                if let Ok(patch_bytes) = read_from_zip(archive, &patch_rel) {
                    let patch_tmp = staged_path.with_extension("patch");
                    if fs::write(&patch_tmp, &patch_bytes).is_ok() {
                        let ok = run_hdiff(dest, &patch_tmp, staged_path);
                        let _ = fs::remove_file(&patch_tmp);
                        if ok && hash_file(staged_path).ok().as_deref() == Some(&entry.hash) {
                            common::log::info(format!("staged (patch): {}", rel));
                            return Ok(());
                        }
                        common::log::warn(format!(
                            "patch unusable, falling back to full: {}",
                            rel
                        ));
                        let _ = fs::remove_file(staged_path);
                    }
                }
            }
        }
    }

    // Full file from zip.
    let zip_rel = format!("{}{}", FULL_PREFIX, rel);
    let bytes = read_from_zip(archive, &zip_rel)
        .with_context(|| format!("read {} from embedded zip", zip_rel))?;
    let actual = blake3::hash(&bytes).to_hex().to_string();
    if actual != entry.hash {
        common::log::error(format!(
            "zip vs manifest hash mismatch: {} (zip={} manifest={})",
            rel, actual, entry.hash
        ));
        bail!("hash mismatch for {} (zip vs manifest)", rel);
    }
    let mut f = File::create(staged_path)
        .with_context(|| format!("create staged {}", staged_path.display()))?;
    f.write_all(&bytes)
        .with_context(|| format!("write staged {}", staged_path.display()))?;
    common::log::info(format!("staged (full): {} ({} bytes)", rel, entry.size));
    Ok(())
}

/// Safety margin on top of the estimated payload size.
const SPACE_BUFFER: u64 = 100 * 1024 * 1024; // 100 MB

/// Verify the install volume has enough free space before writing anything.
/// Bails with a human-readable message (also logged) when short.
///
/// Space model for the two-phase commit:
/// - **Staging** writes the *full* content of every changed file into
///   `.installer_tmp/staged/` and they all coexist until commit. Worst case
///   (every file changed) that is the whole install size. For a patch the
///   staged output is the reconstructed *full* file, not the small patch blob,
///   so patches cost the same as a full install here — `total_patch_size`
///   would badly under-estimate.
/// - **Commit** only renames files within the same volume (dest→backup,
///   staged→dest), which consumes no additional space.
///
/// So the peak extra space needed is bounded by the total install size plus a
/// safety buffer, regardless of full vs patch.
fn check_disk_space(install_dir: &Path, manifest: &Manifest, kind: PayloadKind) -> Result<()> {
    let total_file_bytes: u64 = manifest.files.values().map(|e| e.size).sum();
    let required = total_file_bytes.saturating_add(SPACE_BUFFER);

    let available = fs4::available_space(install_dir)
        .with_context(|| format!("query free space on {}", install_dir.display()))?;

    common::log::info(format!(
        "disk space: required ~{} ({}, staged worst-case), available {} on {}",
        human_bytes(required),
        match kind {
            PayloadKind::Full => "full",
            PayloadKind::Patch => "patch",
        },
        human_bytes(available),
        install_dir.display()
    ));

    if available < required {
        common::log::error(format!(
            "insufficient disk space: need {} but only {} free",
            human_bytes(required),
            human_bytes(available)
        ));
        bail!(
            "Not enough disk space. Need about {} free on the install drive, but only {} is available.",
            human_bytes(required),
            human_bytes(available)
        );
    }
    Ok(())
}

fn human_bytes(b: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if b >= GB {
        format!("{:.2} GB", b as f64 / GB as f64)
    } else if b >= MB {
        format!("{:.1} MB", b as f64 / MB as f64)
    } else if b >= KB {
        format!("{:.1} KB", b as f64 / KB as f64)
    } else {
        format!("{} B", b)
    }
}

fn read_from_zip(archive: &mut ZipArchive<Cursor<&[u8]>>, rel: &str) -> Result<Vec<u8>> {
    let mut f = archive.by_name(rel).with_context(|| format!("{} not in zip", rel))?;
    let mut buf = Vec::with_capacity(f.size() as usize);
    f.read_to_end(&mut buf)?;
    Ok(buf)
}

fn run_hdiff(old: &Path, patch: &Path, out: &Path) -> bool {
    let old_s = old.to_string_lossy().to_string();
    let patch_s = patch.to_string_lossy().to_string();
    let out_s = out.to_string_lossy().to_string();
    let mut p = HDiff::new(old_s, patch_s, out_s);
    p.apply()
}

fn hash_file(path: &Path) -> Result<String> {
    let mut f = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    std::io::copy(&mut f, &mut hasher)?;
    Ok(hasher.finalize().to_hex().to_string())
}

// ---- Two-phase commit primitives --------------------------------------

/// Max attempts for a single rename when the target is briefly locked
/// (AV scanner, Explorer, indexer). 50 × 100 ms ≈ 5 s.
const MOVE_RETRIES: usize = 50;
const MOVE_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(100);

/// Flat, collision-free staged/backup file name for a relative path.
fn staged_name(rel: &str) -> String {
    blake3::hash(rel.as_bytes()).to_hex().to_string()
}

fn journal_path(temp_dir: &Path) -> PathBuf {
    temp_dir.join("commit.journal")
}

/// Move with retry, to survive transient locks (AV/Explorer/indexer).
fn move_retry(src: &Path, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut last_err = None;
    for attempt in 0..MOVE_RETRIES {
        // On Windows rename fails if dest exists; remove it first (also retried).
        if dest.exists() {
            let _ = fs::remove_file(dest);
        }
        match fs::rename(src, dest) {
            Ok(()) => {
                if attempt > 0 {
                    common::log::info(format!(
                        "move succeeded on attempt {} -> {}",
                        attempt + 1,
                        dest.display()
                    ));
                }
                return Ok(());
            }
            Err(e) => {
                last_err = Some(e);
                std::thread::sleep(MOVE_RETRY_DELAY);
            }
        }
    }
    Err(anyhow::anyhow!(
        "could not move {} -> {} after {} attempts: {}",
        src.display(),
        dest.display(),
        MOVE_RETRIES,
        last_err
            .map(|e| e.to_string())
            .unwrap_or_else(|| "unknown".into())
    ))
}

/// Back up the existing file (if any) then move the staged file into place.
fn commit_one(install_dir: &Path, staged_dir: &Path, backup_dir: &Path, rel: &str) -> Result<()> {
    let dest = install_dir.join(rel);
    let staged = staged_dir.join(staged_name(rel));
    if dest.exists() {
        let backup = backup_dir.join(staged_name(rel));
        move_retry(&dest, &backup)
            .with_context(|| format!("backup {} before overwrite", rel))?;
    }
    move_retry(&staged, &dest).with_context(|| format!("install {}", rel))?;
    Ok(())
}

/// Back up then remove an obsolete file (so rollback can restore it).
fn backup_then_remove(install_dir: &Path, backup_dir: &Path, rel: &str) -> Result<()> {
    let dest = install_dir.join(rel);
    if dest.exists() {
        let backup = backup_dir.join(staged_name(rel));
        move_retry(&dest, &backup).with_context(|| format!("backup {} before delete", rel))?;
    }
    Ok(())
}

/// Record every path the commit will touch, so an interrupted commit can be
/// rolled back on the next launch.
fn write_journal(temp_dir: &Path, adds: &[String], deletes: &[String]) -> Result<()> {
    let mut lines = Vec::with_capacity(adds.len() + deletes.len());
    for r in adds {
        lines.push(r.as_str());
    }
    for r in deletes {
        lines.push(r.as_str());
    }
    // Write to .tmp then rename so the journal itself appears atomically.
    let jp = journal_path(temp_dir);
    let tmp = jp.with_extension("journal.tmp");
    fs::write(&tmp, lines.join("\n")).context("write journal")?;
    fs::rename(&tmp, &jp).context("commit journal")?;
    Ok(())
}

/// Roll the live install back to its pre-commit state using the backups.
/// For each touched path: if a backup exists restore it, else the path was
/// newly added so remove it.
fn rollback(temp_dir: &Path, install_dir: &Path, adds: &[String], deletes: &[String]) {
    let backup_dir = temp_dir.join("backup");
    let restore = |rel: &str| {
        let dest = install_dir.join(rel);
        let backup = backup_dir.join(staged_name(rel));
        if backup.exists() {
            if let Err(e) = move_retry(&backup, &dest) {
                common::log::error(format!("rollback restore failed for {}: {e:#}", rel));
            }
        } else {
            // Newly added file with no prior version — remove it.
            let _ = fs::remove_file(&dest);
        }
    };
    for rel in adds {
        restore(rel);
    }
    for rel in deletes {
        restore(rel);
    }
    common::log::warn("rolled back to pre-install state");
}

/// On startup: if a commit journal is present, a previous run was interrupted
/// mid-commit (e.g. power loss). Roll back to the pre-install state.
fn recover_if_interrupted(temp_dir: &Path, install_dir: &Path) {
    let jp = journal_path(temp_dir);
    let Ok(content) = fs::read_to_string(&jp) else {
        return;
    };
    common::log::warn("found interrupted commit journal — rolling back");
    let backup_dir = temp_dir.join("backup");
    for rel in content.lines().filter(|l| !l.trim().is_empty()) {
        let dest = install_dir.join(rel);
        let backup = backup_dir.join(staged_name(rel));
        if backup.exists() {
            let _ = move_retry(&backup, &dest);
        } else {
            let _ = fs::remove_file(&dest);
        }
    }
    let _ = fs::remove_dir_all(temp_dir);
    common::log::warn("recovery complete: install rolled back to previous state");
}

fn read_local_version(install_dir: &Path) -> Option<String> {
    let p = install_dir.join("version.json");
    let s = fs::read_to_string(p).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    v["version"].as_str().map(|s| s.to_string())
}

/// Write state files atomically (.tmp then rename) so a crash can't leave a
/// half-written / corrupt JSON behind.
fn write_local_state(install_dir: &Path, version: &str, manifest: &Manifest) -> Result<()> {
    write_atomic(
        &install_dir.join("version.json"),
        serde_json::to_string_pretty(&serde_json::json!({ "version": version }))?.as_bytes(),
    )?;
    write_atomic(
        &install_dir.join("installer_manifest.json"),
        serde_json::to_string_pretty(manifest)?.as_bytes(),
    )?;
    Ok(())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, bytes).with_context(|| format!("write {}", tmp.display()))?;
    if path.exists() {
        let _ = fs::remove_file(path);
    }
    fs::rename(&tmp, path).with_context(|| format!("commit {}", path.display()))?;
    Ok(())
}

fn strip_prefix<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    s.strip_prefix(prefix)
}

fn cleanup(temp_dir: &Path) {
    let _ = fs::remove_dir_all(temp_dir);
}


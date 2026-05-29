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

    if ctx.payload.kind == PayloadKind::Patch {
        let expected_from = ctx
            .payload
            .from_version
            .as_deref()
            .context("patch payload missing from_version")?;
        let current = read_local_version(&ctx.install_dir);
        let current_ref = current.as_deref().unwrap_or("");
        if current_ref != expected_from {
            bail!(
                "patch expects installed version {} but found {}",
                expected_from,
                if current_ref.is_empty() { "(none)" } else { current_ref }
            );
        }
    }

    fs::create_dir_all(&ctx.install_dir)
        .with_context(|| format!("create {}", ctx.install_dir.display()))?;

    let temp_dir = ctx.install_dir.join(".installer_tmp");
    fs::create_dir_all(&temp_dir)?;

    let total_bytes: u64 = manifest.files.values().map(|e| e.size).sum();
    let done = Arc::new(AtomicU64::new(0));

    let mut archive =
        ZipArchive::new(Cursor::new(ctx.zip_bytes)).context("open embedded zip")?;

    // Deterministic order — easier UX and reproducible.
    let mut rels: Vec<&String> = manifest.files.keys().collect();
    rels.sort();

    for rel in rels {
        if ctx.cancel.load(Ordering::Relaxed) {
            cleanup(&temp_dir);
            bail!("cancelled by user");
        }

        let entry = manifest.files.get(rel).unwrap();
        let dest = ctx.install_dir.join(rel);
        (ctx.on_progress)(done.load(Ordering::Relaxed), total_bytes, rel);

        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }

        // Hash-skip if already correct.
        if dest.exists() {
            if let Ok(h) = hash_file(&dest) {
                if h == entry.hash {
                    done.fetch_add(entry.size, Ordering::Relaxed);
                    (ctx.on_progress)(done.load(Ordering::Relaxed), total_bytes, rel);
                    continue;
                }
            }
        }

        let mut applied = false;

        // Try patch if available and old file exists.
        if ctx.payload.kind == PayloadKind::Patch {
            if let Some(patch_info) = &entry.patch {
                if dest.exists() {
                    let patch_rel = strip_prefix(&patch_info.file, PATCHES_PREFIX)
                        .map(|s| format!("{}{}", PATCHES_PREFIX, s))
                        .unwrap_or_else(|| patch_info.file.clone());
                    if let Ok(patch_bytes) = read_from_zip(&mut archive, &patch_rel) {
                        let patch_tmp = temp_dir.join(format!(
                            "{}.patch",
                            blake3::hash(rel.as_bytes()).to_hex()
                        ));
                        if fs::write(&patch_tmp, &patch_bytes).is_ok() {
                            let out_tmp = temp_dir.join(format!(
                                "{}.patched",
                                blake3::hash(rel.as_bytes()).to_hex()
                            ));
                            let ok = run_hdiff(&dest, &patch_tmp, &out_tmp);
                            let _ = fs::remove_file(&patch_tmp);
                            if ok {
                                if let Ok(h) = hash_file(&out_tmp) {
                                    if h == entry.hash {
                                        replace_file(&out_tmp, &dest)?;
                                        applied = true;
                                    }
                                }
                            }
                            let _ = fs::remove_file(&out_tmp);
                        }
                    }
                }
            }
        }

        // Full extract fallback.
        if !applied {
            let zip_rel = format!("{}{}", FULL_PREFIX, rel);
            let bytes = read_from_zip(&mut archive, &zip_rel)
                .with_context(|| format!("read {} from embedded zip", zip_rel))?;
            let actual = blake3::hash(&bytes).to_hex().to_string();
            if actual != entry.hash {
                bail!("hash mismatch for {} (zip vs manifest)", rel);
            }
            let out_tmp = temp_dir.join(format!(
                "{}.full",
                blake3::hash(rel.as_bytes()).to_hex()
            ));
            {
                let mut f = File::create(&out_tmp)?;
                f.write_all(&bytes)?;
            }
            replace_file(&out_tmp, &dest)?;
        }

        done.fetch_add(entry.size, Ordering::Relaxed);
        (ctx.on_progress)(done.load(Ordering::Relaxed), total_bytes, rel);
    }

    delete_files(&ctx.install_dir, &manifest.deleted_files);

    write_local_state(&ctx.install_dir, &ctx.payload.to_version, manifest)?;

    cleanup(&temp_dir);

    (ctx.on_progress)(total_bytes, total_bytes, "done");
    Ok(())
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

fn replace_file(src: &Path, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    // fs::rename may fail across volumes; install_dir is one volume so it's fine.
    if dest.exists() {
        let _ = fs::remove_file(dest);
    }
    fs::rename(src, dest)
        .with_context(|| format!("rename {} -> {}", src.display(), dest.display()))
}

fn delete_files(install_dir: &Path, list: &[String]) {
    for rel in list {
        let p = install_dir.join(rel);
        if p.exists() {
            let _ = fs::remove_file(&p);
        }
    }
}

fn read_local_version(install_dir: &Path) -> Option<String> {
    let p = install_dir.join("version.json");
    let s = fs::read_to_string(p).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    v["version"].as_str().map(|s| s.to_string())
}

fn write_local_state(install_dir: &Path, version: &str, manifest: &Manifest) -> Result<()> {
    fs::write(
        install_dir.join("version.json"),
        serde_json::to_string_pretty(&serde_json::json!({ "version": version }))?,
    )?;
    fs::write(
        install_dir.join("installer_manifest.json"),
        serde_json::to_string_pretty(manifest)?,
    )?;
    Ok(())
}

fn strip_prefix<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    s.strip_prefix(prefix)
}

fn cleanup(temp_dir: &Path) {
    let _ = fs::remove_dir_all(temp_dir);
}


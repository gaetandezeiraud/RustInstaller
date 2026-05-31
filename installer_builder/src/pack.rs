use crate::args::PackArgs;
use crate::embed;
use anyhow::{Context, Result, bail};
use common::models::{
    FileAssoc, FileEntry, InstallerPayload, Manifest, PatchInfo, PayloadKind, SignedPayload,
};
use common::utils::{bytes_blake3, collect_files, file_blake3, generate_patch};
use ed25519_dalek::{Signer, SigningKey};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

const PATCHES_PREFIX: &str = "patches/";
const FULL_PREFIX: &str = "full/";

pub fn run(args: &PackArgs) -> Result<()> {
    let is_patch = args.from_dir.is_some() || args.from_version.is_some();
    if is_patch && (args.from_dir.is_none() || args.from_version.is_none()) {
        bail!("patch mode requires both --from-dir and --from-version");
    }

    println!(
        "Mode: {}",
        if is_patch { "PATCH" } else { "FULL" }
    );

    let signing = load_signing_key(&args.priv_key)?;
    let pub_key_hex = load_pub_key_hex(&args.pub_key)?;

    let zip_bytes;
    let manifest;
    if is_patch {
        let from_dir = args
            .from_dir
            .as_ref()
            .context("patch mode requires --from-dir")?;
        (zip_bytes, manifest) = build_patch(&args.input, from_dir, &args.exe, &args.to_version)?;
    } else {
        (zip_bytes, manifest) = build_full(&args.input, &args.exe, &args.to_version)?;
    }

    let license_text = match &args.license {
        Some(p) => {
            let text = fs::read_to_string(p)
                .with_context(|| format!("read license file {}", p.display()))?;
            println!("License: {} ({} bytes) from {}", trimmed_title(&text), text.len(), p.display());
            Some(text)
        }
        None => None,
    };

    let associations = parse_assocs(&args.assoc, &args.product)?;

    if args.publisher.trim().is_empty() {
        bail!("--publisher must not be empty");
    }

    let payload = InstallerPayload {
        kind: if is_patch { PayloadKind::Patch } else { PayloadKind::Full },
        product: args.product.clone(),
        publisher: args.publisher.clone(),
        from_version: args.from_version.clone(),
        to_version: args.to_version.clone(),
        min_installer_version: args.min_installer_version.clone(),
        payload_blake3: bytes_blake3(&zip_bytes),
        created_at_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or_default(),
        manifest,
        license_text,
        associations,
    };

    let payload_json = serde_json::to_string(&payload).context("serialize payload")?;
    let signature = signing.sign(payload_json.as_bytes());
    let signed = SignedPayload {
        payload_json,
        signature_hex: hex::encode(signature.to_bytes()),
    };
    let signed_json = serde_json::to_string(&signed).context("serialize signed payload")?;

    println!("Payload: {} bytes (zip)", zip_bytes.len());
    println!("Signed manifest: {} bytes", signed_json.len());

    let stub = build_installer_stub(&pub_key_hex, args.reuse_stub)?;
    println!("Stub: {}", stub.display());

    // Pull the icon from the packaged exe (best-effort).
    #[cfg(windows)]
    let icons = {
        let exe_path = args.input.join(&args.exe);
        if exe_path.exists() {
            match crate::icon::extract_from_exe(&exe_path) {
                Ok(Some(i)) => {
                    println!(
                        "Icon: {} group(s) + {} icon(s) copied from {}",
                        i.groups.len(),
                        i.icons.len(),
                        exe_path.display()
                    );
                    Some(i)
                }
                Ok(None) => {
                    println!("Icon: source exe {} has no icon resources", exe_path.display());
                    None
                }
                Err(e) => {
                    eprintln!("warning: icon extraction failed: {e:#}");
                    None
                }
            }
        } else {
            None
        }
    };
    #[cfg(not(windows))]
    let icons: Option<()> = None;

    let uninstaller = build_uninstaller(args.reuse_stub)?;
    // Stage uninstaller into %TEMP%, stamp icons there, then read its bytes
    // for the installer RCDATA payload. Avoids mutating the cached release artifact.
    let staged_uninstaller = std::env::temp_dir().join(format!(
        "rustinst-uninst-{}.exe",
        std::process::id()
    ));
    fs::copy(&uninstaller, &staged_uninstaller).with_context(|| {
        format!("stage uninstaller {} -> {}", uninstaller.display(), staged_uninstaller.display())
    })?;
    #[cfg(windows)]
    if let Some(i) = &icons {
        if let Err(e) = crate::icon::embed_icons(&staged_uninstaller, i) {
            eprintln!("warning: icon embed into uninstaller failed: {e:#}");
        }
    }
    let uninstaller_bytes = fs::read(&staged_uninstaller)
        .with_context(|| format!("read {}", staged_uninstaller.display()))?;
    let _ = fs::remove_file(&staged_uninstaller);
    println!("Uninstaller: {} bytes (icon-stamped)", uninstaller_bytes.len());

    if let Some(parent) = args.out.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(&stub, &args.out).with_context(|| format!("copy {} -> {}", stub.display(), args.out.display()))?;
    println!("Copied stub to {}", args.out.display());

    embed::embed_resources(&args.out, &zip_bytes, signed_json.as_bytes(), &uninstaller_bytes)?;
    #[cfg(windows)]
    if let Some(i) = &icons {
        if let Err(e) = crate::icon::embed_icons(&args.out, i) {
            eprintln!("warning: icon embed into setup failed: {e:#}");
        }
    }
    println!(
        "Embedded payload + signed manifest + uninstaller{} into {}",
        if icons.is_some() { " + icon" } else { "" },
        args.out.display()
    );

    println!();
    println!("DONE.");
    println!("Next step (Authenticode): signtool sign /fd SHA256 /tr http://timestamp.digicert.com {}", args.out.display());
    Ok(())
}

fn load_signing_key(path: &Path) -> Result<SigningKey> {
    let hex_data = fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
    let bytes = hex::decode(hex_data.trim())
        .with_context(|| format!("decode hex private key {}", path.display()))?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("private key must be 32 bytes"))?;
    Ok(SigningKey::from_bytes(&arr))
}

fn load_pub_key_hex(path: &Path) -> Result<String> {
    let hex_data = fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
    let hex_data = hex_data.trim().to_string();
    let bytes = hex::decode(&hex_data)
        .with_context(|| format!("decode hex public key {}", path.display()))?;
    if bytes.len() != 32 {
        bail!("public key must be 32 bytes");
    }
    Ok(hex_data)
}

fn build_full(input: &Path, exe: &str, version: &str) -> Result<(Vec<u8>, Manifest)> {
    println!("Scanning {}", input.display());
    let files = collect_files(input)?;
    println!("Found {} files", files.len());

    let total_size = Mutex::new(0u64);
    let entries: HashMap<String, FileEntry> = files
        .par_iter()
        .map(|rel| -> Result<(String, FileEntry, Vec<u8>)> {
            let abs = input.join(rel);
            let bytes = fs::read(&abs).with_context(|| format!("read {}", abs.display()))?;
            let hash = bytes_blake3(&bytes);
            let size = bytes.len() as u64;
            {
                let mut t = total_size.lock().unwrap();
                *t += size;
            }
            Ok((rel.clone(), FileEntry { hash, size, patch: None }, bytes))
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .map(|(rel, entry, _)| (rel, entry))
        .collect();

    // Re-read files for zipping (sequential - zip crate isn't thread-safe writer).
    let zip_bytes = write_zip(input, &files, &[], &HashMap::new())?;

    let manifest = Manifest {
        version: version.to_string(),
        exe: exe.to_string(),
        files: entries,
        deleted_files: Vec::new(),
        full_size: *total_size.lock().unwrap(),
        total_patch_size: 0,
    };
    Ok((zip_bytes, manifest))
}

fn build_patch(
    new_input: &Path,
    old_input: &Path,
    exe: &str,
    version: &str,
) -> Result<(Vec<u8>, Manifest)> {
    if let Ok(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .ok_or(())
    {
        let hd = exe_dir.join("hdiffz.exe");
        if !hd.exists() {
            eprintln!(
                "warning: {} not found - patch payload will ship full files instead of HDiffPatch deltas",
                hd.display()
            );
        }
    }

    println!("Scanning new {}", new_input.display());
    let new_files = collect_files(new_input)?;
    println!("Scanning old {}", old_input.display());
    let old_files = collect_files(old_input)?;

    let new_set: HashSet<&String> = new_files.iter().collect();
    let old_set: HashSet<&String> = old_files.iter().collect();

    let mut deleted_files: Vec<String> = old_files
        .iter()
        .filter(|p| !new_set.contains(*p))
        .cloned()
        .collect();
    deleted_files.sort();

    let total_full_size = Mutex::new(0u64);
    let total_patch_size = Mutex::new(0u64);

    // Per-file work: hash new, hash old if present, generate patch if both exist + differ.
    let temp_patches = std::env::temp_dir().join(format!(
        "rustinstaller-patches-{}",
        std::process::id()
    ));
    fs::create_dir_all(&temp_patches)?;

    struct WorkOut {
        rel: String,
        entry: FileEntry,
        patch_path: Option<PathBuf>,
        full_needed: bool,
    }

    let work: Vec<WorkOut> = new_files
        .par_iter()
        .map(|rel| -> Result<WorkOut> {
            let new_abs = new_input.join(rel);
            let new_hash = file_blake3(&new_abs)?;
            let new_size = fs::metadata(&new_abs)?.len();
            {
                let mut t = total_full_size.lock().unwrap();
                *t += new_size;
            }

            if !old_set.contains(rel) {
                return Ok(WorkOut {
                    rel: rel.clone(),
                    entry: FileEntry { hash: new_hash, size: new_size, patch: None },
                    patch_path: None,
                    full_needed: true,
                });
            }

            let old_abs = old_input.join(rel);
            let old_hash = file_blake3(&old_abs)?;
            if old_hash == new_hash {
                // Unchanged - no payload entry needed.
                return Ok(WorkOut {
                    rel: rel.clone(),
                    entry: FileEntry { hash: new_hash, size: new_size, patch: None },
                    patch_path: None,
                    full_needed: false,
                });
            }

            let safe_name = blake3::hash(rel.as_bytes()).to_hex().to_string();
            let patch_path = temp_patches.join(format!("{}.patch", safe_name));
            let ok = generate_patch(&old_abs, &new_abs, &patch_path)
                .with_context(|| format!("hdiffz {}", rel))?;
            if ok && patch_path.exists() {
                let psize = fs::metadata(&patch_path)?.len();
                // Heuristic: if patch is bigger than the full file, just ship the full.
                if psize < new_size {
                    {
                        let mut t = total_patch_size.lock().unwrap();
                        *t += psize;
                    }
                    return Ok(WorkOut {
                        rel: rel.clone(),
                        entry: FileEntry {
                            hash: new_hash,
                            size: new_size,
                            patch: Some(PatchInfo {
                                file: format!("{}{}.patch", PATCHES_PREFIX, safe_name),
                                size: psize,
                            }),
                        },
                        patch_path: Some(patch_path),
                        full_needed: false,
                    });
                }
                // Patch wasn't smaller - fall through to full.
                let _ = fs::remove_file(&patch_path);
            }

            Ok(WorkOut {
                rel: rel.clone(),
                entry: FileEntry { hash: new_hash, size: new_size, patch: None },
                patch_path: None,
                full_needed: true,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let mut entries: HashMap<String, FileEntry> = HashMap::new();
    let mut full_paths: Vec<String> = Vec::new();
    let mut patch_paths: HashMap<String, PathBuf> = HashMap::new();
    for w in work {
        if w.full_needed {
            full_paths.push(w.rel.clone());
        }
        if let Some(p) = &w.patch_path {
            patch_paths.insert(w.rel.clone(), p.clone());
        }
        entries.insert(w.rel, w.entry);
    }

    let zip_bytes = write_zip(new_input, &full_paths, &[], &patch_paths)?;

    let _ = fs::remove_dir_all(&temp_patches);

    let manifest = Manifest {
        version: version.to_string(),
        exe: exe.to_string(),
        files: entries,
        deleted_files,
        full_size: *total_full_size.lock().unwrap(),
        total_patch_size: *total_patch_size.lock().unwrap(),
    };
    Ok((zip_bytes, manifest))
}

/// Build a zip in memory.
///
/// - Files listed in `full_paths` go under `full/<rel>` (read from `input`).
/// - Files listed in `patch_paths` go under the file path recorded by the builder.
fn write_zip(
    input: &Path,
    full_paths: &[String],
    _unused: &[String],
    patch_paths: &HashMap<String, PathBuf>,
) -> Result<Vec<u8>> {
    let buf = Vec::with_capacity(8 * 1024 * 1024);
    let cursor = Cursor::new(buf);
    let mut zip = ZipWriter::new(cursor);
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    for rel in full_paths {
        let abs = input.join(rel);
        let bytes = fs::read(&abs).with_context(|| format!("read {}", abs.display()))?;
        let zip_path = format!("{}{}", FULL_PREFIX, rel);
        zip.start_file(&zip_path, opts)?;
        zip.write_all(&bytes)?;
    }

    for (rel, patch_path) in patch_paths {
        let bytes = fs::read(patch_path)
            .with_context(|| format!("read patch {}", patch_path.display()))?;
        let safe_name = blake3::hash(rel.as_bytes()).to_hex().to_string();
        let zip_path = format!("{}{}.patch", PATCHES_PREFIX, safe_name);
        zip.start_file(&zip_path, opts)?;
        zip.write_all(&bytes)?;
    }

    let cursor = zip.finish()?;
    Ok(cursor.into_inner())
}

/// Build (or reuse) the installer stub with the given public key compiled in.
/// Returns the path to the built `.exe`.
fn build_installer_stub(pub_key_hex: &str, reuse: bool) -> Result<PathBuf> {
    let workspace_root = find_workspace_root()?;
    let target_exe = workspace_root
        .join("target")
        .join("release")
        .join("installer.exe");

    if reuse && target_exe.exists() {
        println!("Reusing existing stub at {}", target_exe.display());
        return Ok(target_exe);
    }

    println!("Building installer stub (cargo build -p installer --release)...");
    let status = std::process::Command::new("cargo")
        .args(["build", "-p", "installer", "--release"])
        .env("INSTALLER_PUB_KEY", pub_key_hex)
        .current_dir(&workspace_root)
        .status()
        .context("invoke cargo build")?;

    if !status.success() {
        bail!("cargo build failed");
    }
    if !target_exe.exists() {
        bail!("expected stub not found at {}", target_exe.display());
    }
    Ok(target_exe)
}

fn build_uninstaller(reuse: bool) -> Result<PathBuf> {
    let workspace_root = find_workspace_root()?;
    let target_exe = workspace_root
        .join("target")
        .join("release")
        .join("uninstall.exe");

    if reuse && target_exe.exists() {
        return Ok(target_exe);
    }

    println!("Building uninstaller (cargo build -p uninstaller --release)...");
    let status = std::process::Command::new("cargo")
        .args(["build", "-p", "uninstaller", "--release"])
        .current_dir(&workspace_root)
        .status()
        .context("invoke cargo build uninstaller")?;
    if !status.success() {
        bail!("uninstaller cargo build failed");
    }
    if !target_exe.exists() {
        bail!("expected uninstaller not found at {}", target_exe.display());
    }
    Ok(target_exe)
}

fn find_workspace_root() -> Result<PathBuf> {
    let mut p: PathBuf = std::env::current_dir()?;
    loop {
        let manifest = p.join("Cargo.toml");
        if manifest.exists() {
            let text = fs::read_to_string(&manifest).unwrap_or_default();
            if text.contains("[workspace]") {
                return Ok(p);
            }
        }
        if !p.pop() {
            bail!("could not locate workspace root from {:?}", std::env::current_dir());
        }
    }
}

/// Parse `--assoc ".ext:Description"` entries into `FileAssoc`s.
/// Extension is normalized to a leading dot; description may contain colons.
fn parse_assocs(raw: &[String], product: &str) -> Result<Vec<FileAssoc>> {
    let mut out = Vec::with_capacity(raw.len());
    for s in raw {
        let (ext, desc) = s
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("bad --assoc '{}': expected \".ext:Description\"", s))?;
        let ext = common::assoc::normalize_ext(ext);
        if ext == "." {
            bail!("bad --assoc '{}': empty extension", s);
        }
        let description = desc.trim().to_string();
        let progid = common::assoc::progid_for(product, &ext);
        println!("Association: {} -> {} ({})", ext, progid, description);
        out.push(FileAssoc { ext, description });
    }
    Ok(out)
}

/// First non-empty line of `s`, truncated to 60 chars - used for log preview.
fn trimmed_title(s: &str) -> String {
    let line = s.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    if line.chars().count() > 60 {
        format!("{}...", line.chars().take(60).collect::<String>())
    } else {
        line.to_string()
    }
}

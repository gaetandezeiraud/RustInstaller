use anyhow::{Context, Result};
use std::fs::{self, File};
use std::path::Path;
use std::process::{Command, Stdio};
use walkdir::WalkDir;

pub fn file_blake3(path: &Path) -> Result<String> {
    let mut file = File::open(path)
        .with_context(|| format!("open {}", path.display()))?;
    let mut hasher = blake3::Hasher::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(hasher.finalize().to_hex().to_string())
}

pub fn bytes_blake3(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

pub fn collect_files(root: &Path) -> Result<Vec<String>> {
    let mut files = Vec::new();
    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_file() {
            let path = entry.path();
            let relative = path
                .strip_prefix(root)?
                .to_string_lossy()
                .replace('\\', "/");
            files.push(relative);
        }
    }
    Ok(files)
}

/// Invoke hdiffz.exe (must be next to the current exe) to produce a binary patch.
pub fn generate_patch(old_file: &Path, new_file: &Path, out_file: &Path) -> Result<bool> {
    if let Some(parent) = out_file.parent() {
        fs::create_dir_all(parent)?;
    }

    let current_exe = std::env::current_exe()?;
    let exe_dir = current_exe.parent().context("failed to get exe dir")?;
    let hdiffz_path = exe_dir.join("hdiffz.exe");

    let status = match Command::new(&hdiffz_path)
        .arg(old_file)
        .arg(new_file)
        .arg(out_file)
        .arg("-c-zstd-21")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // hdiffz.exe not installed — caller falls back to shipping full file.
            return Ok(false);
        }
        Err(e) => return Err(e).with_context(|| format!("execute {}", hdiffz_path.display())),
    };

    Ok(status.success())
}

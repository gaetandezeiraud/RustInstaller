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

/// Write a file atomically: write to a sibling `.tmp` then rename over the
/// target. A crash can leave the `.tmp` behind but never a half-written
/// target, so readers always see either the old or the new complete file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes).with_context(|| format!("write {}", tmp.display()))?;
    if path.exists() {
        let _ = fs::remove_file(path);
    }
    fs::rename(&tmp, path).with_context(|| format!("commit {}", path.display()))?;
    Ok(())
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
            // hdiffz.exe not installed - caller falls back to shipping full file.
            return Ok(false);
        }
        Err(e) => return Err(e).with_context(|| format!("execute {}", hdiffz_path.display())),
    };

    Ok(status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blake3_is_consistent() {
        assert_eq!(bytes_blake3(b"abc"), bytes_blake3(b"abc"));
        assert_ne!(bytes_blake3(b"abc"), bytes_blake3(b"abd"));
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("f");
        fs::write(&p, b"abc").unwrap();
        assert_eq!(file_blake3(&p).unwrap(), bytes_blake3(b"abc"));
    }

    #[test]
    fn write_atomic_overwrites_no_tmp_leftover() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("state.json");
        write_atomic(&p, b"one").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"one");
        write_atomic(&p, b"two").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two");
        let tmp_left = fs::read_dir(d.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.path().extension().map_or(false, |x| x == "tmp"));
        assert!(!tmp_left, "no .tmp should remain");
    }

    #[test]
    fn collect_files_relative_and_normalized() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join("a").join("b")).unwrap();
        fs::write(d.path().join("a").join("b").join("c.txt"), b"x").unwrap();
        fs::write(d.path().join("root.txt"), b"y").unwrap();
        let mut got = collect_files(d.path()).unwrap();
        got.sort();
        assert_eq!(got, vec!["a/b/c.txt".to_string(), "root.txt".to_string()]);
    }
}

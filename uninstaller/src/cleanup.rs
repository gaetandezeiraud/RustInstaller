//! Shared file/registry/shortcut cleanup helpers used by both stages.

use anyhow::{Context, Result};
use common::models::{InstallInfo, Manifest};
use std::fs;
use std::path::{Path, PathBuf};

/// The folder this uninstaller runs from. Since the installer places
/// `uninstall.exe` + metadata in `%LOCALAPPDATA%\<publisher>\Uninstall\<product>`,
/// this is the *data* dir, not the application directory. The real app
/// directory is read from `installer_info.json` (`install_dir`).
pub fn self_dir() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    exe.parent()
        .map(|p| p.to_path_buf())
        .context("locate uninstaller parent dir")
}

/// Remove state files written into the application directory by the installer
/// (`version.json`, `installer_manifest.json`).
pub fn remove_app_state_files(app_dir: &Path) -> usize {
    let mut count = 0;
    for extra in ["version.json", "installer_manifest.json"] {
        let p = app_dir.join(extra);
        if p.exists() && fs::remove_file(&p).is_ok() {
            count += 1;
        }
    }
    count
}

pub fn read_info(install_dir: &Path) -> Result<InstallInfo> {
    let p = install_dir.join("installer_info.json");
    let s = fs::read_to_string(&p)
        .with_context(|| format!("read {} - is this an installed product?", p.display()))?;
    serde_json::from_str(&s).context("parse installer_info.json")
}

pub fn read_manifest(install_dir: &Path) -> Result<Manifest> {
    let p = install_dir.join("installer_manifest.json");
    let s = fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
    serde_json::from_str(&s).context("parse installer_manifest.json")
}

/// Remove every payload file from `manifest`. Returns removed count.
pub fn remove_payload_files(install_dir: &Path, manifest: &Manifest) -> usize {
    let mut count = 0;
    for rel in manifest.files.keys() {
        let p = install_dir.join(rel);
        if p.exists() && fs::remove_file(&p).is_ok() {
            count += 1;
        }
    }
    count
}

pub fn remove_shortcuts(product: &str) {
    for p in common::shortcuts::paths_for(product) {
        if p.exists() {
            let _ = fs::remove_file(&p);
        }
    }
}

/// Recursively remove every empty subdirectory of `install_dir` (bottom-up).
/// Leaves `install_dir` itself in place.
pub fn remove_empty_subdirs(install_dir: &Path) {
    let dirs = walk_dirs(install_dir);
    for d in dirs.into_iter().rev() {
        if d == install_dir {
            continue;
        }
        let _ = fs::remove_dir(&d);
    }
}

fn walk_dirs(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.to_path_buf()];
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                out.push(p.clone());
                stack.push(p);
            }
        }
    }
    out
}

#[cfg(windows)]
pub fn unregister(key: &str) {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RegDeleteTreeW};
    use windows::core::PCWSTR;
    let sub = format!(
        r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{}",
        key
    );
    let wide: Vec<u16> = sub.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(wide.as_ptr()));
    }
}

#[cfg(not(windows))]
pub fn unregister(_key: &str) {}

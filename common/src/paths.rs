//! Per-user install-metadata paths.
//!
//! The uninstaller and its `installer_info.json` / `installer_manifest.json`
//! live OUTSIDE the application folder, so deleting the app folder by hand
//! never orphans the Add/Remove Programs entry (mirrors InstallShield's
//! "Installation Information" folder, but per-user / no admin).
//!
//! Layout: `%LOCALAPPDATA%\<publisher>\Uninstall\<product>\`

use std::path::PathBuf;

/// Folder holding `uninstall.exe` + install metadata for one product.
/// `None` only if `%LOCALAPPDATA%` can't be resolved.
pub fn uninstall_dir(publisher: &str, product: &str) -> Option<PathBuf> {
    let base = dirs::data_local_dir()?;
    Some(
        base.join(sanitize_component(publisher))
            .join("Uninstall")
            .join(sanitize_component(product)),
    )
}

/// Make a string safe to use as a single path component: drop characters
/// illegal on Windows, collapse whitespace, trim trailing dots/spaces, and
/// fall back to a placeholder if nothing usable remains.
pub fn sanitize_component(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_end_matches(['.', ' ']).trim();
    if trimmed.is_empty() {
        "Unknown".to_string()
    } else {
        trimmed.to_string()
    }
}

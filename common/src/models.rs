use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub exe: String,
    pub files: HashMap<String, FileEntry>,
    #[serde(default)]
    pub deleted_files: Vec<String>,
    #[serde(default)]
    pub full_size: u64,
    #[serde(default)]
    pub total_patch_size: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FileEntry {
    pub hash: String,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch: Option<PatchInfo>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PatchInfo {
    pub file: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum PayloadKind {
    Full,
    Patch,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct InstallerPayload {
    pub kind: PayloadKind,
    pub product: String,
    pub from_version: Option<String>,
    pub to_version: String,
    pub min_installer_version: String,
    pub payload_blake3: String,
    pub created_at_unix: i64,
    pub manifest: Manifest,
}

/// What gets embedded in the installer .exe as RCDATA id=2.
///
/// `payload_json` is the exact UTF-8 byte sequence the signature was computed over.
/// The verifier verifies the signature against those bytes, *then* parses
/// `InstallerPayload` from them. This avoids any serializer-determinism trap.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SignedPayload {
    pub payload_json: String,
    pub signature_hex: String,
}

/// Persisted to `<install_dir>/installer_info.json` by the installer.
/// Read by the uninstaller (and any tooling) to locate registry entries
/// and walk the manifest for cleanup.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct InstallInfo {
    pub product: String,
    pub version: String,
    pub install_dir: String,
    pub installed_at_unix: i64,
    /// HKCU subkey under `Software\Microsoft\Windows\CurrentVersion\Uninstall`.
    pub registry_key: String,
    /// Optional path (relative to install_dir) of the product's main exe.
    pub exe: String,
}

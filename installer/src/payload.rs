use anyhow::{Context, Result, bail};
use common::models::{InstallerPayload, SignedPayload};
use ed25519_dalek::{Signature, VerifyingKey, Verifier};

include!(concat!(env!("OUT_DIR"), "/pub_key.rs"));

pub struct LoadedPayload {
    pub payload: InstallerPayload,
    pub zip_bytes: Vec<u8>,
    pub uninstaller_bytes: Vec<u8>,
}

#[cfg(windows)]
pub fn load_and_verify() -> Result<LoadedPayload> {
    let zip_bytes = read_resource(1)?;
    let signed_bytes = read_resource(2)?;
    let uninstaller_bytes = read_resource(3)?;

    let signed: SignedPayload =
        serde_json::from_slice(&signed_bytes).context("parse signed payload JSON")?;

    verify_signature(&signed)?;

    let payload: InstallerPayload =
        serde_json::from_str(&signed.payload_json).context("parse inner payload JSON")?;

    let actual_hash = blake3::hash(&zip_bytes).to_hex().to_string();
    if actual_hash != payload.payload_blake3 {
        bail!(
            "payload hash mismatch: manifest declared {} but RCDATA hashes to {}",
            payload.payload_blake3,
            actual_hash
        );
    }

    check_min_installer_version(&payload.min_installer_version)?;

    Ok(LoadedPayload {
        payload,
        zip_bytes,
        uninstaller_bytes,
    })
}

#[cfg(not(windows))]
pub fn load_and_verify() -> Result<LoadedPayload> {
    bail!("installer is Windows-only")
}

fn verify_signature(signed: &SignedPayload) -> Result<()> {
    if PUB_KEY == [0u8; 32] {
        bail!("installer was built without INSTALLER_PUB_KEY — refusing to install");
    }
    let key = VerifyingKey::from_bytes(&PUB_KEY).context("invalid embedded public key")?;
    let sig_bytes = hex::decode(&signed.signature_hex).context("decode signature hex")?;
    let sig_arr: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("signature must be 64 bytes"))?;
    let sig = Signature::from_bytes(&sig_arr);
    key.verify(signed.payload_json.as_bytes(), &sig)
        .context("Ed25519 signature verification failed")?;
    Ok(())
}

fn check_min_installer_version(required: &str) -> Result<()> {
    let installer = env!("CARGO_PKG_VERSION");
    if compare_semver(installer, required) < 0 {
        bail!(
            "installer version {} is too old (payload requires {})",
            installer,
            required
        );
    }
    Ok(())
}

fn compare_semver(a: &str, b: &str) -> i32 {
    let pa: Vec<u64> = a.split('.').filter_map(|s| s.parse().ok()).collect();
    let pb: Vec<u64> = b.split('.').filter_map(|s| s.parse().ok()).collect();
    for i in 0..pa.len().max(pb.len()) {
        let x = pa.get(i).copied().unwrap_or(0);
        let y = pb.get(i).copied().unwrap_or(0);
        if x < y {
            return -1;
        }
        if x > y {
            return 1;
        }
    }
    0
}

#[cfg(windows)]
fn read_resource(id: u16) -> Result<Vec<u8>> {
    use windows::Win32::System::LibraryLoader::{
        FindResourceW, GetModuleHandleW, LoadResource, LockResource, SizeofResource,
    };
    use windows::core::PCWSTR;

    const RT_RCDATA: u16 = 10;

    unsafe {
        let module = GetModuleHandleW(PCWSTR::null()).context("GetModuleHandle")?;
        let hres = FindResourceW(
            Some(module.into()),
            PCWSTR(id as usize as *const u16),
            PCWSTR(RT_RCDATA as usize as *const u16),
        );
        if hres.is_invalid() {
            bail!("FindResource id={} failed (resource missing?)", id);
        }
        let size = SizeofResource(Some(module.into()), hres);
        if size == 0 {
            bail!("resource id={} has size 0", id);
        }
        let hglobal = LoadResource(Some(module.into()), hres).context("LoadResource")?;
        let ptr = LockResource(hglobal);
        if ptr.is_null() {
            bail!("LockResource id={} returned null", id);
        }
        let slice = std::slice::from_raw_parts(ptr as *const u8, size as usize);
        Ok(slice.to_vec())
    }
}

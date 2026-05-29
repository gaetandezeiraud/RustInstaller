use anyhow::{Context, Result, bail};
use std::path::Path;

#[cfg(windows)]
pub fn embed_resources(
    exe: &Path,
    payload_zip: &[u8],
    signed_json: &[u8],
    uninstaller_exe: &[u8],
) -> Result<()> {
    use windows::Win32::System::LibraryLoader::{
        BeginUpdateResourceW, EndUpdateResourceW, UpdateResourceW,
    };
    use windows::core::PCWSTR;

    let wide: Vec<u16> = exe
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        let h = BeginUpdateResourceW(PCWSTR(wide.as_ptr()), false).context("BeginUpdateResource")?;
        if h.is_invalid() {
            bail!("BeginUpdateResource returned invalid handle for {}", exe.display());
        }

        const RT_RCDATA: u16 = 10;
        const LANG_NEUTRAL: u16 = 0;

        // RCDATA id=1 → payload zip
        UpdateResourceW(
            h,
            PCWSTR(RT_RCDATA as usize as *const u16),
            PCWSTR(1usize as *const u16),
            LANG_NEUTRAL,
            Some(payload_zip.as_ptr() as *const _),
            payload_zip.len() as u32,
        )
        .context("UpdateResource id=1 (payload)")?;

        // RCDATA id=2 → signed manifest JSON
        UpdateResourceW(
            h,
            PCWSTR(RT_RCDATA as usize as *const u16),
            PCWSTR(2usize as *const u16),
            LANG_NEUTRAL,
            Some(signed_json.as_ptr() as *const _),
            signed_json.len() as u32,
        )
        .context("UpdateResource id=2 (signed manifest)")?;

        // RCDATA id=3 → uninstaller binary
        UpdateResourceW(
            h,
            PCWSTR(RT_RCDATA as usize as *const u16),
            PCWSTR(3usize as *const u16),
            LANG_NEUTRAL,
            Some(uninstaller_exe.as_ptr() as *const _),
            uninstaller_exe.len() as u32,
        )
        .context("UpdateResource id=3 (uninstaller)")?;

        EndUpdateResourceW(h, false).context("EndUpdateResource")?;
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn embed_resources(
    _exe: &Path,
    _payload_zip: &[u8],
    _signed_json: &[u8],
    _uninstaller_exe: &[u8],
) -> Result<()> {
    bail!("embed_resources is Windows-only")
}

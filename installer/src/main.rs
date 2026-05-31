#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod extract;
mod install;
mod payload;
#[cfg(windows)]
mod proc;
#[cfg(windows)]
mod ui_minimal;
#[cfg(windows)]
mod ui_win32;

use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// Exit code when a patch is run against the wrong installed version. Distinct
/// from generic failure (1) so a launcher can tell "wrong version, fetch the
/// full installer" from a real error.
const EXIT_VERSION_MISMATCH: i32 = 10;

fn main() {
    if let Err(e) = run() {
        let code = if e.downcast_ref::<extract::VersionMismatch>().is_some() {
            EXIT_VERSION_MISMATCH
        } else {
            1
        };
        report_fatal(&format!("{e:#}"));
        std::process::exit(code);
    }
}

fn run() -> Result<()> {
    let loaded = payload::load_and_verify()?;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let launch = args.iter().any(|a| a == "--launch");
    let translator = common::i18n::Translator::detect(&args);

    // Compact auto-start update UI (app-triggered self-update). Skips license,
    // path picker and buttons - runs immediately, shows icon + progress.
    if let Some(idx) = args.iter().position(|a| a == "--minimal" || a == "/minimal") {
        let path = path_arg(&args, idx)
            .or_else(|| std::env::var("RUSTINSTALLER_PATH").ok())
            .unwrap_or_else(|| default_install_path(&loaded.payload.product).to_string_lossy().into_owned());
        #[cfg(windows)]
        return ui_minimal::run(loaded, PathBuf::from(path), launch, translator);
        #[cfg(not(windows))]
        return run_silent(&loaded, PathBuf::from(path), launch);
    }

    if let Some(idx) = args.iter().position(|a| a == "--silent" || a == "/S") {
        let path = path_arg(&args, idx)
            .or_else(|| std::env::var("RUSTINSTALLER_PATH").ok())
            .unwrap_or_else(|| default_install_path(&loaded.payload.product).to_string_lossy().into_owned());
        return run_silent(&loaded, PathBuf::from(path), launch);
    }
    // Diagnostic: re-hash an installed dir against its local manifest.
    if let Some(idx) = args.iter().position(|a| a == "--verify-install") {
        attach_console();
        let dir = path_arg(&args, idx)
            .or_else(|| std::env::var("RUSTINSTALLER_PATH").ok())
            .unwrap_or_else(|| {
                default_install_path(&loaded.payload.product).to_string_lossy().into_owned()
            });
        return extract::verify_install(&PathBuf::from(dir));
    }

    if args.iter().any(|a| a == "--verify") {
        attach_console();
        let license = match &loaded.payload.license_text {
            Some(t) => format!("custom ({} bytes)", t.len()),
            None => "built-in placeholder".to_string(),
        };
        println!(
            "OK: {} {} -> {} (payload {} bytes verified)\nLicense: {}",
            match loaded.payload.kind {
                common::models::PayloadKind::Full => "FULL",
                common::models::PayloadKind::Patch => "PATCH",
            },
            loaded.payload.from_version.clone().unwrap_or_else(|| "(fresh)".to_string()),
            loaded.payload.to_version,
            loaded.zip_bytes.len(),
            license,
        );
        return Ok(());
    }

    let default_path = default_install_path(&loaded.payload.product);

    #[cfg(windows)]
    ui_win32::run(loaded, default_path, launch, translator)?;

    #[cfg(not(windows))]
    anyhow::bail!("only Windows is supported");

    Ok(())
}

/// Attach to the parent console (if launched from one) so println!/eprintln!
/// from this GUI-subsystem binary is visible. No-op off Windows.
fn attach_console() {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

fn run_silent(
    loaded: &payload::LoadedPayload,
    install_dir: PathBuf,
    launch: bool,
) -> Result<()> {
    attach_console();
    println!(
        "Silent install: {} {} -> {}",
        loaded.payload.product, loaded.payload.to_version, install_dir.display()
    );
    let progress = Arc::new(|done: u64, total: u64, name: &str| {
        if total > 0 {
            let pct = (done * 100) / total;
            eprintln!("[{:>3}%] {}", pct, name);
        }
    }) as Arc<dyn Fn(u64, u64, &str) + Send + Sync>;

    let ctx = extract::InstallCtx {
        install_dir: install_dir.clone(),
        payload: &loaded.payload,
        zip_bytes: &loaded.zip_bytes,
        cancel: Arc::new(AtomicBool::new(false)),
        on_progress: progress,
    };
    extract::install(ctx)?;
    install::finalize(&install_dir, &loaded.payload, &loaded.uninstaller_bytes)?;

    if launch && !loaded.payload.manifest.exe.is_empty() {
        install::launch_product(&install_dir, &loaded.payload.manifest.exe)?;
        println!("Launched {}", loaded.payload.manifest.exe);
    }
    println!("Done.");
    Ok(())
}

/// The value right after a flag, unless it's another flag.
fn path_arg(args: &[String], flag_idx: usize) -> Option<String> {
    args.get(flag_idx + 1)
        .filter(|s| !s.starts_with("--") && !s.starts_with('/'))
        .cloned()
}

fn default_install_path(product: &str) -> PathBuf {
    // Per the spec we install to a user-local path (no admin).
    // LOCALAPPDATA is the natural fit for a non-elevated installer.
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local).join("Programs").join(product);
    }
    if let Some(home) = std::env::var_os("USERPROFILE") {
        return PathBuf::from(home).join(product);
    }
    PathBuf::from(format!(r"C:\Users\Public\{}", product))
}

#[cfg(windows)]
fn report_fatal(msg: &str) {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    use windows::core::PCWSTR;
    let text: Vec<u16> = OsStr::new(msg).encode_wide().chain(std::iter::once(0)).collect();
    let cap: Vec<u16> = OsStr::new("Installer error")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            PCWSTR(cap.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(not(windows))]
fn report_fatal(msg: &str) {
    eprintln!("FATAL: {msg}");
}

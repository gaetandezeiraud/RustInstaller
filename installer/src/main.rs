#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod extract;
mod install;
mod payload;
#[cfg(windows)]
mod ui_win32;
#[cfg(windows)]
mod webview2_detect;

use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

fn main() {
    if let Err(e) = run() {
        report_fatal(&format!("{e:#}"));
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let loaded = payload::load_and_verify()?;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let launch = args.iter().any(|a| a == "--launch");

    if let Some(idx) = args.iter().position(|a| a == "--silent" || a == "/S") {
        let path = args
            .get(idx + 1)
            .filter(|s| !s.starts_with("--") && *s != "/S")
            .cloned()
            .or_else(|| std::env::var("RUSTINSTALLER_PATH").ok())
            .unwrap_or_else(|| default_install_path(&loaded.payload.product).to_string_lossy().into_owned());
        return run_silent(&loaded, PathBuf::from(path), launch);
    }
    if args.iter().any(|a| a == "--verify") {
        #[cfg(windows)]
        let wv2 = webview2_detect::detect();
        #[cfg(not(windows))]
        let wv2: Option<String> = None;
        println!(
            "OK: {} {} -> {} (payload {} bytes verified)\nWebView2 runtime: {}",
            match loaded.payload.kind {
                common::models::PayloadKind::Full => "FULL",
                common::models::PayloadKind::Patch => "PATCH",
            },
            loaded.payload.from_version.clone().unwrap_or_else(|| "(fresh)".to_string()),
            loaded.payload.to_version,
            loaded.zip_bytes.len(),
            wv2.unwrap_or_else(|| "not installed (Win32 UI will be used)".to_string()),
        );
        return Ok(());
    }

    let default_path = default_install_path(&loaded.payload.product);

    #[cfg(windows)]
    {
        // WebView2 UI scaffolding — detection only in V1, fall through to Win32.
        if let Some(_v) = webview2_detect::detect() {
            // TODO V2: launch webview2_ui::run(loaded, default_path, launch).
        }
        ui_win32::run(loaded, default_path, launch)?;
    }

    #[cfg(not(windows))]
    anyhow::bail!("only Windows is supported");

    Ok(())
}

fn run_silent(
    loaded: &payload::LoadedPayload,
    install_dir: PathBuf,
    launch: bool,
) -> Result<()> {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
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

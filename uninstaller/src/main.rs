#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use anyhow::{Context, Result, bail};
use common::models::{InstallInfo, Manifest};
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    if let Err(e) = run() {
        fatal(&format!("{e:#}"));
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let silent = args.iter().any(|a| a == "--silent" || a == "/S");

    let install_dir = locate_install_dir(&args)?;
    let info: InstallInfo = read_info(&install_dir)?;
    let manifest: Manifest = read_manifest(&install_dir)?;

    if !silent && !confirm(&info) {
        return Ok(());
    }

    let removed = remove_files(&install_dir, &manifest);
    let _ = remove_empty_dirs(&install_dir);

    #[cfg(windows)]
    unregister(&info.registry_key);

    schedule_self_and_dir_cleanup(&install_dir)?;

    if !silent {
        notify_done(&info.product, removed);
    } else {
        println!("Uninstalled {} ({} files removed).", info.product, removed);
    }
    Ok(())
}

fn locate_install_dir(args: &[String]) -> Result<PathBuf> {
    if let Some(idx) = args.iter().position(|a| a == "--install-dir") {
        if let Some(p) = args.get(idx + 1) {
            return Ok(PathBuf::from(p));
        }
    }
    let exe = std::env::current_exe()?;
    exe.parent()
        .map(|p| p.to_path_buf())
        .context("locate uninstaller parent dir")
}

fn read_info(install_dir: &Path) -> Result<InstallInfo> {
    let p = install_dir.join("installer_info.json");
    let s = fs::read_to_string(&p)
        .with_context(|| format!("read {} — is this an installed product?", p.display()))?;
    serde_json::from_str(&s).context("parse installer_info.json")
}

fn read_manifest(install_dir: &Path) -> Result<Manifest> {
    let p = install_dir.join("installer_manifest.json");
    let s = fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
    serde_json::from_str(&s).context("parse installer_manifest.json")
}

fn remove_files(install_dir: &Path, manifest: &Manifest) -> usize {
    let mut count = 0;
    for rel in manifest.files.keys() {
        let p = install_dir.join(rel);
        if p.exists() && fs::remove_file(&p).is_ok() {
            count += 1;
        }
    }
    // Local state files.
    for extra in ["version.json", "installer_manifest.json", "installer_info.json"] {
        let p = install_dir.join(extra);
        if p.exists() {
            let _ = fs::remove_file(&p);
            count += 1;
        }
    }
    count
}

fn remove_empty_dirs(install_dir: &Path) -> Result<()> {
    // Walk bottom-up, attempting to remove any empty subdir.
    let entries = walk_dirs(install_dir);
    for d in entries.into_iter().rev() {
        // Don't remove install_dir itself yet — the cmd helper does that.
        if d == install_dir {
            continue;
        }
        let _ = fs::remove_dir(&d);
    }
    Ok(())
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
fn unregister(key: &str) {
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
fn unregister(_key: &str) {}

#[cfg(windows)]
fn schedule_self_and_dir_cleanup(install_dir: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    const DETACHED_PROCESS: u32 = 0x00000008;

    let self_path = std::env::current_exe()?;
    // ping waits ~1s so our process exits first; then del removes uninstaller, then rd removes install_dir.
    let cmdline = format!(
        r#"ping 127.0.0.1 -n 2 > nul & del /q /f "{}" & rd /s /q "{}""#,
        self_path.display(),
        install_dir.display()
    );
    Command::new("cmd")
        .raw_arg("/C")
        .raw_arg(format!("\"{}\"", cmdline))
        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("spawn cleanup cmd")?;
    Ok(())
}

#[cfg(not(windows))]
fn schedule_self_and_dir_cleanup(_install_dir: &Path) -> Result<()> {
    Ok(())
}

#[cfg(windows)]
fn confirm(info: &InstallInfo) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{
        IDYES, MB_ICONQUESTION, MB_YESNO, MessageBoxW,
    };
    use windows::core::PCWSTR;
    let text = format!(
        "Uninstall {} {} from\n{}?",
        info.product, info.version, info.install_dir
    );
    let t: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let c: Vec<u16> = "Uninstall".encode_utf16().chain(std::iter::once(0)).collect();
    let r = unsafe { MessageBoxW(None, PCWSTR(t.as_ptr()), PCWSTR(c.as_ptr()), MB_YESNO | MB_ICONQUESTION) };
    r == IDYES
}

#[cfg(not(windows))]
fn confirm(_info: &InstallInfo) -> bool {
    true
}

#[cfg(windows)]
fn notify_done(product: &str, removed: usize) {
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONINFORMATION, MB_OK, MessageBoxW};
    use windows::core::PCWSTR;
    let text = format!("{} uninstalled ({} files removed).", product, removed);
    let t: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let c: Vec<u16> = "Uninstall".encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        MessageBoxW(None, PCWSTR(t.as_ptr()), PCWSTR(c.as_ptr()), MB_OK | MB_ICONINFORMATION);
    }
}

#[cfg(not(windows))]
fn notify_done(_product: &str, _removed: usize) {}

#[cfg(windows)]
fn fatal(msg: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    use windows::core::PCWSTR;
    let t: Vec<u16> = msg.encode_utf16().chain(std::iter::once(0)).collect();
    let c: Vec<u16> = "Uninstall error".encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        MessageBoxW(None, PCWSTR(t.as_ptr()), PCWSTR(c.as_ptr()), MB_OK | MB_ICONERROR);
    }
}

#[cfg(not(windows))]
fn fatal(msg: &str) {
    eprintln!("FATAL: {msg}");
}

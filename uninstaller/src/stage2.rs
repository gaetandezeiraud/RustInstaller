//! Stage 2: runs from `%TEMP%` after Stage 1 spawned us. Waits for the parent
//! process (Stage 1) to fully exit so the `uninstall.exe` lock is released,
//! then removes the application dir AND the data dir (where uninstall.exe +
//! metadata live), and finally schedules our own removal via
//! `MoveFileExW(MOVEFILE_DELAY_UNTIL_REBOOT)` so Windows cleans us up at next
//! reboot. No `cmd.exe`, no console flash.

use crate::ui::{self, StepCounter, UninstallParams};
use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

pub fn run(
    app_dir: PathBuf,
    data_dir: PathBuf,
    product: String,
    parent_pid: Option<u32>,
) -> Result<()> {
    // Continue stage 1's log file (keyed by stage 1's PID) so the whole
    // uninstall is in one %TEMP% file for support.
    let log_id = parent_pid.unwrap_or_else(std::process::id);
    common::log::init(common::log::log_path_for_stage2(log_id));
    common::log::info(format!(
        "stage2 start: product={} app_dir={} data_dir={} parent_pid={:?}",
        product,
        app_dir.display(),
        data_dir.display(),
        parent_pid
    ));

    let app_dir_w = app_dir.clone();
    let data_dir_w = data_dir.clone();

    let tr = crate::ui::tr();
    let params = UninstallParams {
        title: tr.fmt("uninstall.stage2_title", &[("product", &product)]),
        subtitle: tr.get("uninstall.stage2_subtitle"),
        confirm_text: String::new(), // never shown - we auto-advance to Progress
        worker: Box::new(move |progress: Arc<dyn Fn(u64, u64, &str) + Send + Sync>| {
            let tr = crate::ui::tr();
            // Wait for Stage 1 to exit so file locks release.
            if let Some(pid) = parent_pid {
                wait_for_pid(pid, Duration::from_secs(10));
            }

            let counter = StepCounter::new(5, progress);
            counter.step(&tr.get("uninstall.waiting"));
            counter.step(&tr.get("uninstall.removing_uninstaller"));
            counter.step(&tr.get("uninstall.removing_state2"));

            // Remove the application dir (may be empty / already gone).
            if !app_dir_w.as_os_str().is_empty() {
                remove_dir_retry(&app_dir_w);
            }

            // Remove the data dir (uninstall.exe + metadata). This is where we
            // were launched from; the running copy is the %TEMP% one, so the
            // original is free to delete.
            remove_dir_retry(&data_dir_w);
            // Best-effort: prune now-empty parent folders (Uninstall, publisher).
            if let Some(parent) = data_dir_w.parent() {
                let _ = fs::remove_dir(parent); // "Uninstall"
                if let Some(grand) = parent.parent() {
                    let _ = fs::remove_dir(grand); // "<publisher>"
                }
            }
            counter.step(&tr.get("uninstall.removing_install_dir"));

            // Schedule self for deletion on next reboot (no cmd, no flash).
            schedule_self_delete_on_reboot();
            common::log::info("stage2 complete; self scheduled for delete-on-reboot");
            counter.step(&tr.get("uninstall.done"));

            // Brief pause so user sees the 100% bar.
            thread::sleep(Duration::from_millis(400));
        }),
        auto_start: true,
    };

    let _ = ui::run(params);
    Ok(())
}

/// Recursively remove a directory, retrying through transient locks.
fn remove_dir_retry(dir: &Path) {
    for _ in 0..30 {
        if !dir.exists() {
            return;
        }
        if fs::remove_dir_all(dir).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn wait_for_pid(pid: u32, timeout: Duration) {
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };

    unsafe {
        match OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            Ok(h) if !h.is_invalid() => {
                let ms = timeout.as_millis().min(u32::MAX as u128) as u32;
                let r = WaitForSingleObject(h, ms);
                let _ = CloseHandle(h);
                if r == WAIT_OBJECT_0 {
                    return;
                }
            }
            _ => {}
        }
    }
    // Fallback: short sleep so locks at least likely released.
    thread::sleep(Duration::from_millis(500));
}

fn schedule_self_delete_on_reboot() {
    use windows::Win32::Storage::FileSystem::{MOVEFILE_DELAY_UNTIL_REBOOT, MoveFileExW};
    use windows::core::PCWSTR;

    let Ok(self_exe) = std::env::current_exe() else {
        return;
    };
    let w: Vec<u16> = std::os::windows::ffi::OsStrExt::encode_wide(self_exe.as_os_str())
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let _ = MoveFileExW(PCWSTR(w.as_ptr()), PCWSTR::null(), MOVEFILE_DELAY_UNTIL_REBOOT);
    }
}

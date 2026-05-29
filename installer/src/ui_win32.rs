#![cfg(windows)]

use crate::extract::{InstallCtx, install};
use crate::install as install_mod;
use crate::payload::LoadedPayload;
use anyhow::Result;
use common::models::{InstallerPayload, PayloadKind};
use std::cell::RefCell;
use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontW, CreateSolidBrush, DEFAULT_CHARSET, DEFAULT_PITCH, DeleteObject, FF_DONTCARE,
    FW_NORMAL, FW_SEMIBOLD, GetStockObject, HBRUSH, HFONT, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS,
    CLEARTYPE_QUALITY, SetBkMode, SetTextColor, TRANSPARENT, WHITE_BRUSH,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    BST_CHECKED, BST_UNCHECKED, ICC_PROGRESS_CLASS, INITCOMMONCONTROLSEX, InitCommonControlsEx,
    PBM_SETPOS, PBM_SETRANGE32, PROGRESS_CLASSW,
};

const BM_GETCHECK: u32 = 0x00F0;
const BM_SETCHECK: u32 = 0x00F1;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

const BS_PUSHBUTTON: u32 = 0x0;
const BS_DEFPUSHBUTTON: u32 = 0x1;
const BS_AUTOCHECKBOX: u32 = 0x3;
const ES_READONLY: u32 = 0x0800;
const ES_MULTILINE: u32 = 0x0004;
const ES_LEFT: u32 = 0x0000;
const WS_VSCROLL: WINDOW_STYLE = WINDOW_STYLE(0x0020_0000);

const ID_PATH_EDIT: usize = 1001;
const ID_BROWSE_BTN: usize = 1002;
const ID_INSTALL_BTN: usize = 1003;
const ID_CANCEL_BTN: usize = 1004;
const ID_PROGRESS: usize = 1005;
const ID_STATUS: usize = 1006;
const ID_HEADER: usize = 1007;
const ID_SUBHEADER: usize = 1008;
const ID_PATH_LABEL: usize = 1009;
const ID_CLOSE_BTN: usize = 1010;
const ID_LICENSE_EDIT: usize = 1011;
const ID_ACCEPT_CHK: usize = 1012;
const ID_NEXT_BTN: usize = 1013;
const ID_BACK_BTN: usize = 1014;
const ID_LAUNCH_CHK: usize = 1015;
const ID_BANNER: usize = 1016;

const WM_APP_PROGRESS: u32 = WM_APP + 1;
const WM_APP_DONE: u32 = WM_APP + 2;
const WM_APP_ERROR: u32 = WM_APP + 3;

const WIN_W: i32 = 700;
const WIN_H: i32 = 500;
const BANNER_H: i32 = 72;
const PAD: i32 = 24;

const ACCENT: u32 = 0x00C56C0F; // Windows 11 accent (BGR: orange-blue mix dark)
const ACCENT_LIGHT: u32 = 0x00F3F3F3; // light gray card

const LOREM: &str =
"END USER LICENSE AGREEMENT — SAMPLE\r\n\r\n\
Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod \
tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, \
quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo \
consequat.\r\n\r\n\
Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore \
eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, \
sunt in culpa qui officia deserunt mollit anim id est laborum.\r\n\r\n\
Sed ut perspiciatis unde omnis iste natus error sit voluptatem accusantium \
doloremque laudantium, totam rem aperiam, eaque ipsa quae ab illo inventore \
veritatis et quasi architecto beatae vitae dicta sunt explicabo.\r\n\r\n\
Nemo enim ipsam voluptatem quia voluptas sit aspernatur aut odit aut fugit, \
sed quia consequuntur magni dolores eos qui ratione voluptatem sequi nesciunt.\r\n\r\n\
At vero eos et accusamus et iusto odio dignissimos ducimus qui blanditiis \
praesentium voluptatum deleniti atque corrupti quos dolores et quas molestias \
excepturi sint occaecati cupiditate non provident, similique sunt in culpa \
qui officia deserunt mollitia animi, id est laborum et dolorum fuga.\r\n\r\n\
By clicking 'I accept' you agree to be bound by the terms above.";

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    License,
    Choose,
    Progress,
    Done,
    Error,
}

struct ProgressState {
    done: u64,
    total: u64,
    name: String,
}

struct UiState {
    phase: Phase,
    cancel: Arc<AtomicBool>,
    progress: Arc<std::sync::Mutex<ProgressState>>,
    error_text: String,
    font_normal: HFONT,
    font_bold: HFONT,
    font_header: HFONT,
    banner_brush: HBRUSH,
    card_brush: HBRUSH,
    license_accepted: bool,
    chosen_path: Option<PathBuf>,
}

thread_local! {
    static STATE: RefCell<Option<Rc<RefCell<UiState>>>> = RefCell::new(None);
    static PAYLOAD: RefCell<Option<InstallerPayload>> = RefCell::new(None);
    static UNINSTALLER: RefCell<Option<Vec<u8>>> = RefCell::new(None);
    static LAUNCH_FLAG: RefCell<bool> = RefCell::new(false);
}

pub fn run(loaded: LoadedPayload, default_path: PathBuf, launch_flag: bool) -> Result<()> {
    PAYLOAD.with(|p| *p.borrow_mut() = Some(loaded.payload.clone()));
    UNINSTALLER.with(|u| *u.borrow_mut() = Some(loaded.uninstaller_bytes.clone()));
    LAUNCH_FLAG.with(|l| *l.borrow_mut() = launch_flag);

    unsafe {
        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_PROGRESS_CLASS,
        };
        let _ = InitCommonControlsEx(&icc);

        let hinstance = GetModuleHandleW(PCWSTR::null())?;

        let class_name = w!("RustInstallerWnd");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: WNDCLASS_STYLES(0),
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: HINSTANCE(hinstance.0),
            hIcon: HICON::default(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hbrBackground: HBRUSH(GetStockObject(WHITE_BRUSH).0),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: class_name,
            hIconSm: HICON::default(),
        };
        RegisterClassExW(&wc);

        let font_normal = create_font("Segoe UI", 16, FW_NORMAL.0 as i32);
        let font_bold = create_font("Segoe UI", 16, FW_SEMIBOLD.0 as i32);
        let font_header = create_font("Segoe UI Semibold", 22, FW_SEMIBOLD.0 as i32);
        let banner_brush = CreateSolidBrush(COLORREF(ACCENT_LIGHT));
        let card_brush = CreateSolidBrush(COLORREF(0x00FFFFFF));

        let title = wide(&format!(
            "{} {} — Setup",
            loaded.payload.product, loaded.payload.to_version
        ));

        let state = Rc::new(RefCell::new(UiState {
            phase: Phase::License,
            cancel: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(std::sync::Mutex::new(ProgressState {
                done: 0,
                total: 0,
                name: String::new(),
            })),
            error_text: String::new(),
            font_normal,
            font_bold,
            font_header,
            banner_brush,
            card_brush,
            license_accepted: false,
            chosen_path: Some(default_path.clone()),
        }));
        STATE.with(|s| *s.borrow_mut() = Some(state.clone()));

        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class_name,
            PCWSTR(title.as_ptr()),
            WS_OVERLAPPED | WS_SYSMENU | WS_CAPTION | WS_MINIMIZEBOX,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            WIN_W,
            WIN_H,
            None,
            None,
            Some(HINSTANCE(hinstance.0)),
            None,
        )?;

        center_window(hwnd);
        build_controls(hwnd, &loaded.payload, &default_path);
        apply_phase(hwnd, Phase::License);

        let _ = ShowWindow(hwnd, SW_SHOW);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

unsafe fn create_font(name: &str, height: i32, weight: i32) -> HFONT {
    let name_w = wide(name);
    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            ((DEFAULT_PITCH.0 as u32) | ((FF_DONTCARE.0 as u32) << 4)) as u32,
            PCWSTR(name_w.as_ptr()),
        )
    }
}

unsafe fn apply_font(hwnd: HWND, id: usize, font: HFONT) {
    unsafe {
        let h = GetDlgItem(Some(hwnd), id as i32).unwrap_or_default();
        if !h.is_invalid() {
            SendMessageW(h, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
        }
    }
}

unsafe fn build_controls(hwnd: HWND, payload: &InstallerPayload, default_path: &PathBuf) {
    let hinst = unsafe { GetModuleHandleW(PCWSTR::null()).unwrap_or_default() };
    let hinst = HINSTANCE(hinst.0);

    let header = wide(&format!(
        "Install {} {}",
        payload.product, payload.to_version
    ));
    let sub = match payload.kind {
        PayloadKind::Full => "Welcome — fresh installation".to_string(),
        PayloadKind::Patch => format!(
            "Update {} → {}",
            payload.from_version.clone().unwrap_or_default(),
            payload.to_version
        ),
    };
    let sub_w = wide(&sub);

    // Banner background — a wide empty STATIC; WM_CTLCOLORSTATIC paints it.
    let banner_w: Vec<u16> = "".encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let _ = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            PCWSTR(banner_w.as_ptr()),
            WS_VISIBLE | WS_CHILD,
            0,
            0,
            WIN_W,
            BANNER_H,
            Some(hwnd),
            Some(HMENU(ID_BANNER as *mut _)),
            Some(hinst),
            None,
        );

        let _ = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            PCWSTR(header.as_ptr()),
            WS_VISIBLE | WS_CHILD,
            PAD,
            16,
            WIN_W - PAD * 2,
            28,
            Some(hwnd),
            Some(HMENU(ID_HEADER as *mut _)),
            Some(hinst),
            None,
        );
        let _ = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            PCWSTR(sub_w.as_ptr()),
            WS_VISIBLE | WS_CHILD,
            PAD,
            46,
            WIN_W - PAD * 2,
            20,
            Some(hwnd),
            Some(HMENU(ID_SUBHEADER as *mut _)),
            Some(hinst),
            None,
        );

        // === License page ===
        // Layout (from top):
        //   banner:                0..BANNER_H
        //   license edit:          BANNER_H + PAD .. checkbox_y - 8
        //   accept checkbox:       checkbox_y .. checkbox_y + 22
        //   bottom button row:     WIN_H - 84
        let checkbox_y = WIN_H - 124;
        let license_top = BANNER_H + PAD;
        let license_h = checkbox_y - license_top - 24;
        let lorem_w = wide(LOREM);
        let _ = CreateWindowExW(
            WS_EX_CLIENTEDGE,
            w!("EDIT"),
            PCWSTR(lorem_w.as_ptr()),
            WS_CHILD | WS_CLIPSIBLINGS
                | WS_BORDER
                | WS_VSCROLL
                | WINDOW_STYLE((ES_MULTILINE | ES_READONLY | ES_LEFT) as u32),
            PAD,
            license_top,
            WIN_W - PAD * 2,
            license_h,
            Some(hwnd),
            Some(HMENU(ID_LICENSE_EDIT as *mut _)),
            Some(hinst),
            None,
        );
        let _ = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("BUTTON"),
            w!("I accept the terms of the license agreement"),
            WS_CHILD | WS_CLIPSIBLINGS | WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX),
            PAD,
            checkbox_y,
            WIN_W - PAD * 2,
            22,
            Some(hwnd),
            Some(HMENU(ID_ACCEPT_CHK as *mut _)),
            Some(hinst),
            None,
        );

        // === Choose page ===
        let _ = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!("Install location:"),
            WS_CHILD,
            PAD,
            BANNER_H + PAD + 8,
            WIN_W - PAD * 2,
            20,
            Some(hwnd),
            Some(HMENU(ID_PATH_LABEL as *mut _)),
            Some(hinst),
            None,
        );

        let path_str = wide(&default_path.to_string_lossy());
        let _ = CreateWindowExW(
            WS_EX_CLIENTEDGE,
            w!("EDIT"),
            PCWSTR(path_str.as_ptr()),
            WS_CHILD | WS_BORDER | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
            PAD,
            BANNER_H + PAD + 32,
            WIN_W - PAD * 2 - 120,
            28,
            Some(hwnd),
            Some(HMENU(ID_PATH_EDIT as *mut _)),
            Some(hinst),
            None,
        );
        let _ = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("BUTTON"),
            w!("Browse..."),
            WS_CHILD | WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON),
            WIN_W - PAD - 110,
            BANNER_H + PAD + 32,
            110,
            28,
            Some(hwnd),
            Some(HMENU(ID_BROWSE_BTN as *mut _)),
            Some(hinst),
            None,
        );

        // === Progress page ===
        let _ = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PROGRESS_CLASSW,
            PCWSTR::null(),
            WS_CHILD,
            PAD,
            BANNER_H + PAD + 16,
            WIN_W - PAD * 2,
            22,
            Some(hwnd),
            Some(HMENU(ID_PROGRESS as *mut _)),
            Some(hinst),
            None,
        );
        let _ = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!(""),
            WS_CHILD,
            PAD,
            BANNER_H + PAD + 48,
            WIN_W - PAD * 2,
            48,
            Some(hwnd),
            Some(HMENU(ID_STATUS as *mut _)),
            Some(hinst),
            None,
        );

        // === Done page extras ===
        let _ = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("BUTTON"),
            w!("Run program now"),
            WS_CHILD | WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX),
            PAD,
            WIN_H - 124,
            WIN_W - PAD * 2,
            22,
            Some(hwnd),
            Some(HMENU(ID_LAUNCH_CHK as *mut _)),
            Some(hinst),
            None,
        );

        // === Bottom buttons ===
        let btn_y = WIN_H - 84;
        let _ = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("BUTTON"),
            w!("< Back"),
            WS_CHILD | WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON),
            PAD,
            btn_y,
            100,
            32,
            Some(hwnd),
            Some(HMENU(ID_BACK_BTN as *mut _)),
            Some(hinst),
            None,
        );
        let _ = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("BUTTON"),
            w!("Next >"),
            WS_CHILD | WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON),
            WIN_W - PAD - 240,
            btn_y,
            110,
            32,
            Some(hwnd),
            Some(HMENU(ID_NEXT_BTN as *mut _)),
            Some(hinst),
            None,
        );
        let _ = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("BUTTON"),
            w!("Install"),
            WS_CHILD | WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON),
            WIN_W - PAD - 240,
            btn_y,
            110,
            32,
            Some(hwnd),
            Some(HMENU(ID_INSTALL_BTN as *mut _)),
            Some(hinst),
            None,
        );
        let _ = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("BUTTON"),
            w!("Cancel"),
            WS_CHILD | WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON),
            WIN_W - PAD - 120,
            btn_y,
            120,
            32,
            Some(hwnd),
            Some(HMENU(ID_CANCEL_BTN as *mut _)),
            Some(hinst),
            None,
        );
        let _ = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("BUTTON"),
            w!("Finish"),
            WS_CHILD | WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON),
            WIN_W - PAD - 120,
            btn_y,
            120,
            32,
            Some(hwnd),
            Some(HMENU(ID_CLOSE_BTN as *mut _)),
            Some(hinst),
            None,
        );
    }

    // Apply fonts.
    STATE.with(|s| {
        let Some(st) = s.borrow().as_ref().cloned() else { return; };
        let st = st.borrow();
        unsafe {
            apply_font(hwnd, ID_HEADER, st.font_header);
            apply_font(hwnd, ID_SUBHEADER, st.font_normal);
            for id in [
                ID_PATH_LABEL, ID_PATH_EDIT, ID_BROWSE_BTN, ID_INSTALL_BTN, ID_CANCEL_BTN,
                ID_PROGRESS, ID_STATUS, ID_CLOSE_BTN, ID_LICENSE_EDIT, ID_ACCEPT_CHK,
                ID_NEXT_BTN, ID_BACK_BTN, ID_LAUNCH_CHK,
            ] {
                apply_font(hwnd, id, st.font_normal);
            }
        }
    });
}

unsafe fn apply_phase(hwnd: HWND, phase: Phase) {
    STATE.with(|s| {
        if let Some(state) = s.borrow().as_ref() {
            state.borrow_mut().phase = phase;
        }
    });

    let show = |id: usize, vis: bool| unsafe {
        let h = GetDlgItem(Some(hwnd), id as i32).unwrap_or_default();
        let _ = ShowWindow(h, if vis { SW_SHOW } else { SW_HIDE });
    };

    // Header/banner always visible.
    show(ID_BANNER, true);
    show(ID_HEADER, true);
    show(ID_SUBHEADER, true);

    let (lic, choose, prog, _done) = match phase {
        Phase::License => (true, false, false, false),
        Phase::Choose => (false, true, false, false),
        Phase::Progress => (false, false, true, false),
        Phase::Done => (false, false, true, true),
        Phase::Error => (false, false, false, true),
    };

    show(ID_LICENSE_EDIT, lic);
    show(ID_ACCEPT_CHK, lic);

    show(ID_PATH_LABEL, choose);
    show(ID_PATH_EDIT, choose);
    show(ID_BROWSE_BTN, choose);

    show(ID_PROGRESS, prog);
    show(ID_STATUS, phase == Phase::Progress || phase == Phase::Done || phase == Phase::Error);
    show(ID_LAUNCH_CHK, phase == Phase::Done);

    show(ID_BACK_BTN, phase == Phase::Choose);
    show(ID_NEXT_BTN, phase == Phase::License);
    show(ID_INSTALL_BTN, phase == Phase::Choose);
    show(ID_CANCEL_BTN, phase == Phase::License || phase == Phase::Choose || phase == Phase::Progress);
    show(ID_CLOSE_BTN, phase == Phase::Done || phase == Phase::Error);

    match phase {
        Phase::Done => unsafe {
            set_window_text(
                GetDlgItem(Some(hwnd), ID_STATUS as i32).unwrap_or_default(),
                "Installation complete.",
            );
            // Default the launch checkbox to checked if launch flag set OR exe known.
            let default_checked = LAUNCH_FLAG.with(|l| *l.borrow())
                || PAYLOAD.with(|p| p.borrow().as_ref().map(|p| !p.manifest.exe.is_empty()).unwrap_or(false));
            let h = GetDlgItem(Some(hwnd), ID_LAUNCH_CHK as i32).unwrap_or_default();
            SendMessageW(
                h,
                BM_SETCHECK,
                Some(WPARAM(if default_checked { BST_CHECKED.0 as usize } else { BST_UNCHECKED.0 as usize })),
                Some(LPARAM(0)),
            );
        },
        _ => {}
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CTLCOLORSTATIC => unsafe {
            let hdc = windows::Win32::Graphics::Gdi::HDC(wparam.0 as *mut core::ffi::c_void);
            let ctrl = HWND(lparam.0 as *mut _);
            let banner = GetDlgItem(Some(hwnd), ID_BANNER as i32).unwrap_or_default();
            let header = GetDlgItem(Some(hwnd), ID_HEADER as i32).unwrap_or_default();
            let sub = GetDlgItem(Some(hwnd), ID_SUBHEADER as i32).unwrap_or_default();
            let _ = SetBkMode(hdc, TRANSPARENT);
            if ctrl == banner || ctrl == header || ctrl == sub {
                SetTextColor(hdc, COLORREF(0x00333333));
                return LRESULT(
                    STATE.with(|s| {
                        s.borrow()
                            .as_ref()
                            .map(|st| st.borrow().banner_brush.0 as isize)
                            .unwrap_or(0)
                    }),
                );
            }
            return LRESULT(
                STATE.with(|s| {
                    s.borrow()
                        .as_ref()
                        .map(|st| st.borrow().card_brush.0 as isize)
                        .unwrap_or(0)
                }),
            );
        },
        WM_COMMAND => unsafe {
            let id = (wparam.0 & 0xFFFF) as usize;
            match id {
                ID_BROWSE_BTN => on_browse(hwnd),
                ID_INSTALL_BTN => on_install(hwnd),
                ID_CANCEL_BTN => on_cancel(hwnd),
                ID_NEXT_BTN => on_next(hwnd),
                ID_BACK_BTN => on_back(hwnd),
                ID_ACCEPT_CHK => on_accept_toggle(hwnd),
                ID_CLOSE_BTN => on_finish(hwnd),
                _ => {}
            }
            LRESULT(0)
        },
        m if m == WM_APP_PROGRESS => unsafe {
            update_progress(hwnd);
            LRESULT(0)
        },
        m if m == WM_APP_DONE => unsafe {
            apply_phase(hwnd, Phase::Done);
            LRESULT(0)
        },
        m if m == WM_APP_ERROR => unsafe {
            STATE.with(|s| {
                if let Some(state) = s.borrow().as_ref() {
                    let text = state.borrow().error_text.clone();
                    let label = GetDlgItem(Some(hwnd), ID_STATUS as i32).unwrap_or_default();
                    set_window_text(label, &format!("Error: {}", text));
                }
            });
            apply_phase(hwnd, Phase::Error);
            LRESULT(0)
        },
        WM_CLOSE => unsafe {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        },
        WM_DESTROY => unsafe {
            // Cleanup GDI resources.
            STATE.with(|s| {
                if let Some(state) = s.borrow().as_ref() {
                    let st = state.borrow();
                    let _ = DeleteObject(st.font_normal.into());
                    let _ = DeleteObject(st.font_bold.into());
                    let _ = DeleteObject(st.font_header.into());
                    let _ = DeleteObject(st.banner_brush.into());
                    let _ = DeleteObject(st.card_brush.into());
                }
            });
            PostQuitMessage(0);
            LRESULT(0)
        },
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

unsafe fn on_next(hwnd: HWND) {
    let phase = STATE.with(|s| s.borrow().as_ref().map(|st| st.borrow().phase).unwrap_or(Phase::License));
    if phase == Phase::License {
        let accepted = STATE.with(|s| s.borrow().as_ref().map(|st| st.borrow().license_accepted).unwrap_or(false));
        if !accepted {
            unsafe {
                message_box(hwnd, "You must accept the license to continue.", MB_ICONWARNING);
            }
            return;
        }
        unsafe { apply_phase(hwnd, Phase::Choose) };
    }
}

unsafe fn on_back(hwnd: HWND) {
    let phase = STATE.with(|s| s.borrow().as_ref().map(|st| st.borrow().phase).unwrap_or(Phase::License));
    if phase == Phase::Choose {
        unsafe { apply_phase(hwnd, Phase::License) };
    }
}

unsafe fn on_accept_toggle(hwnd: HWND) {
    let h = unsafe { GetDlgItem(Some(hwnd), ID_ACCEPT_CHK as i32).unwrap_or_default() };
    let state = unsafe { SendMessageW(h, BM_GETCHECK, None, None) };
    let checked = state.0 as u32 == BST_CHECKED.0;
    STATE.with(|s| {
        if let Some(st) = s.borrow().as_ref() {
            st.borrow_mut().license_accepted = checked;
        }
    });
}

unsafe fn on_browse(hwnd: HWND) {
    unsafe {
        if let Some(picked) = pick_folder_com(hwnd) {
            let edit = GetDlgItem(Some(hwnd), ID_PATH_EDIT as i32).unwrap_or_default();
            set_window_text(edit, &picked);
        }
    }
}

unsafe fn pick_folder_com(hwnd: HWND) -> Option<String> {
    use windows::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CLSCTX_INPROC_SERVER, CoCreateInstance,
        CoInitializeEx, CoUninitialize,
    };
    use windows::Win32::UI::Shell::{
        FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, IShellItem,
        SIGDN_FILESYSPATH,
    };

    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        let dialog: IFileOpenDialog =
            match CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) {
                Ok(d) => d,
                Err(_) => {
                    CoUninitialize();
                    return None;
                }
            };
        let _ = dialog.SetOptions(FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM);
        let _ = dialog.Show(Some(hwnd));
        let item_res: windows::core::Result<IShellItem> = dialog.GetResult();
        let result = match item_res {
            Ok(item) => match item.GetDisplayName(SIGDN_FILESYSPATH) {
                Ok(pwstr) => {
                    let s = pwstr.to_string().ok();
                    windows::Win32::System::Com::CoTaskMemFree(Some(pwstr.0 as *const _));
                    s
                }
                Err(_) => None,
            },
            Err(_) => None,
        };
        CoUninitialize();
        result
    }
}

unsafe fn on_install(hwnd: HWND) {
    let edit = unsafe { GetDlgItem(Some(hwnd), ID_PATH_EDIT as i32).unwrap_or_default() };
    let path = unsafe { get_window_text(edit) };
    if path.trim().is_empty() {
        unsafe { message_box(hwnd, "Choose an install folder first.", MB_ICONWARNING) };
        return;
    }
    let pb = PathBuf::from(path.trim());

    STATE.with(|s| {
        if let Some(st) = s.borrow().as_ref() {
            st.borrow_mut().chosen_path = Some(pb.clone());
        }
    });

    unsafe { apply_phase(hwnd, Phase::Progress) };

    let cancel = STATE.with(|s| s.borrow().as_ref().map(|st| st.borrow().cancel.clone()).unwrap());
    let progress_shared = STATE.with(|s| s.borrow().as_ref().map(|st| st.borrow().progress.clone()).unwrap());
    let hwnd_isize = hwnd.0 as isize;

    thread::spawn(move || {
        let loaded = match crate::payload::load_and_verify() {
            Ok(l) => l,
            Err(e) => {
                push_error(HWND(hwnd_isize as *mut _), &format!("{e}"));
                return;
            }
        };
        let progress_cb: Arc<dyn Fn(u64, u64, &str) + Send + Sync> = {
            let progress_shared = progress_shared.clone();
            Arc::new(move |done, total, name| {
                if let Ok(mut guard) = progress_shared.lock() {
                    guard.done = done;
                    guard.total = total;
                    guard.name = name.to_string();
                }
                let _ = unsafe {
                    PostMessageW(
                        Some(HWND(hwnd_isize as *mut _)),
                        WM_APP_PROGRESS,
                        WPARAM(0),
                        LPARAM(0),
                    )
                };
            })
        };
        let ctx = InstallCtx {
            install_dir: pb.clone(),
            payload: &loaded.payload,
            zip_bytes: &loaded.zip_bytes,
            cancel: cancel.clone(),
            on_progress: progress_cb,
        };
        if let Err(e) = install(ctx) {
            push_error(HWND(hwnd_isize as *mut _), &format!("{e}"));
            return;
        }
        if let Err(e) =
            install_mod::finalize(&pb, &loaded.payload, &loaded.uninstaller_bytes)
        {
            push_error(HWND(hwnd_isize as *mut _), &format!("finalize: {e}"));
            return;
        }
        let _ = unsafe {
            PostMessageW(
                Some(HWND(hwnd_isize as *mut _)),
                WM_APP_DONE,
                WPARAM(0),
                LPARAM(0),
            )
        };
    });
}

unsafe fn on_cancel(hwnd: HWND) {
    let phase = STATE.with(|s| s.borrow().as_ref().map(|st| st.borrow().phase).unwrap_or(Phase::License));
    match phase {
        Phase::License | Phase::Choose => {
            let _ = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        }
        Phase::Progress => {
            STATE.with(|s| {
                if let Some(state) = s.borrow().as_ref() {
                    state.borrow().cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            });
        }
        _ => {}
    }
}

unsafe fn on_finish(hwnd: HWND) {
    // If "Run now" is checked, launch the product before closing.
    let h = unsafe { GetDlgItem(Some(hwnd), ID_LAUNCH_CHK as i32).unwrap_or_default() };
    let checked = unsafe { SendMessageW(h, BM_GETCHECK, None, None) }.0 as u32 == BST_CHECKED.0;
    if checked {
        let path = STATE.with(|s| s.borrow().as_ref().and_then(|st| st.borrow().chosen_path.clone()));
        let exe = PAYLOAD.with(|p| p.borrow().as_ref().map(|p| p.manifest.exe.clone()).unwrap_or_default());
        if let Some(pb) = path {
            let _ = crate::install::launch_product(&pb, &exe);
        }
    }
    let _ = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
}

unsafe fn update_progress(hwnd: HWND) {
    STATE.with(|s| {
        let Some(state) = s.borrow().as_ref().cloned() else { return; };
        let st = state.borrow();
        let bar = unsafe { GetDlgItem(Some(hwnd), ID_PROGRESS as i32).unwrap_or_default() };
        let label = unsafe { GetDlgItem(Some(hwnd), ID_STATUS as i32).unwrap_or_default() };

        let (done, total, name) = {
            if let Ok(guard) = st.progress.lock() {
                (guard.done, guard.total, guard.name.clone())
            } else {
                (0, 0, String::new())
            }
        };

        let total_val = if total == 0 { 1 } else { total };
        let scaled = ((done as u128 * 10000u128) / total_val as u128) as i32;
        unsafe {
            SendMessageW(bar, PBM_SETRANGE32, Some(WPARAM(0)), Some(LPARAM(10000)));
            SendMessageW(bar, PBM_SETPOS, Some(WPARAM(scaled as usize)), Some(LPARAM(0)));
        }
        let pct = scaled / 100;
        let txt = if total > 0 {
            format!(
                "{}%   ({} / {} bytes)\n{}",
                pct,
                done,
                total,
                name
            )
        } else {
            name
        };
        unsafe { set_window_text(label, &txt) };
    });
}

fn push_error(hwnd: HWND, msg: &str) {
    STATE.with(|s| {
        if let Some(state) = s.borrow().as_ref() {
            state.borrow_mut().error_text = msg.to_string();
        }
    });
    let _ = unsafe { PostMessageW(Some(hwnd), WM_APP_ERROR, WPARAM(0), LPARAM(0)) };
}

unsafe fn center_window(hwnd: HWND) {
    let mut rect = RECT::default();
    unsafe { let _ = GetWindowRect(hwnd, &mut rect); };
    let w = rect.right - rect.left;
    let h = rect.bottom - rect.top;
    let sw = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let sh = unsafe { GetSystemMetrics(SM_CYSCREEN) };
    let x = (sw - w) / 2;
    let y = (sh - h) / 2;
    unsafe {
        let _ = SetWindowPos(hwnd, None, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER);
    }
}

fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

unsafe fn set_window_text(hwnd: HWND, s: &str) {
    let w = wide(s);
    unsafe { let _ = SetWindowTextW(hwnd, PCWSTR(w.as_ptr())); };
}

unsafe fn get_window_text(hwnd: HWND) -> String {
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; (len + 1) as usize];
    unsafe { GetWindowTextW(hwnd, &mut buf) };
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    OsString::from_wide(&buf[..end]).to_string_lossy().into_owned()
}

unsafe fn message_box(hwnd: HWND, text: &str, style: MESSAGEBOX_STYLE) {
    let t = wide(text);
    let c = wide("Installer");
    unsafe {
        MessageBoxW(Some(hwnd), PCWSTR(t.as_ptr()), PCWSTR(c.as_ptr()), style);
    }
}

#![windows_subsystem = "windows"]
#![allow(non_snake_case)] // Keep the executable name consistent with the product name.

mod backend;

use std::ffi::c_void;
use std::ptr::{null, null_mut};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;

type Handle = *mut c_void;
type Hwnd = Handle;
type Wparam = usize;
type Lparam = isize;
type Lresult = isize;

#[repr(C)]
struct WndClass {
    style: u32,
    wndproc: Option<unsafe extern "system" fn(Hwnd, u32, Wparam, Lparam) -> Lresult>,
    cls_extra: i32,
    wnd_extra: i32,
    instance: Handle,
    icon: Handle,
    cursor: Handle,
    background: Handle,
    menu_name: *const u16,
    class_name: *const u16,
}
#[repr(C)]
#[derive(Default)]
struct Rect { left: i32, top: i32, right: i32, bottom: i32 }
#[repr(C)]
#[derive(Clone, Copy)]
struct LogFont {
    height: i32, width: i32, escapement: i32, orientation: i32, weight: i32,
    italic: u8, underline: u8, strike_out: u8, charset: u8,
    out_precision: u8, clip_precision: u8, quality: u8, pitch_and_family: u8,
    face_name: [u16; 32],
}
#[repr(C)]
struct NonClientMetrics {
    size: u32,
    border_width: i32, scroll_width: i32, scroll_height: i32, caption_width: i32, caption_height: i32,
    caption_font: LogFont,
    small_caption_width: i32, small_caption_height: i32, small_caption_font: LogFont,
    menu_width: i32, menu_height: i32, menu_font: LogFont, status_font: LogFont, message_font: LogFont,
    padded_border_width: i32,
}
#[repr(C)]
struct Point { x: i32, y: i32 }
#[repr(C)]
struct MinMaxInfo { reserved: Point, max_size: Point, max_position: Point, min_track_size: Point, max_track_size: Point }
#[repr(C)]
struct Msg { hwnd: Hwnd, message: u32, wparam: Wparam, lparam: Lparam, time: u32, pt: Point, private: u32 }

#[link(name = "user32")]
extern "system" {
    fn RegisterClassW(class: *const WndClass) -> u16;
    fn CreateWindowExW(ex: u32, class: *const u16, title: *const u16, style: u32, x: i32, y: i32, w: i32, h: i32, parent: Hwnd, menu: Handle, instance: Handle, param: *mut c_void) -> Hwnd;
    fn DefWindowProcW(hwnd: Hwnd, msg: u32, wp: Wparam, lp: Lparam) -> Lresult;
    fn PostQuitMessage(code: i32);
    fn GetMessageW(msg: *mut Msg, hwnd: Hwnd, min: u32, max: u32) -> i32;
    fn TranslateMessage(msg: *const Msg) -> i32;
    fn DispatchMessageW(msg: *const Msg) -> Lresult;
    fn IsDialogMessageW(hwnd: Hwnd, msg: *mut Msg) -> i32;
    fn ShowWindow(hwnd: Hwnd, show: i32) -> i32;
    fn UpdateWindow(hwnd: Hwnd) -> i32;
    fn LoadCursorW(instance: Handle, name: *const u16) -> Handle;
    fn LoadIconW(instance: Handle, name: *const u16) -> Handle;
    fn LoadImageW(instance: Handle, name: *const u16, image_type: u32, width: i32, height: i32, flags: u32) -> Handle;
    fn DestroyIcon(icon: Handle) -> i32;
    fn GetSystemMetrics(index: i32) -> i32;
    fn SendMessageW(hwnd: Hwnd, msg: u32, wp: Wparam, lp: Lparam) -> Lresult;
    fn SetWindowTextW(hwnd: Hwnd, text: *const u16) -> i32;
    fn GetWindowTextLengthW(hwnd: Hwnd) -> i32;
    fn GetWindowTextW(hwnd: Hwnd, text: *mut u16, max: i32) -> i32;
    fn EnableWindow(hwnd: Hwnd, enable: i32) -> i32;
    fn MoveWindow(hwnd: Hwnd, x: i32, y: i32, w: i32, h: i32, repaint: i32) -> i32;
    fn GetClientRect(hwnd: Hwnd, rect: *mut Rect) -> i32;
    fn GetDC(hwnd: Hwnd) -> Handle;
    fn ReleaseDC(hwnd: Hwnd, dc: Handle) -> i32;
    fn SetWindowPos(hwnd: Hwnd, after: Hwnd, x: i32, y: i32, w: i32, h: i32, flags: u32) -> i32;
    fn SystemParametersInfoW(action: u32, param: u32, data: *mut c_void, flags: u32) -> i32;
    fn SetTimer(hwnd: Hwnd, id: usize, ms: u32, proc: *mut c_void) -> usize;
    fn SetProcessDPIAware() -> i32;
    fn SetFocus(hwnd: Hwnd) -> Hwnd;
}
#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(name: *const u16) -> Handle;
    fn GetProcAddress(module: Handle, name: *const u8) -> *const c_void;
}
#[link(name = "gdi32")]
extern "system" {
    fn GetStockObject(index: i32) -> Handle;
    fn SetBkColor(dc: Handle, color: u32) -> u32;
    fn SetTextColor(dc: Handle, color: u32) -> u32;
    fn GetDeviceCaps(dc: Handle, index: i32) -> i32;
    fn CreateFontIndirectW(font: *const LogFont) -> Handle;
    fn DeleteObject(object: Handle) -> i32;
}

const WS_CHILD: u32 = 0x40000000;
const WS_VISIBLE: u32 = 0x10000000;
const WS_VSCROLL: u32 = 0x00200000;
const WS_TABSTOP: u32 = 0x00010000;
const ES_MULTILINE: u32 = 4;
const ES_AUTOVSCROLL: u32 = 0x40;
const ES_AUTOHSCROLL: u32 = 0x80;
const ES_WANTRETURN: u32 = 0x1000;
const WM_COMMAND: u32 = 0x0111;
const WM_SIZE: u32 = 5;
const WM_SETICON: u32 = 0x0080;
const WM_DPICHANGED: u32 = 0x02e0;
const WM_GETMINMAXINFO: u32 = 0x0024;
const WM_TIMER: u32 = 0x0113;
const WM_DESTROY: u32 = 2;
const WM_SETFONT: u32 = 0x0030;
const WM_CTLCOLOREDIT: u32 = 0x0133;
const WM_CTLCOLORSTATIC: u32 = 0x0138;
const WM_COPY: u32 = 0x0301;
const EM_SETSEL: u32 = 0x00b1;
const EM_SETLIMITTEXT: u32 = 0x00c5;
const IDC_GENERATE: usize = 101;
const IDC_COPY: usize = 102;
const IDC_CLEAR_INPUT: usize = 104;
const IDC_ENGINE_UPDATE: usize = 105;
const IDC_MODEL_DOWNLOAD: usize = 106;

enum UiEvent {
    Status(String), Ready(Result<bool, String>), ModelStored, ModelDownloadFailed(String),
    Done(Result<String, String>), UpdateDone(Result<UpdateOutcome, String>), Warning(String), ActiveRuntimeChanged,
    RuntimeMissing, RuntimeReady(Option<u32>),
    RuntimeSetupFailed { error: String, session_active: bool, during_download: bool },
}
enum WorkerCommand {
    Generate { email: String, guidance: String, affiliation: String, recipient: String, sender: String },
    DownloadModel,
    CheckAndUpdate,
    DownloadRuntime,
}
#[derive(Debug, PartialEq, Eq)]
enum UpdateOutcome {
    Current(u32), Applied { build: u32, warning: Option<String> }, Deferred { installed: u32, build: u32 },
}
#[derive(Default)]
struct VersionTracker { epoch: u64, installed: Option<u32> }
impl VersionTracker {
    fn begin(&mut self) -> u64 { self.epoch = self.epoch.wrapping_add(1); self.epoch }
    fn label(&self) -> String {
        self.installed.map(|build| format!("llama.cpp b{build}")).unwrap_or_else(|| "llama.cpp".into())
    }
    fn record(&mut self, build: u32) {
        self.begin();
        self.installed = Some(build);
    }
    fn finish(&mut self, epoch: u64, result: Result<u32, String>) -> Option<String> {
        if epoch != self.epoch { return None; }
        match result {
            Ok(build) => { self.installed = Some(build); Some(self.label()) }
            Err(_) => { self.installed = None; Some("llama.cpp（バージョンを確認できません）".into()) }
        }
    }
}
struct State {
    model_name: Hwnd, model_download: Hwnd,
    engine_info: Hwnd, engine_update: Hwnd,
    input_label: Hwnd, input: Hwnd, guidance_label: Hwnd, guidance: Hwnd,
    affiliation_label: Hwnd, affiliation: Hwnd, recipient_label: Hwnd, recipient: Hwnd, sender_label: Hwnd, sender: Hwnd,
    generate: Hwnd, copy: Hwnd, clear_input: Hwnd, output_label: Hwnd, output: Hwnd, status: Hwnd,
    font: Handle, icon_small: Handle, icon_large: Handle,
    rx: Receiver<UiEvent>, tx: Sender<WorkerCommand>,
    version_tx: Sender<(u64, Result<u32, String>)>, version_rx: Receiver<(u64, Result<u32, String>)>, version: VersionTracker,
    loading: bool, busy: bool, worker_alive: bool, model_present: bool, downloading: bool, no_new_version: bool,
    runtime_ready: bool, runtime_needs_repair: bool,
}
static mut STATE: *mut State = null_mut();

fn wide(s: &str) -> Vec<u16> { s.encode_utf16().chain(Some(0)).collect() }

// Resolve newer DPI APIs at runtime so older Windows 10 versions can fall
// back to system-DPI awareness instead of failing to start.
unsafe fn user32_proc(name: &[u8]) -> *const c_void {
    let user32 = GetModuleHandleW(wide("user32.dll").as_ptr());
    if user32.is_null() { null() } else { GetProcAddress(user32, name.as_ptr()) }
}

unsafe fn enable_dpi_awareness() {
    let proc = user32_proc(b"SetProcessDpiAwarenessContext\0");
    if !proc.is_null() {
        let set: unsafe extern "system" fn(Handle) -> i32 = std::mem::transmute(proc);
        // DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2
        if set((-4isize) as Handle) != 0 { return; }
    }
    SetProcessDPIAware();
}

unsafe fn system_dpi() -> u32 {
    let dc = GetDC(null_mut());
    if dc.is_null() { return 96; }
    let dpi = GetDeviceCaps(dc, 88); // LOGPIXELSX
    ReleaseDC(null_mut(), dc);
    if dpi > 0 { dpi as u32 } else { 96 }
}

unsafe fn window_dpi(hwnd: Hwnd) -> u32 {
    let proc = user32_proc(b"GetDpiForWindow\0");
    if !proc.is_null() {
        let get: unsafe extern "system" fn(Hwnd) -> u32 = std::mem::transmute(proc);
        let dpi = get(hwnd);
        if dpi != 0 { return dpi; }
    }
    system_dpi()
}

fn scale_for_dpi(value: i32, dpi: u32) -> i32 {
    ((i64::from(value) * i64::from(dpi.max(1)) + 48) / 96).clamp(0, i64::from(i32::MAX)) as i32
}

fn unscale_for_dpi(value: i32, dpi: u32) -> i32 {
    let dpi = i64::from(dpi.max(1));
    ((i64::from(value) * 96 + dpi / 2) / dpi).clamp(0, i64::from(i32::MAX)) as i32
}

unsafe fn message_font(dpi: u32) -> Handle {
    let mut metrics: NonClientMetrics = std::mem::zeroed();
    metrics.size = std::mem::size_of::<NonClientMetrics>() as u32;
    let data = &mut metrics as *mut NonClientMetrics as *mut c_void;
    let proc = user32_proc(b"SystemParametersInfoForDpi\0");
    let ok = if proc.is_null() {
        SystemParametersInfoW(0x0029, metrics.size, data, 0) // SPI_GETNONCLIENTMETRICS
    } else {
        let get: unsafe extern "system" fn(u32, u32, *mut c_void, u32, u32) -> i32 = std::mem::transmute(proc);
        get(0x0029, metrics.size, data, 0, dpi)
    };
    if ok == 0 { null_mut() } else { CreateFontIndirectW(&metrics.message_font) }
}

fn controls(s: &State) -> [Hwnd; 20] {
    [s.model_name, s.model_download, s.engine_info, s.engine_update, s.input_label, s.clear_input, s.input,
        s.affiliation_label, s.recipient_label, s.sender_label, s.affiliation, s.recipient, s.sender,
        s.guidance_label, s.guidance, s.generate, s.copy, s.output_label, s.output, s.status]
}

unsafe fn update_font(s: &mut State, dpi: u32) {
    let font = message_font(dpi);
    let selected = if font.is_null() { GetStockObject(17) } else { font };
    for hwnd in controls(s) { SendMessageW(hwnd, WM_SETFONT, selected as usize, 1); }
    let previous = std::mem::replace(&mut s.font, font);
    if !previous.is_null() { DeleteObject(previous); }
}

unsafe fn icon_metric(index: i32, dpi: u32) -> i32 {
    let proc = user32_proc(b"GetSystemMetricsForDpi\0");
    if !proc.is_null() {
        let get: unsafe extern "system" fn(i32, u32) -> i32 = std::mem::transmute(proc);
        let size = get(index, dpi);
        if size > 0 { return size; }
    }
    GetSystemMetrics(index).max(1)
}

unsafe fn replace_window_icon(hwnd: Hwnd, old: &mut Handle, kind: usize, width: i32, height: i32) {
    let icon = LoadImageW(GetModuleHandleW(null()), 1usize as *const u16, 1, width, height, 0); // IMAGE_ICON
    if icon.is_null() { return; }
    SendMessageW(hwnd, WM_SETICON, kind, icon as Lparam);
    let previous = std::mem::replace(old, icon);
    if !previous.is_null() { DestroyIcon(previous); }
}

unsafe fn update_window_icons(hwnd: Hwnd, s: &mut State, dpi: u32) {
    replace_window_icon(hwnd, &mut s.icon_small, 0, icon_metric(49, dpi), icon_metric(50, dpi)); // SM_CXSMICON / SM_CYSMICON
    replace_window_icon(hwnd, &mut s.icon_large, 1, icon_metric(11, dpi), icon_metric(12, dpi)); // SM_CXICON / SM_CYICON
}

unsafe fn set_text(hwnd: Hwnd, value: &str) {
    // Windows EDIT controls expect CRLF line endings.
    let windows_text = value.replace("\r\n", "\n").replace('\r', "\n").replace('\n', "\r\n");
    SetWindowTextW(hwnd, wide(&windows_text).as_ptr());
}
unsafe fn get_text(hwnd: Hwnd) -> String {
    let len = GetWindowTextLengthW(hwnd).max(0) as usize;
    let mut text = vec![0u16; len + 1];
    let got = GetWindowTextW(hwnd, text.as_mut_ptr(), (len + 1) as i32).max(0) as usize;
    String::from_utf16_lossy(&text[..got])
}
unsafe fn control(parent: Hwnd, class: &str, text: &str, style: u32, ex: u32, id: usize, instance: Handle) -> Hwnd {
    let hwnd = CreateWindowExW(ex, wide(class).as_ptr(), wide(text).as_ptr(), WS_CHILD | WS_VISIBLE | style,
        0, 0, 0, 0, parent, id as Handle, instance, null_mut());
    SendMessageW(hwnd, WM_SETFONT, GetStockObject(17) as usize, 1);
    hwnd
}

#[derive(Clone, Copy, Debug, Default)]
struct ControlRect { x: i32, y: i32, width: i32, height: i32 }

// Compute the layout in 96-DPI units, then convert each control to pixels.
fn control_rects(client_width: i32, client_height: i32, dpi: u32) -> [ControlRect; 20] {
    let w = (unscale_for_dpi(client_width, dpi) - 24).max(240);
    let height = unscale_for_dpi(client_height, dpi).max(450);
    let input_h = ((height - 350) * 45 / 100).max(85);
    let output_h = (height - input_h - 350).max(85);
    let rect = |x, y, width, height| {
        let left = scale_for_dpi(x, dpi);
        let top = scale_for_dpi(y, dpi);
        ControlRect { x: left, y: top,
            width: scale_for_dpi(x + width, dpi) - left,
            height: scale_for_dpi(y + height, dpi) - top }
    };
    let mut result = [ControlRect::default(); 20];
    let mut y = 10;
    result[0] = rect(12, y, w - 198, 24);
    result[1] = rect(12 + w - 190, y, 190, 26); y += 32;
    result[2] = rect(12, y, w - 198, 24);
    result[3] = rect(12 + w - 190, y, 190, 26); y += 32;
    result[4] = rect(12, y, w - 120, 22);
    result[5] = rect(12 + w - 110, y, 110, 22); y += 23;
    result[6] = rect(12, y, w, input_h); y += input_h + 8;
    let name_w = (w - 24) / 3;
    result[7] = rect(12, y, name_w, 22);
    result[8] = rect(24 + name_w, y, name_w, 22);
    result[9] = rect(36 + 2 * name_w, y, name_w, 22); y += 23;
    result[10] = rect(12, y, name_w, 26);
    result[11] = rect(24 + name_w, y, name_w, 26);
    result[12] = rect(36 + 2 * name_w, y, name_w, 26); y += 35;
    result[13] = rect(12, y, w, 22); y += 23;
    result[14] = rect(12, y, w, 50); y += 56;
    result[15] = rect(12, y, 150, 30);
    result[16] = rect(170, y, 150, 30); y += 37;
    result[17] = rect(12, y, w, 22); y += 23;
    result[18] = rect(12, y, w, output_h); y += output_h + 6;
    result[19] = rect(12, y, w, 42);
    result
}

unsafe fn layout(hwnd: Hwnd, s: &State) {
    let mut rect = Rect::default();
    GetClientRect(hwnd, &mut rect);
    for (hwnd, bounds) in controls(s).into_iter().zip(control_rects(rect.right, rect.bottom, window_dpi(hwnd))) {
        MoveWindow(hwnd, bounds.x, bounds.y, bounds.width, bounds.height, 1);
    }
}
fn spawn_worker() -> (Sender<WorkerCommand>, Receiver<UiEvent>) {
    spawn_worker_with(backend::runtime_update::needs_bootstrap,
        |status| backend::bootstrap_runtime(&|text| status(text)))
}

fn spawn_worker_with<N, B>(needs_bootstrap: N, bootstrap: B) -> (Sender<WorkerCommand>, Receiver<UiEvent>)
where N: Fn() -> Result<bool, String> + Send + 'static,
      B: Fn(&dyn Fn(String)) -> Result<Option<u32>, String> + Send + 'static {
    let (command_tx, command_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    thread::spawn(move || {
        let start = |events: &Sender<UiEvent>| {
            let pending = backend::runtime_update::pending_build().ok().flatten().is_some();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                backend::start(|text| { let _ = events.send(UiEvent::Status(text)); })
            })).unwrap_or_else(|_| Err("モデル初期化中にエラーが発生しました。".into()));
            if pending && result.is_ok() { let _ = events.send(UiEvent::ActiveRuntimeChanged); }
            result
        };
        let prepare = |session: &mut Option<backend::Session>, events: &Sender<UiEvent>, installed: Option<u32>| {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(&needs_bootstrap))
                .unwrap_or_else(|_| Err("エンジンの状態を確認できません。".into()));
            match result {
                Ok(true) => { let _ = events.send(UiEvent::RuntimeMissing); return; }
                Ok(false) => {}
                Err(error) => {
                    let session_active = session.as_mut().is_some_and(|current| current.is_alive());
                    let _ = events.send(UiEvent::RuntimeSetupFailed { error, session_active, during_download: false });
                    return;
                }
            }
            let _ = events.send(UiEvent::RuntimeReady(installed));
            if session.as_mut().is_some_and(|current| current.is_alive()) {
                let _ = events.send(UiEvent::Ready(Ok(true)));
                return;
            }
            *session = match backend::model_present() {
                Ok(false) => { let _ = events.send(UiEvent::Ready(Ok(false))); None }
                Ok(true) => match start(events) {
                    Ok(mut loaded) => {
                        let warning = loaded.take_cleanup_warning();
                        let _ = events.send(UiEvent::Ready(Ok(true)));
                        if let Some(warning) = warning { let _ = events.send(UiEvent::Warning(warning)); }
                        Some(loaded)
                    }
                    Err(error) => { let _ = events.send(UiEvent::Ready(Err(error))); None }
                },
                Err(error) => { let _ = events.send(UiEvent::Ready(Err(error))); None }
            };
        };
        let mut session = None;
        prepare(&mut session, &event_tx, None);
        while let Ok(command) = command_rx.recv() {
            let (email, guidance, affiliation, recipient, sender) = match command {
                WorkerCommand::Generate { email, guidance, affiliation, recipient, sender } => (email, guidance, affiliation, recipient, sender),
                WorkerCommand::DownloadModel => {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        backend::download_model(|text| { let _ = event_tx.send(UiEvent::Status(text)); })
                    })).unwrap_or_else(|_| Err("モデルダウンロード中にエラーが発生しました。".into()));
                    match result {
                        Ok(()) => {
                            let _ = event_tx.send(UiEvent::ModelStored);
                            prepare(&mut session, &event_tx, None);
                        }
                        Err(error) => { let _ = event_tx.send(UiEvent::ModelDownloadFailed(error)); }
                    }
                    continue;
                }
                WorkerCommand::CheckAndUpdate => {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        check_and_update(&mut session, &|text| {
                            let _ = event_tx.send(UiEvent::Status(text));
                        })
                    })).unwrap_or_else(|_| Err("エンジン更新確認中にエラーが発生しました。".into()));
                    let _ = event_tx.send(UiEvent::UpdateDone(result));
                    continue;
                }
                WorkerCommand::DownloadRuntime => {
                    let status = |text| { let _ = event_tx.send(UiEvent::Status(text)); };
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| bootstrap(&status)))
                        .unwrap_or_else(|_| Err("エンジン取得中にエラーが発生しました。".into()));
                    match result {
                        Ok(installed) => {
                            if installed.is_some() { drop(session.take()); }
                            prepare(&mut session, &event_tx, installed);
                        }
                        Err(error) => {
                            let session_active = session.as_mut().is_some_and(|current| current.is_alive());
                            let _ = event_tx.send(UiEvent::RuntimeSetupFailed { error, session_active, during_download: true });
                        }
                    }
                    continue;
                }
            };
            if session.as_mut().is_some_and(|current| !current.is_alive()) { session = None; }
            if session.is_none() {
                match start(&event_tx) {
                    Ok(loaded) => { session = Some(loaded); },
                    Err(error) => { let _ = event_tx.send(UiEvent::Done(Err(error))); continue; }
                }
            }
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                session.as_mut().unwrap().generate(&email, &guidance, &affiliation, &recipient, &sender, |text| {
                    let _ = event_tx.send(UiEvent::Status(text));
                })
            }));
            let result = match result {
                Ok(result) => {
                    if result.is_err() && !session.as_mut().unwrap().is_alive() { session = None; }
                    result
                }
                Err(_) => { session = None; Err("返信生成中にエラーが発生しました。".into()) }
            };
            let warning = session.as_mut().and_then(backend::Session::take_cleanup_warning);
            let _ = event_tx.send(UiEvent::Done(result));
            if let Some(warning) = warning { let _ = event_tx.send(UiEvent::Warning(warning)); }
        }
        // Dropping the session ends its server after the window closes.
    });
    (command_tx, event_rx)
}

fn check_and_update(session: &mut Option<backend::Session>, status: &impl Fn(String)) -> Result<UpdateOutcome, String> {
    status("llama.cpp更新を確認中…".into());
    let check = backend::runtime_update::check_for_update()?;
    apply_update_if_available(check, backend::model_present, backend::runtime_update::pending_matches,
        |release| backend::apply_runtime_update(session, release, status),
        |release| backend::defer_runtime_update(release, status))
}

fn apply_update_if_available(
    check: backend::runtime_update::Check,
    model_present: impl FnOnce() -> Result<bool, String>,
    pending_matches: impl FnOnce(&backend::runtime_update::Release) -> Result<bool, String>,
    apply: impl FnOnce(&backend::runtime_update::Release) -> Result<Option<String>, String>,
    defer: impl FnOnce(&backend::runtime_update::Release) -> Result<(), String>,
) -> Result<UpdateOutcome, String> {
    let Some(release) = check.available else { return Ok(UpdateOutcome::Current(check.installed)); };
    if model_present()? {
        let warning = apply(&release)?;
        Ok(UpdateOutcome::Applied { build: release.build, warning })
    } else {
        if !pending_matches(&release)? {
            defer(&release)?;
        }
        Ok(UpdateOutcome::Deferred { installed: check.installed, build: release.build })
    }
}

fn update_button_state(available: bool, no_new_version: bool) -> (&'static str, bool) {
    if no_new_version { ("利用可能な更新なし", false) } else { ("更新確認", available) }
}

fn engine_button_state(available: bool, runtime_ready: bool, runtime_needs_repair: bool, no_new_version: bool) -> (&'static str, bool) {
    if runtime_needs_repair { ("ダウンロード", available) }
    else { update_button_state(available && runtime_ready, no_new_version) }
}

fn request_local_version(s: &mut State) {
    let epoch = s.version.begin();
    let events = s.version_tx.clone();
    thread::spawn(move || {
        let result = std::panic::catch_unwind(backend::runtime_update::installed_build)
            .unwrap_or_else(|_| Err("エンジンのバージョンを確認できません。".into()));
        let _ = events.send((epoch, result));
    });
}

unsafe fn refresh_buttons(s: &State) {
    let available = s.worker_alive && !s.loading && !s.busy;
    set_text(s.model_download, if s.model_present { "ダウンロード済" } else if s.downloading { "ダウンロード中…" } else { "ダウンロード（6.72GB）" });
    EnableWindow(s.model_download, i32::from(available && !s.model_present));
    let (label, enabled) = engine_button_state(available, s.runtime_ready, s.runtime_needs_repair, s.no_new_version);
    set_text(s.engine_update, label);
    EnableWindow(s.engine_update, i32::from(enabled));
    EnableWindow(s.generate, i32::from(available && s.model_present && s.runtime_ready));
}

unsafe fn update_engine(s: &mut State) {
    if !s.worker_alive || s.loading || s.busy { return; }
    if s.runtime_needs_repair {
        s.loading = true;
        s.version.begin();
        set_text(s.engine_info, "llama.cpp準備中…");
        set_text(s.status, "エンジンのダウンロードを準備中…");
        refresh_buttons(s);
        if s.tx.send(WorkerCommand::DownloadRuntime).is_err() {
            s.loading = false;
            refresh_buttons(s);
            set_text(s.status, "エンジン取得を開始できません。アプリを再起動してください。");
        }
        return;
    }
    if !s.runtime_ready || s.no_new_version { return; }
    s.version.begin(); // Discard any older local version result during an update.
    s.busy = true;
    EnableWindow(s.clear_input, 0);
    refresh_buttons(s);
    set_text(s.engine_info, "llama.cpp更新を確認中…");
    if s.tx.send(WorkerCommand::CheckAndUpdate).is_err() {
        s.busy = false;
        EnableWindow(s.clear_input, 1);
        refresh_buttons(s);
        set_text(s.status, "エンジン更新を開始できません。アプリを再起動してください。");
    }
}

unsafe fn download_model(s: &mut State) {
    if !s.worker_alive || s.loading || s.busy || s.model_present { return; }
    s.loading = true;
    s.downloading = true;
    refresh_buttons(s);
    set_text(s.status, "モデルダウンロードを準備中…");
    if s.tx.send(WorkerCommand::DownloadModel).is_err() {
        s.loading = false;
        s.downloading = false;
        refresh_buttons(s);
        set_text(s.status, "モデルダウンロードを開始できません。アプリを再起動してください。");
    }
}

unsafe fn begin(s: &mut State) {
    if !s.worker_alive || s.loading || s.busy || !s.model_present || !s.runtime_ready { return; }
    let email = get_text(s.input);
    if email.trim().is_empty() { set_text(s.status, "受信メールを入力してください。"); return; }
    let guidance = get_text(s.guidance);
    let affiliation = get_text(s.affiliation);
    let recipient = get_text(s.recipient);
    let sender = get_text(s.sender);
    s.busy = true;
    EnableWindow(s.generate, 0);
    EnableWindow(s.input, 0);
    EnableWindow(s.clear_input, 0);
    EnableWindow(s.guidance, 0);
    EnableWindow(s.affiliation, 0);
    EnableWindow(s.recipient, 0);
    EnableWindow(s.sender, 0);
    refresh_buttons(s);
    set_text(s.output, "");
    set_text(s.status, "準備中…");
    if s.tx.send(WorkerCommand::Generate { email, guidance, affiliation, recipient, sender }).is_err() {
        s.busy = false;
        EnableWindow(s.input, 1);
        EnableWindow(s.clear_input, 1);
        EnableWindow(s.guidance, 1);
        EnableWindow(s.affiliation, 1);
        EnableWindow(s.recipient, 1);
        EnableWindow(s.sender, 1);
        refresh_buttons(s);
        set_text(s.status, "処理開始に失敗しました。アプリを再起動して下さい。");
    }
}
unsafe fn poll(s: &mut State) {
    loop {
        match s.rx.try_recv() {
            Ok(UiEvent::Status(text)) => set_text(s.status, &text),
            Ok(UiEvent::ActiveRuntimeChanged) => request_local_version(s),
            Ok(UiEvent::RuntimeMissing) => {
                s.loading = false;
                s.downloading = false;
                s.runtime_ready = false;
                s.runtime_needs_repair = true;
                s.no_new_version = false;
                s.version.begin();
                set_text(s.engine_info, "llama.cpp（未ダウンロード）");
                set_text(s.status, "llama.cpp の「ダウンロード」を押してください。");
                refresh_buttons(s);
            }
            Ok(UiEvent::RuntimeReady(build)) => {
                let was_repairing = s.runtime_needs_repair;
                s.runtime_ready = true;
                s.runtime_needs_repair = false;
                if let Some(build) = build {
                    s.no_new_version = false;
                    s.version.record(build);
                    set_text(s.engine_info, &s.version.label());
                } else if s.version.installed.is_none() && (s.version.epoch == 0 || was_repairing) {
                    request_local_version(s);
                }
                refresh_buttons(s);
            }
            Ok(UiEvent::RuntimeSetupFailed { error, session_active, during_download }) => {
                s.loading = false;
                s.downloading = false;
                s.runtime_ready = session_active;
                s.runtime_needs_repair = true;
                s.no_new_version = false;
                s.version.begin();
                let label = if session_active { s.version.label() }
                    else if during_download { "llama.cpp（取得できません）".into() }
                    else { "llama.cpp（確認できません）".into() };
                set_text(s.engine_info, &label);
                let status = if during_download {
                    format!("エンジン取得に失敗しました: {error}。「ダウンロード」を押してください。")
                } else {
                    format!("エンジンの状態を確認できません: {error}")
                };
                set_text(s.status, &status);
                refresh_buttons(s);
            }
            Ok(UiEvent::Warning(warning)) => {
                let status = get_text(s.status);
                set_text(s.status, &format!("{status} {warning}"));
            }
            Ok(UiEvent::Ready(result)) => {
                s.loading = false;
                s.downloading = false;
                s.model_present = backend::model_present().unwrap_or(s.model_present);
                match result {
                    Ok(true) => set_text(s.status, "モデルの準備ができました。"),
                    Ok(false) => set_text(s.status, "モデルをダウンロードしてください。"),
                    Err(err) => set_text(s.status, &format!("モデル読込に失敗しました: {err}。受信メールを入力し「返信文生成」を押すと、再試行します。")),
                }
                refresh_buttons(s);
            }
            Ok(UiEvent::ModelStored) => {
                s.model_present = true;
                s.downloading = false;
                refresh_buttons(s);
            }
            Ok(UiEvent::ModelDownloadFailed(error)) => {
                s.loading = false;
                s.downloading = false;
                s.model_present = backend::model_present().unwrap_or(s.model_present);
                set_text(s.status, &format!("モデルダウンロードに失敗しました: {error}"));
                refresh_buttons(s);
            }
            Ok(UiEvent::Done(result)) => {
                s.busy = false;
                EnableWindow(s.input, 1);
                EnableWindow(s.clear_input, 1);
                EnableWindow(s.guidance, 1);
                EnableWindow(s.affiliation, 1);
                EnableWindow(s.recipient, 1);
                EnableWindow(s.sender, 1);
                match result {
                    Ok(text) => { set_text(s.output, &text); set_text(s.status, "生成完了。内容を確認して下さい。"); }
                    Err(err) => set_text(s.status, &format!("エラー: {err}")),
                }
                refresh_buttons(s);
            }
            Ok(UiEvent::UpdateDone(result)) => {
                s.busy = false;
                EnableWindow(s.clear_input, 1);
                match result {
                    Ok(UpdateOutcome::Current(build)) => {
                        s.no_new_version = true;
                        s.version.record(build);
                        set_text(s.engine_info, &format!("llama.cpp b{build}"));
                        set_text(s.status, "利用可能な更新はありません。");
                    }
                    Ok(UpdateOutcome::Applied { build, warning }) => {
                        s.version.record(build);
                        set_text(s.engine_info, &format!("llama.cpp b{build}"));
                        if let Some(warning) = warning { set_text(s.status, &format!("エンジンを更新しました。 {warning}")); }
                        else { set_text(s.status, "エンジンを更新しました。"); }
                    }
                    Ok(UpdateOutcome::Deferred { installed, build }) => {
                        s.version.record(installed);
                        set_text(s.engine_info, &format!("llama.cpp b{installed} → b{build}（適用待ち）"));
                        set_text(s.status, "更新版を取得しました。モデル取得後に推論を確認して適用します。");
                    }
                    Err(error) => {
                        set_text(s.engine_info, &s.version.label());
                        request_local_version(s);
                        set_text(s.status, &format!("エンジン更新エラー: {error}"));
                    }
                }
                refresh_buttons(s);
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                if s.worker_alive {
                    s.worker_alive = false;
                    s.loading = false;
                    s.busy = false;
                    s.downloading = false;
                    EnableWindow(s.input, 1);
                    EnableWindow(s.clear_input, 1);
                    EnableWindow(s.guidance, 1);
                    EnableWindow(s.affiliation, 1);
                    EnableWindow(s.recipient, 1);
                    EnableWindow(s.sender, 1);
                    refresh_buttons(s);
                    set_text(s.status, "処理を続けられません。アプリを再起動してください。");
                }
                break;
            }
        }
    }
    while let Ok((epoch, result)) = s.version_rx.try_recv() {
        if let Some(label) = s.version.finish(epoch, result) { set_text(s.engine_info, &label); }
    }
}
unsafe extern "system" fn wndproc(hwnd: Hwnd, msg: u32, wp: Wparam, lp: Lparam) -> Lresult {
    match msg {
        WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC if wp != 0 => {
            SetBkColor(wp as Handle, 0x00ff_ffff);
            SetTextColor(wp as Handle, 0);
            GetStockObject(0) as Lresult // WHITE_BRUSH
        }
        WM_GETMINMAXINFO if lp != 0 => {
            let info = &mut *(lp as *mut MinMaxInfo);
            let dpi = window_dpi(hwnd);
            info.min_track_size.x = scale_for_dpi(640, dpi);
            info.min_track_size.y = scale_for_dpi(600, dpi);
            0
        }
        WM_SIZE if !STATE.is_null() => { layout(hwnd, &*STATE); 0 }
        WM_DPICHANGED if lp != 0 => {
            let dpi = (wp & 0xffff) as u32;
            if !STATE.is_null() {
                update_font(&mut *STATE, dpi);
                update_window_icons(hwnd, &mut *STATE, dpi);
            }
            let suggested = &*(lp as *const Rect);
            SetWindowPos(hwnd, null_mut(), suggested.left, suggested.top,
                suggested.right - suggested.left, suggested.bottom - suggested.top, 0x0014); // NOZORDER | NOACTIVATE
            if !STATE.is_null() { layout(hwnd, &*STATE); }
            0
        }
        WM_COMMAND if !STATE.is_null() => {
            match wp & 0xffff {
                IDC_GENERATE => begin(&mut *STATE),
                IDC_COPY => { SendMessageW((*STATE).output, EM_SETSEL, 0, -1); SendMessageW((*STATE).output, WM_COPY, 0, 0); set_text((*STATE).status, "返信文をコピーしました。"); }
                IDC_ENGINE_UPDATE => update_engine(&mut *STATE),
                IDC_MODEL_DOWNLOAD => download_model(&mut *STATE),
                IDC_CLEAR_INPUT if !(*STATE).busy => {
                    for edit in [(*STATE).input, (*STATE).affiliation, (*STATE).recipient, (*STATE).sender, (*STATE).guidance] {
                        set_text(edit, "");
                    }
                    SetFocus((*STATE).input);
                }
                _ => {}
            }
            0
        }
        WM_TIMER if !STATE.is_null() => { poll(&mut *STATE); 0 }
        WM_DESTROY => { PostQuitMessage(0); 0 }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn main() { unsafe {
    enable_dpi_awareness();
    let dpi = system_dpi();
    let instance = GetModuleHandleW(null());
    let name = wide("ReplyJPApp");
    let class = WndClass { style: 0, wndproc: Some(wndproc), cls_extra: 0, wnd_extra: 0, instance,
        icon: LoadIconW(instance, 1usize as *const u16), cursor: LoadCursorW(null_mut(), 32512usize as *const u16),
        background: GetStockObject(0), menu_name: null(), class_name: name.as_ptr() };
    RegisterClassW(&class);
    let title = format!("ReplyJP v{}", env!("CARGO_PKG_VERSION"));
    let hwnd = CreateWindowExW(0, name.as_ptr(), wide(&title).as_ptr(), 0x00cf0000,
        0x80000000u32 as i32, 0x80000000u32 as i32, scale_for_dpi(820, dpi), scale_for_dpi(760, dpi), null_mut(), null_mut(), instance, null_mut());
    if hwnd.is_null() { return; }
    let edit_style = WS_TABSTOP | WS_VSCROLL | ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN;
    let model_present = backend::model_present().unwrap_or(false);
    let runtime_needs_bootstrap = backend::runtime_update::needs_bootstrap().unwrap_or(true);
    let (worker_tx, worker_rx) = spawn_worker();
    let (version_tx, version_rx) = mpsc::channel();
    let s = Box::new(State {
        model_name: control(hwnd, "STATIC", "使用モデル：Unsloth Gemma 4 12B IT QAT（UD-Q4_K_XL）", 0, 0, 0, instance),
        model_download: control(hwnd, "BUTTON", if model_present { "ダウンロード済" } else { "ダウンロード（6.72GB）" }, WS_TABSTOP, 0, IDC_MODEL_DOWNLOAD, instance),
        engine_info: control(hwnd, "STATIC", if runtime_needs_bootstrap { "llama.cpp（未ダウンロード）" } else { "llama.cpp" }, 0, 0, 0, instance),
        engine_update: control(hwnd, "BUTTON", if runtime_needs_bootstrap { "ダウンロード" } else { "更新確認" }, WS_TABSTOP, 0, IDC_ENGINE_UPDATE, instance),
        input_label: control(hwnd, "STATIC", "受信メール", 0, 0, 0, instance),
        input: control(hwnd, "EDIT", "", edit_style, 0x200, 0, instance),
        affiliation_label: control(hwnd, "STATIC", "相手の所属（任意）", 0, 0, 0, instance),
        affiliation: control(hwnd, "EDIT", "", WS_TABSTOP | ES_AUTOHSCROLL, 0x200, 0, instance),
        recipient_label: control(hwnd, "STATIC", "相手の名前（任意）", 0, 0, 0, instance),
        recipient: control(hwnd, "EDIT", "", WS_TABSTOP | ES_AUTOHSCROLL, 0x200, 0, instance),
        sender_label: control(hwnd, "STATIC", "自分の名前（任意）", 0, 0, 0, instance),
        sender: control(hwnd, "EDIT", "", WS_TABSTOP | ES_AUTOHSCROLL, 0x200, 0, instance),
        guidance_label: control(hwnd, "STATIC", "返信に含めたい内容（任意。承諾・日程など）", 0, 0, 0, instance),
        guidance: control(hwnd, "EDIT", "", edit_style, 0x200, 0, instance),
        generate: control(hwnd, "BUTTON", "返信文生成", WS_TABSTOP, 0, IDC_GENERATE, instance),
        copy: control(hwnd, "BUTTON", "返信文コピー", WS_TABSTOP, 0, IDC_COPY, instance),
        clear_input: control(hwnd, "BUTTON", "入力クリア", WS_TABSTOP, 0, IDC_CLEAR_INPUT, instance),
        output_label: control(hwnd, "STATIC", "返信文", 0, 0, 0, instance),
        output: control(hwnd, "EDIT", "", edit_style, 0x200, 0, instance),
        status: control(hwnd, "STATIC", if runtime_needs_bootstrap { "llama.cpp の確認中…" } else { "モデル読込中…" }, 0, 0, 0, instance),
        font: null_mut(), icon_small: null_mut(), icon_large: null_mut(),
        rx: worker_rx, tx: worker_tx, version_tx, version_rx, version: VersionTracker::default(),
        loading: true, busy: false, worker_alive: true, model_present, downloading: false, no_new_version: false,
        runtime_ready: false, runtime_needs_repair: runtime_needs_bootstrap,
    });
    refresh_buttons(&s);
    // Let the model's token-budget check report oversized email histories;
    // a small edit-control limit would otherwise silently truncate a paste.
    SendMessageW(s.input, EM_SETLIMITTEXT, 0x7fff_fffe, 0);
    for edit in [s.guidance, s.output] { SendMessageW(edit, EM_SETLIMITTEXT, 1_000_000, 0); }
    for edit in [s.affiliation, s.recipient, s.sender] { SendMessageW(edit, EM_SETLIMITTEXT, 256, 0); }
    STATE = Box::into_raw(s);
    update_font(&mut *STATE, window_dpi(hwnd));
    update_window_icons(hwnd, &mut *STATE, window_dpi(hwnd));
    layout(hwnd, &*STATE);
    if !runtime_needs_bootstrap { request_local_version(&mut *STATE); }
    SetTimer(hwnd, 1, 100, null_mut());
    ShowWindow(hwnd, 1);
    UpdateWindow(hwnd);
    let mut message: Msg = std::mem::zeroed();
    while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
        if IsDialogMessageW(hwnd, &mut message) == 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    let state = Box::from_raw(STATE);
    STATE = null_mut();
    if !state.font.is_null() { DeleteObject(state.font); }
    if !state.icon_small.is_null() { DestroyIcon(state.icon_small); }
    if !state.icon_large.is_null() { DestroyIcon(state.icon_large); }
    drop(state);
} }

#[cfg(test)]
mod dpi_tests {
    use super::{control_rects, message_font, scale_for_dpi, wide, DeleteObject, Handle, LogFont, NonClientMetrics};
    use std::ptr::null_mut;

    #[repr(C)]
    struct Size { width: i32, height: i32 }
    #[link(name = "gdi32")]
    extern "system" {
        fn CreateCompatibleDC(dc: Handle) -> Handle;
        fn DeleteDC(dc: Handle) -> i32;
        fn SelectObject(dc: Handle, object: Handle) -> Handle;
        fn GetTextExtentPoint32W(dc: Handle, text: *const u16, count: i32, size: *mut Size) -> i32;
    }

    #[test]
    fn japanese_labels_fit_at_common_display_scales() {
        assert_eq!(std::mem::size_of::<LogFont>(), 92);
        assert_eq!(std::mem::size_of::<NonClientMetrics>(), 504);
        let captions = [
            (0, "使用モデル：Unsloth Gemma 4 12B IT QAT（UD-Q4_K_XL）", false),
            (1, "ダウンロード（6.72GB）", true),
            (1, "ダウンロード中…", true),
            (2, "llama.cpp（バージョンを確認できません）", false),
            (3, "利用可能な更新なし", true),
            (4, "受信メール", false),
            (5, "入力クリア", true),
            (7, "相手の所属（任意）", false),
            (8, "相手の名前（任意）", false),
            (9, "自分の名前（任意）", false),
            (13, "返信に含めたい内容（任意。承諾・日程など）", false),
            (15, "返信文生成", true),
            (16, "返信文コピー", true),
            (17, "返信文", false),
        ];
        unsafe {
            for dpi in [96, 120, 144, 192] {
                let width = scale_for_dpi(624, dpi);
                let height = scale_for_dpi(556, dpi);
                let rects = control_rects(width, height, dpi);
                assert!(rects.iter().all(|r| r.x >= 0 && r.y >= 0 && r.width > 0 && r.height > 0
                    && r.x + r.width <= width && r.y + r.height <= height), "controls outside window at {dpi} DPI: {rects:?}");
                assert!(rects[0].x + rects[0].width <= rects[1].x);
                assert!(rects[2].x + rects[2].width <= rects[3].x);
                assert!(rects[15].x + rects[15].width <= rects[16].x);
                let font = message_font(dpi);
                assert!(!font.is_null(), "system message font unavailable at {dpi} DPI");
                let dc = CreateCompatibleDC(null_mut());
                assert!(!dc.is_null());
                let old = SelectObject(dc, font);
                for (index, text, button) in captions {
                    let text_wide = wide(text);
                    let mut size = Size { width: 0, height: 0 };
                    assert_ne!(GetTextExtentPoint32W(dc, text_wide.as_ptr(), (text_wide.len() - 1) as i32, &mut size), 0);
                    let r = rects[index];
                    let padding = if button { scale_for_dpi(8, dpi) } else { 0 };
                    assert!(size.width + padding <= r.width && size.height <= r.height,
                        "caption {text:?} does not fit at {dpi} DPI: {}x{} in {}x{}", size.width, size.height, r.width, r.height);
                }
                SelectObject(dc, old);
                DeleteObject(font);
                DeleteDC(dc);
            }
        }
    }
}

#[cfg(test)]
mod workflow_tests {
    use super::{apply_update_if_available, backend, engine_button_state, spawn_worker_with, update_button_state, UiEvent, UpdateOutcome, VersionTracker, WorkerCommand};
    use std::cell::Cell;
    use std::sync::mpsc::RecvTimeoutError;
    use std::sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}};
    use std::time::Duration;

    #[test]
    fn missing_model_waits_for_user_and_generation_does_not_download() {
        // The test executable is built in its own target directory, without a model.
        if backend::model_present().unwrap() { return; }
        let (commands, events) = spawn_worker_with(|| Ok(false), |_| panic!("download was not requested"));
        assert!(matches!(events.recv_timeout(Duration::from_secs(3)), Ok(UiEvent::RuntimeReady(None))));
        assert!(matches!(events.recv_timeout(Duration::from_secs(3)), Ok(UiEvent::Ready(Ok(false)))));
        assert!(matches!(events.recv_timeout(Duration::from_millis(250)), Err(RecvTimeoutError::Timeout)));
        commands.send(WorkerCommand::Generate { email: "test".into(), guidance: String::new(),
            affiliation: String::new(), recipient: String::new(), sender: String::new() }).unwrap();
        match events.recv_timeout(Duration::from_secs(3)) {
            Ok(UiEvent::Done(Err(error))) => assert!(error.contains("モデルが見つからない")),
            _ => panic!("generation should fail without starting a download"),
        }
        assert!(!backend::model_present().unwrap());
    }

    #[test]
    fn missing_runtime_waits_for_the_download_command() {
        if backend::model_present().unwrap() { return; }
        let missing = Arc::new(AtomicBool::new(true));
        let checked = missing.clone();
        let installed = missing.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let (commands, events) = spawn_worker_with(move || Ok(checked.load(Ordering::SeqCst)), move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
            installed.store(false, Ordering::SeqCst);
            Ok(Some(12311))
        });
        assert!(matches!(events.recv_timeout(Duration::from_secs(3)), Ok(UiEvent::RuntimeMissing)));
        assert!(matches!(events.recv_timeout(Duration::from_millis(250)), Err(RecvTimeoutError::Timeout)));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        commands.send(WorkerCommand::DownloadRuntime).unwrap();
        assert!(matches!(events.recv_timeout(Duration::from_secs(3)), Ok(UiEvent::RuntimeReady(Some(12311)))));
        assert!(matches!(events.recv_timeout(Duration::from_secs(3)), Ok(UiEvent::Ready(Ok(false)))));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn runtime_check_error_does_not_start_a_download() {
        let (_commands, events) = spawn_worker_with(|| Err("invalid runtime settings".into()),
            |_| panic!("download was not requested"));
        assert!(matches!(events.recv_timeout(Duration::from_secs(3)),
            Ok(UiEvent::RuntimeSetupFailed { session_active: false, during_download: false, .. })));
        assert!(matches!(events.recv_timeout(Duration::from_millis(250)), Err(RecvTimeoutError::Timeout)));
    }

    #[test]
    fn runtime_download_failure_waits_for_an_explicit_retry() {
        if backend::model_present().unwrap() { return; }
        let missing = Arc::new(AtomicBool::new(true));
        let checked = missing.clone();
        let installed = missing.clone();
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let (commands, events) = spawn_worker_with(move || Ok(checked.load(Ordering::SeqCst)), move |_| {
            if counted.fetch_add(1, Ordering::SeqCst) == 0 { Err("offline".into()) }
            else { installed.store(false, Ordering::SeqCst); Ok(Some(12311)) }
        });
        assert!(matches!(events.recv_timeout(Duration::from_secs(3)), Ok(UiEvent::RuntimeMissing)));
        commands.send(WorkerCommand::DownloadRuntime).unwrap();
        assert!(matches!(events.recv_timeout(Duration::from_secs(3)),
            Ok(UiEvent::RuntimeSetupFailed { session_active: false, during_download: true, .. })));
        assert!(matches!(events.recv_timeout(Duration::from_millis(250)), Err(RecvTimeoutError::Timeout)));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        commands.send(WorkerCommand::DownloadRuntime).unwrap();
        assert!(matches!(events.recv_timeout(Duration::from_secs(3)), Ok(UiEvent::RuntimeReady(Some(12311)))));
        assert!(matches!(events.recv_timeout(Duration::from_secs(3)), Ok(UiEvent::Ready(Ok(false)))));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn update_check_applies_newer_build_without_a_second_command() {
        let applied = Cell::new(false);
        let check = backend::runtime_update::Check { installed: 100, available: Some(backend::runtime_update::test_release(101)) };
        let outcome = apply_update_if_available(check, || Ok(true), |_| panic!("pending check is not needed"),
            |release| { assert_eq!(release.build, 101); applied.set(true); Ok(None) },
            |_| panic!("model is available")).unwrap();
        assert_eq!(outcome, UpdateOutcome::Applied { build: 101, warning: None });
        assert!(applied.get());
    }

    #[test]
    fn update_without_model_is_downloaded_once_and_deferred() {
        let downloads = Cell::new(0);
        for existing in [None, Some(101)] {
            let check = backend::runtime_update::Check { installed: 100, available: Some(backend::runtime_update::test_release(101)) };
            let outcome = apply_update_if_available(check, || Ok(false), |_| Ok(existing == Some(101)),
                |_| panic!("cannot apply without a model"),
                |release| { assert_eq!(release.build, 101); downloads.set(downloads.get() + 1); Ok(()) }).unwrap();
            assert_eq!(outcome, UpdateOutcome::Deferred { installed: 100, build: 101 });
        }
        assert_eq!(downloads.get(), 1);
    }

    #[test]
    fn update_check_without_new_build_does_not_download() {
        let check = backend::runtime_update::Check { installed: 100, available: None };
        let outcome = apply_update_if_available(check, || panic!("no model check needed"), |_| panic!("no pending check needed"),
            |_| panic!("no update to apply"), |_| panic!("no update to download")).unwrap();
        assert_eq!(outcome, UpdateOutcome::Current(100));
    }

    #[test]
    fn update_button_shows_no_new_version_as_disabled() {
        assert_eq!(update_button_state(true, false), ("更新確認", true));
        assert_eq!(update_button_state(false, false), ("更新確認", false));
        assert_eq!(update_button_state(true, true), ("利用可能な更新なし", false));
        assert_eq!(engine_button_state(true, false, true, false), ("ダウンロード", true));
        assert_eq!(engine_button_state(false, false, true, false), ("ダウンロード", false));
        assert_eq!(engine_button_state(true, true, false, false), ("更新確認", true));
    }

    #[test]
    fn stale_local_version_cannot_override_an_updated_runtime() {
        let mut version = VersionTracker::default();
        let old_check = version.begin();
        version.record(12311);
        assert_eq!(version.finish(old_check, Ok(12308)), None);
        assert_eq!(version.label(), "llama.cpp b12311");
        let current_check = version.begin();
        assert_eq!(version.finish(current_check, Ok(12312)).as_deref(), Some("llama.cpp b12312"));
        let failed_check = version.begin();
        assert_eq!(version.finish(failed_check, Err("unavailable".into())).as_deref(), Some("llama.cpp（バージョンを確認できません）"));
    }
}

// The window is the same tao + wry code on Windows and on Linux: Win32 and
// WebView2 underneath on one, GTK and WebKitGTK on the other. What only one
// of them has (the WM_SIZING subclass, the GTK clipboard) is marked as such.
#[cfg(any(windows, target_os = "linux"))]
use std::path::PathBuf;
#[cfg(any(windows, target_os = "linux"))]
use std::time::{Duration, Instant};

#[cfg(any(windows, target_os = "linux"))]
use image::imageops::FilterType;
#[cfg(any(windows, target_os = "linux"))]
use tao::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
#[cfg(any(windows, target_os = "linux"))]
use tao::event::{Event, WindowEvent};
#[cfg(any(windows, target_os = "linux"))]
use tao::event_loop::{ControlFlow, EventLoop, EventLoopBuilder, EventLoopProxy};
#[cfg(any(windows, target_os = "linux"))]
use tao::platform::run_return::EventLoopExtRunReturn;
#[cfg(any(windows, target_os = "linux"))]
use tao::window::{Icon, Theme, WindowBuilder};
#[cfg(any(windows, target_os = "linux"))]
use wry::{NewWindowResponse, WebContext, WebViewBuilder};
#[cfg(windows)]
use wry::{Theme as WebViewTheme, WebViewBuilderExtWindows};
#[cfg(target_os = "linux")]
use tao::platform::unix::WindowExtUnix;
#[cfg(target_os = "linux")]
use wry::WebViewBuilderExtUnix;
#[cfg(windows)]
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
#[cfg(windows)]
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, GetClientRect, GetWindowRect, SetWindowLongPtrW, GWLP_WNDPROC, WM_SIZING,
    WMSZ_BOTTOM, WMSZ_TOP, WMSZ_TOPLEFT, WMSZ_TOPRIGHT, WNDPROC,
};

#[cfg(any(windows, target_os = "linux"))]
const MIRRYL_LOGO: &[u8] = include_bytes!("../../../assets/Mirryl.png");

// ---------------------------------------------------------------- where it opens
// The window opens where the last one was left, not wherever Windows puts it.
// One remembered place is enough - the LAST window to close wins - so there is
// no per-device bookkeeping and no file that grows with the number of phones.

/// The file the position is remembered in. Handed over by the app, exactly like
/// MIRRYL_TRANSPORT_FILE: the engine does not get to decide where settings live.
#[cfg(any(windows, target_os = "linux"))]
fn window_state_path() -> Option<PathBuf> {
    std::env::var_os("MIRRYL_WINDOW_STATE_FILE").map(PathBuf::from)
}

/// `x y width height`, physical pixels, one line. No JSON: the engine carries
/// no serde, and four numbers do not need one.
#[cfg(any(windows, target_os = "linux"))]
fn parse_window_state(text: &str) -> Option<(i32, i32, u32, u32)> {
    let mut parts = text.split_whitespace();
    let x = parts.next()?.parse().ok()?;
    let y = parts.next()?.parse().ok()?;
    let width: u32 = parts.next()?.parse().ok()?;
    let height: u32 = parts.next()?.parse().ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    Some((x, y, width, height))
}

/// Whether a remembered size still suits this phone.
///
/// Only ONE size is remembered, so it can easily have come from a differently
/// shaped device. Restoring a tall phone's window for a wide tablet would open
/// a window with bars down both sides, which looks broken. Position always
/// carries over; size only when the shape agrees.
#[cfg(any(windows, target_os = "linux"))]
fn aspect_matches(width: u32, height: u32, device_width: u32, device_height: u32) -> bool {
    if height == 0 || device_height == 0 {
        return false;
    }
    let saved = width as f64 / height as f64;
    let device = device_width as f64 / device_height as f64;
    (saved - device).abs() / device < 0.12
}

/// Set from any thread to ask the viewer window to close itself.
///
/// There is exactly one window per process, so one flag is the whole story and
/// beats threading an event-loop proxy through the session context.
static CLOSE_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Ask the window to close. Safe to call from any thread, and from a process
/// with no window at all (nothing is listening, nothing happens).
pub fn request_close() {
    CLOSE_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// What the loop reacts to besides the window's own events.
///
/// `Resize` comes from the page (the Ctrl+R view-rotation toggle). The rest
/// come from the app, line by line on stdin, when the engine runs inside one of
/// its tabs (MIRRYL_EMBEDDED): the mirror then has no window of its own until
/// the app asks for one, and can hand it back without the session restarting.
#[cfg(any(windows, target_os = "linux"))]
#[derive(Debug, Clone)]
enum MirrylEvent {
    Resize(u32, u32),
    /// "window": give the mirror a window of its own.
    OpenWindow,
    /// "tab": close that window and keep the session, so the app shows it again.
    CloseWindow,
    /// "close": end the session.
    Quit,
    /// The app is gone (its end of stdin closed).
    AppGone,
    /// Linux only: the phone's copied text, for the PC clipboard. GTK's
    /// clipboard may only be touched from the thread that runs this loop, so
    /// the viewer thread asks for the write through here.
    #[cfg(target_os = "linux")]
    SetClipboard(String),
}

/// Reads the app's commands, one per line, and turns them into loop events.
/// Unknown lines are ignored, so an older engine and a newer app never fight.
#[cfg(any(windows, target_os = "linux"))]
fn read_app_commands(proxy: EventLoopProxy<MirrylEvent>) {
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            let event = match line.trim() {
                "window" => MirrylEvent::OpenWindow,
                "tab" => MirrylEvent::CloseWindow,
                "close" => MirrylEvent::Quit,
                _ => continue,
            };
            if proxy.send_event(event).is_err() {
                return;
            }
        }
        let _ = proxy.send_event(MirrylEvent::AppGone);
    });
}

/// One line to the app, which reads the engine's stdout. Flushed at once:
/// the app is waiting on it to show or hide the picture.
pub fn tell_app(line: &str) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// Set once `run_viewer_window` creates its event loop. A proxy, not a polled
/// flag like CLOSE_REQUESTED: the WaitUntil poll below does not reliably wake
/// on its own here, and an EventLoopProxy's `send_event` is the mechanism tao
/// guarantees will wake a possibly-idle loop from another thread.
#[cfg(any(windows, target_os = "linux"))]
static RESIZE_PROXY: std::sync::Mutex<Option<EventLoopProxy<MirrylEvent>>> =
    std::sync::Mutex::new(None);

/// Ask the window to resize to a physical width/height. Safe to call from any
/// thread; before the window exists, or after it has closed, there is no
/// proxy to send to and this is a no-op.
#[cfg(any(windows, target_os = "linux"))]
pub fn request_resize(width: u32, height: u32) {
    // This is also the one place that learns the CURRENT correct shape
    // (Ctrl+R's 90/270 swap included), so the WM_SIZING lock below tracks it
    // for free instead of needing its own rotation-aware code path.
    if let Ok(mut ratio) = LOCKED_ASPECT.lock() {
        *ratio = (width, height);
    }
    if let Ok(guard) = RESIZE_PROXY.lock() {
        if let Some(proxy) = guard.as_ref() {
            let _ = proxy.send_event(MirrylEvent::Resize(width, height));
        }
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn request_resize(_width: u32, _height: u32) {}

/// Linux: puts the phone's copied text on the PC clipboard. Safe to call from
/// any thread; the write itself happens on the loop's thread (see
/// `MirrylEvent::SetClipboard` for where it goes).
#[cfg(target_os = "linux")]
pub fn request_set_clipboard(text: &str) -> Result<(), String> {
    let guard = RESIZE_PROXY
        .lock()
        .map_err(|_| "window state poisoned".to_owned())?;
    let proxy = guard
        .as_ref()
        .ok_or_else(|| "no window loop to hold the clipboard".to_owned())?;
    proxy
        .send_event(MirrylEvent::SetClipboard(text.to_owned()))
        .map_err(|_| "the window loop is closing".to_owned())
}

/// The clipboard text as the app reads it from a MIRRYL-CLIPBOARD line: one
/// line of lowercase hex, so newlines and any other byte travel intact.
#[cfg(any(target_os = "linux", test))]
fn clipboard_line(text: &str) -> String {
    use std::fmt::Write;
    let mut line = String::with_capacity("MIRRYL-CLIPBOARD ".len() + text.len() * 2);
    line.push_str("MIRRYL-CLIPBOARD ");
    for byte in text.bytes() {
        let _ = write!(line, "{byte:02x}");
    }
    line
}

// ---------------------------------------------------------------- locked aspect ratio
// A free-form resize leaves black bars when the window ends up a different
// shape than the phone: dragging any edge or corner could stretch the window
// into a rectangle that does not match the picture at all. Locking the shape beats the
// simpler "only let the corners be dragged" some remote-desktop tools do -
// it holds true from every edge as well, not just the corners, and there is
// never a frame where the mismatch is visible.
//
// The ratio tracks whatever the CURRENT correct shape is, not just the
// phone's raw resolution: `request_resize` is also how Ctrl+R's view
// rotation tells the window to follow a 90/270 swap, and the lock has to
// swap with it or a rotated picture would fight its own window.
#[cfg(any(windows, target_os = "linux"))]
static LOCKED_ASPECT: std::sync::Mutex<(u32, u32)> = std::sync::Mutex::new((0, 0));

/// The window procedure this replaces, so the subclass can hand off anything
/// that is not WM_SIZING. There is exactly one window per process (same
/// reasoning as CLOSE_REQUESTED above), so one slot is the whole story.
#[cfg(windows)]
static ORIGINAL_WNDPROC: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

/// Hooks WM_SIZING on the window's own procedure so every live drag - from
/// any of the 8 resize handles Windows offers, not just the corners - is
/// corrected to the locked aspect ratio before the frame is drawn. Silently
/// does nothing if the raw HWND cannot be obtained; a window that free-resizes
/// is a much smaller problem than one that fails to open at all.
#[cfg(windows)]
fn install_aspect_lock(window: &tao::window::Window) {
    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(win32) = handle.as_raw() else {
        return;
    };
    let hwnd = HWND(win32.hwnd.get() as *mut std::ffi::c_void);
    // SAFETY: hwnd is the window this process just created and owns; the new
    // procedure is a plain `extern "system" fn` with no captured state (all
    // of it lives in the statics above), which is what GWLP_WNDPROC requires.
    let previous =
        unsafe { SetWindowLongPtrW(hwnd, GWLP_WNDPROC, aspect_lock_wndproc as *const () as isize) };
    ORIGINAL_WNDPROC.store(previous, std::sync::atomic::Ordering::SeqCst);
}

/// Replaces `rect` (screen coordinates, whole window including the title bar
/// and borders) with one whose CLIENT area matches the locked ratio, moving
/// only the edge(s) implied by which handle `edge` is - the anchor corner the
/// user is NOT dragging never moves, which is what makes the resize feel
/// like it is happening from the handle under the cursor instead of the
/// window jumping.
#[cfg(windows)]
fn apply_aspect_lock(hwnd: HWND, edge: u32, rect: &mut RECT) {
    let (ratio_w, ratio_h) = *LOCKED_ASPECT.lock().unwrap_or_else(|e| e.into_inner());
    if ratio_w == 0 || ratio_h == 0 {
        return;
    }
    let ratio = ratio_w as f64 / ratio_h as f64;

    // The chrome (title bar + borders) around the client area, measured fresh
    // every call - DPI can change if the window is dragged to another
    // monitor mid-resize, and this stays correct either way.
    let mut window_rect = RECT::default();
    let mut client_rect = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut window_rect);
        let _ = GetClientRect(hwnd, &mut client_rect);
    }
    let chrome_w = (window_rect.right - window_rect.left) - (client_rect.right - client_rect.left);
    let chrome_h = (window_rect.bottom - window_rect.top) - (client_rect.bottom - client_rect.top);

    // Which point stays fixed depends on which of the 8 handles is being
    // dragged: the edge/corner NOT under the cursor never moves. TOP and
    // BOTTOM are the only two where the drag itself is purely vertical, so
    // those alone let height drive instead of width.
    if edge == WMSZ_TOP || edge == WMSZ_BOTTOM {
        let client_h = (rect.bottom - rect.top - chrome_h).max(1);
        let client_w = (client_h as f64 * ratio).round() as i32;
        // Left edge is the anchor for both - only the right edge follows.
        rect.right = rect.left + client_w + chrome_w;
        return;
    }

    let client_w = (rect.right - rect.left - chrome_w).max(1);
    let client_h = (client_w as f64 / ratio).round() as i32;
    if edge == WMSZ_TOPLEFT || edge == WMSZ_TOPRIGHT {
        // Bottom edge is the anchor - top follows to reach the new height.
        rect.top = rect.bottom - client_h - chrome_h;
    } else {
        // WMSZ_LEFT, WMSZ_RIGHT, WMSZ_BOTTOMLEFT, WMSZ_BOTTOMRIGHT: top edge
        // is the anchor - bottom follows.
        rect.bottom = rect.top + client_h + chrome_h;
    }
}

#[cfg(windows)]
unsafe extern "system" fn aspect_lock_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_SIZING {
        let rect = unsafe { &mut *(lparam.0 as *mut RECT) };
        apply_aspect_lock(hwnd, wparam.0 as u32, rect);
        return LRESULT(1);
    }
    let previous = ORIGINAL_WNDPROC.load(std::sync::atomic::Ordering::SeqCst);
    let previous: WNDPROC = unsafe { std::mem::transmute(previous) };
    unsafe { CallWindowProcW(previous, hwnd, msg, wparam, lparam) }
}

/// Linux: the same "keep the window the phone's shape", asked of the window
/// manager through GTK geometry hints instead of a WM_SIZING subclass. X11
/// honours it from every edge; Wayland has no such request, so there the hint
/// does nothing and the page's own letterboxing keeps the picture right.
/// Called again after every Resize, so a Ctrl+R rotation swaps it.
#[cfg(target_os = "linux")]
fn install_aspect_lock(window: &tao::window::Window) {
    use gtk::gdk;
    use gtk::prelude::GtkWindowExt;

    let (ratio_w, ratio_h) = *LOCKED_ASPECT.lock().unwrap_or_else(|e| e.into_inner());
    if ratio_w == 0 || ratio_h == 0 {
        return;
    }
    let ratio = ratio_w as f64 / ratio_h as f64;
    let geometry = gdk::Geometry::new(0, 0, 0, 0, 0, 0, 0, 0, ratio, ratio, gdk::Gravity::NorthWest);
    window
        .gtk_window()
        .set_geometry_hints(None::<&gtk::Widget>, Some(&geometry), gdk::WindowHints::ASPECT);
}

/// How often the window wakes to notice a close asked for by another thread.
///
/// Sleeping until the next input event is not enough: a session whose phone
/// has gone produces no events AT ALL, so the idle close would never be seen
/// and the window would sit there with the countdown reading 0 s.
#[cfg(any(windows, target_os = "linux"))]
const CLOSE_POLL: Duration = Duration::from_millis(200);

#[cfg(windows)]
pub fn show_error(message: &str) {
    use std::os::windows::ffi::OsStrExt;

    unsafe extern "system" {
        fn MessageBoxW(window: isize, text: *const u16, title: *const u16, kind: u32) -> i32;
    }
    const MB_OK: u32 = 0;
    const MB_ICONERROR: u32 = 0x10;
    let text: Vec<u16> = std::ffi::OsStr::new(message)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let title: Vec<u16> = std::ffi::OsStr::new("Mirryl Connect")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let _ = unsafe { MessageBoxW(0, text.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR) };
}

#[cfg(any(windows, target_os = "linux"))]
pub fn app_icon() -> Option<Icon> {
    let image = image::load_from_memory(MIRRYL_LOGO)
        .ok()?
        .resize_exact(64, 64, FilterType::Lanczos3)
        .into_rgba8();
    let (width, height) = image.dimensions();
    Icon::from_rgba(image.into_raw(), width, height).ok()
}

#[cfg(any(windows, target_os = "linux"))]
pub fn webview_profile(name: &str) -> PathBuf {
    std::env::temp_dir()
        .join("MirrylConnect")
        .join(if cfg!(windows) { "WebView2" } else { "WebKit" })
        .join(name)
}

/// The mirror's own window and everything it needs: the webview lives inside
/// the window and has to go first, so they are kept and dropped together.
#[cfg(any(windows, target_os = "linux"))]
struct ViewerWindow {
    webview: wry::WebView,
    window: tao::window::Window,
}

/// Runs the session's event loop until the session ends.
///
/// Standalone, the window opens at once and closing it ends the session. With
/// `embedded`, the app shows the page in one of its tabs: there is no window
/// until the app sends "window", "tab" takes it away again with the session
/// still running, and "close" - or the app going away while no window is
/// open - ends it. Closing the window with its X always ends the session.
///
/// `before_switch` runs on this thread every time the page is about to move
/// between the app and the window. It frees the video and audio streams the
/// old page held, so the new one does not find them busy.
#[cfg(any(windows, target_os = "linux"))]
pub fn run_viewer_window(
    url: &str,
    title: &str,
    device_width: u32,
    device_height: u32,
    auto_stop: Option<Duration>,
    embedded: bool,
    before_switch: &dyn Fn(),
) -> Result<(), String> {
    let mut event_loop: EventLoop<MirrylEvent> = EventLoopBuilder::with_user_event().build();
    if let Ok(mut guard) = RESIZE_PROXY.lock() {
        *guard = Some(event_loop.create_proxy());
    }
    if embedded {
        read_app_commands(event_loop.create_proxy());
    }

    let profile = webview_profile("viewer");
    std::fs::create_dir_all(&profile).map_err(|error| {
        if cfg!(windows) {
            format!("could not prepare WebView2: {error}")
        } else {
            format!("could not prepare the web view's folder: {error}")
        }
    })?;
    let mut web_context = WebContext::new(Some(profile));

    let mut shown: Option<ViewerWindow> = if embedded {
        None
    } else {
        Some(open_viewer_window(
            &event_loop,
            &mut web_context,
            url,
            title,
            device_width,
            device_height,
        )?)
    };

    let deadline = auto_stop.map(|duration| Instant::now() + duration);
    event_loop.run_return(|event, target, control_flow| {
        // Always wake again shortly, instead of sleeping until the next input:
        // that is the only way a close asked for by another thread can ever be
        // seen. An idle wake is a no-op, and the window is idle precisely when
        // nothing is being mirrored.
        *control_flow = ControlFlow::WaitUntil(Instant::now() + CLOSE_POLL);
        if CLOSE_REQUESTED.load(std::sync::atomic::Ordering::SeqCst) {
            *control_flow = ControlFlow::Exit;
            return;
        }
        match event {
            Event::UserEvent(MirrylEvent::Resize(width, height)) => {
                if let Some(viewer) = shown.as_ref() {
                    viewer.window.set_inner_size(PhysicalSize::new(width, height));
                    // GTK keeps the hint it was given, not a ratio it re-reads.
                    #[cfg(target_os = "linux")]
                    install_aspect_lock(&viewer.window);
                }
            }
            // With the mirror in its own window, that window has the focus and
            // GTK writes the clipboard here. In one of the app's tabs this
            // process has no window at all, and on Wayland a client without
            // the keyboard focus cannot set the clipboard: the app, which has
            // it, does the write (MIRRYL-CLIPBOARD, read by spawn_in_tab).
            #[cfg(target_os = "linux")]
            Event::UserEvent(MirrylEvent::SetClipboard(text)) => {
                if shown.is_some() || !embedded {
                    gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD).set_text(&text);
                } else {
                    tell_app(&clipboard_line(&text));
                }
            }
            Event::UserEvent(MirrylEvent::OpenWindow) => {
                if let Some(viewer) = shown.as_ref() {
                    viewer.window.set_focus();
                    return;
                }
                before_switch();
                match open_viewer_window(
                    target,
                    &mut web_context,
                    url,
                    title,
                    device_width,
                    device_height,
                ) {
                    Ok(viewer) => {
                        shown = Some(viewer);
                        tell_app("MIRRYL-WINDOW");
                    }
                    Err(error) => {
                        // The page is still served: say so and let the app put
                        // it back in its tab rather than ending the session.
                        tell_app(&format!("MIRRYL-WINDOW-FAILED {error}"));
                        tell_app("MIRRYL-TAB");
                    }
                }
            }
            Event::UserEvent(MirrylEvent::CloseWindow) => {
                if let Some(viewer) = shown.take() {
                    close_viewer_window(viewer);
                    before_switch();
                }
                tell_app("MIRRYL-TAB");
            }
            Event::UserEvent(MirrylEvent::Quit) => *control_flow = ControlFlow::Exit,
            // Gone while the mirror sits in one of its tabs: there is nothing
            // left to show it in. A mirror in its own window outlives the app.
            Event::UserEvent(MirrylEvent::AppGone) if shown.is_none() => {
                *control_flow = ControlFlow::Exit;
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => *control_flow = ControlFlow::Exit,
            Event::MainEventsCleared
                if deadline.is_some_and(|deadline| Instant::now() >= deadline) =>
            {
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    });

    if let Some(viewer) = shown.take() {
        close_viewer_window(viewer);
    }
    Ok(())
}

/// Opens the mirror's own window on `url`: where it was last left, at the
/// phone's shape, locked to it.
#[cfg(any(windows, target_os = "linux"))]
fn open_viewer_window(
    target: &tao::event_loop::EventLoopWindowTarget<MirrylEvent>,
    web_context: &mut WebContext,
    url: &str,
    title: &str,
    device_width: u32,
    device_height: u32,
) -> Result<ViewerWindow, String> {
    let (width, height) = initial_viewer_size(device_width, device_height);

    // A remembered place is only usable if a monitor is still THERE. Unplug the
    // second screen and the old coordinates point into nothing: the window would
    // open where it cannot be seen, and cannot be dragged back either.
    let remembered = window_state_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| parse_window_state(&text))
        .filter(|(x, y, _, _)| {
            target.available_monitors().any(|monitor| {
                let origin = monitor.position();
                let size = monitor.size();
                *x >= origin.x
                    && *y >= origin.y
                    && *x < origin.x + size.width as i32
                    && *y < origin.y + size.height as i32
            })
        });

    let mut builder = WindowBuilder::new()
        .with_title(title)
        .with_min_inner_size(LogicalSize::new(240.0, 320.0))
        .with_resizable(true)
        .with_theme(Some(Theme::Dark))
        .with_window_icon(app_icon());
    builder = match remembered {
        Some((x, y, saved_width, saved_height)) => {
            let builder = builder.with_position(PhysicalPosition::new(x, y));
            if aspect_matches(saved_width, saved_height, device_width, device_height) {
                builder.with_inner_size(PhysicalSize::new(saved_width, saved_height))
            } else {
                builder.with_inner_size(LogicalSize::new(width, height))
            }
        }
        None => builder.with_inner_size(LogicalSize::new(width, height)),
    };
    let window = builder
        .build(target)
        .map_err(|error| format!("could not create the Mirryl window: {error}"))?;

    // Locked to whatever shape the window actually opened at (not necessarily
    // raw device_width/device_height - a remembered size within aspect_matches'
    // tolerance can differ slightly), so the first live resize never jumps to a
    // shape other than the one already on screen. request_resize keeps this
    // current from here on (rotation included).
    let initial_size = window.inner_size();
    if let Ok(mut ratio) = LOCKED_ASPECT.lock() {
        *ratio = (initial_size.width, initial_size.height);
    }
    install_aspect_lock(&window);

    let allowed_origin = origin_prefix(url)?;
    let navigation_origin = allowed_origin.clone();
    let builder = WebViewBuilder::new_with_web_context(web_context)
        .with_url(url)
        .with_autoplay(true)
        .with_devtools(false)
        // No with_clipboard: that permission lets the PAGE read the PC's
        // clipboard whenever it likes, and the viewer does not need it. Ctrl+V
        // arrives through the browser's `paste` event, which does not ask for it.
        .with_navigation_handler(move |target| target.starts_with(&navigation_origin))
        .with_new_window_req_handler(|_, _| NewWindowResponse::Deny);
    #[cfg(windows)]
    let webview = builder
        .with_browser_accelerator_keys(false)
        .with_default_context_menus(false)
        .with_theme(WebViewTheme::Dark)
        .build(&window)
        .map_err(|error| {
            format!(
                "could not start the native view. Check that the Microsoft \
WebView2 Runtime is installed: {error}"
            )
        })?;
    // On Linux wry goes inside the GTK box tao already put in the window.
    #[cfg(target_os = "linux")]
    let webview = builder
        .build_gtk(
            window
                .default_vbox()
                .ok_or_else(|| "the Mirryl window has no GTK container".to_owned())?,
        )
        .map_err(|error| {
            format!(
                "could not start the native view. Check that libwebkit2gtk-4.1 \
is installed: {error}"
            )
        })?;
    Ok(ViewerWindow { webview, window })
}

/// Remembers where the window was left and takes it off screen.
#[cfg(any(windows, target_os = "linux"))]
fn close_viewer_window(viewer: ViewerWindow) {
    let ViewerWindow { webview, window } = viewer;
    // Remember where it was left, BEFORE hiding it - the position of a hidden
    // window is not something to rely on. Every way a window goes away passes
    // through here: the X, the idle auto-close, a close asked for by the app,
    // and handing the mirror back to its tab. Best-effort throughout: failing
    // to write this must never turn a clean close into an error.
    if let Some(path) = window_state_path() {
        if let Ok(position) = window.outer_position() {
            let size = window.inner_size();
            if size.width > 0 && size.height > 0 {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(
                    &path,
                    format!("{} {} {} {}", position.x, position.y, size.width, size.height),
                );
            }
        }
    }

    // Hide the window the instant it is let go, before anything else is torn
    // down. Once the loop stops pumping messages the window can no longer
    // repaint, so it would sit on screen as a white rectangle for as long as
    // the session teardown takes - which is exactly how a clean close ends up
    // looking like a hang. Dropping the webview and the window is what really
    // destroys them; hiding first just makes that invisible to the user.
    window.set_visible(false);
    drop(webview);
    drop(window);
}

#[cfg(any(windows, target_os = "linux"))]
fn initial_viewer_size(device_width: u32, device_height: u32) -> (f64, f64) {
    let width = f64::from(device_width.max(1));
    let height = f64::from(device_height.max(1));
    let scale = (1120.0 / width).min(900.0 / height);
    ((width * scale).max(240.0), (height * scale).max(320.0))
}

#[cfg(any(windows, target_os = "linux"))]
fn origin_prefix(url: &str) -> Result<String, String> {
    let scheme_end = url
        .find("://")
        .ok_or_else(|| "the local Mirryl URL is invalid".to_owned())?;
    let after_scheme = scheme_end + 3;
    let path = url[after_scheme..]
        .find('/')
        .map(|index| after_scheme + index)
        .unwrap_or(url.len());
    Ok(url[..path].to_owned())
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn run_viewer_window(
    _url: &str,
    _title: &str,
    _device_width: u32,
    _device_height: u32,
    _auto_stop: Option<std::time::Duration>,
    _embedded: bool,
    _before_switch: &dyn Fn(),
) -> Result<(), String> {
    Err("the native Mirryl window requires Windows or Linux".to_owned())
}

#[cfg(test)]
mod clipboard_line_tests {
    use super::clipboard_line;

    #[test]
    fn carries_any_text_as_one_line_of_hex() {
        assert_eq!(clipboard_line(""), "MIRRYL-CLIPBOARD ");
        assert_eq!(clipboard_line("Hi"), "MIRRYL-CLIPBOARD 4869");
        // A newline would end the line the app reads; as hex it cannot.
        assert_eq!(clipboard_line("a\nb"), "MIRRYL-CLIPBOARD 610a62");
        assert_eq!(clipboard_line("ñ"), "MIRRYL-CLIPBOARD c3b1");
    }
}

#[cfg(all(test, any(windows, target_os = "linux")))]
mod window_state_tests {
    use super::{aspect_matches, parse_window_state};

    #[test]
    fn parses_the_four_numbers_and_rejects_junk() {
        assert_eq!(parse_window_state("100 -40 900 1400"), Some((100, -40, 900, 1400)));
        // A negative x is normal and must survive: a second monitor placed to
        // the LEFT of the primary one has negative coordinates.
        assert_eq!(parse_window_state("-1920 0 800 1200"), Some((-1920, 0, 800, 1200)));
        assert_eq!(parse_window_state(""), None);
        assert_eq!(parse_window_state("100 200"), None);
        assert_eq!(parse_window_state("a b c d"), None);
        // A zero-sized window is not a window; treat it as nothing remembered.
        assert_eq!(parse_window_state("10 10 0 500"), None);
    }

    #[test]
    fn a_remembered_size_only_applies_to_a_similarly_shaped_device() {
        // Same phone, window resized: keep it.
        assert!(aspect_matches(540, 1170, 1080, 2340));
        // Slightly off (a title bar of difference): still fine.
        assert!(aspect_matches(540, 1150, 1080, 2340));
        // A tablet's window for a tall phone: that would open with bars down
        // both sides, so the size is dropped and only the position carries.
        assert!(!aspect_matches(1200, 800, 1080, 2340));
        assert!(!aspect_matches(100, 0, 1080, 2340));
    }
}

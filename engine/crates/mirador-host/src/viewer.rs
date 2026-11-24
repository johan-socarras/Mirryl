use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use mirador_core::adb::AdbClient;

use crate::diagnostics::SessionLog;

/// Stdout byte an agent writes, in place of the ready byte, when this Android
/// gives it no way to do its job; one UTF-8 line saying why follows it.
const UNSUPPORTED_BYTE: u8 = 0x7e;
/// Prefix `connect_stream_agent` puts on an error built from that line, so the
/// stream handlers can tell "this phone can't, stop asking" from a transient
/// failure worth retrying. Control characters keep it out of real ADB text.
const UNSUPPORTED_MARKER: &str = "\u{1}mirryl-unsupported\u{1}";
/// Stdout byte an agent writes first thing in `main`, before any work: the VM
/// is up and the agent's own code is running. Optional - an agent that goes
/// straight to the ready byte is still understood.
const ALIVE_BYTE: u8 = 0x7d;
/// How long the phone gets to start the VM and reach the agent's `main`. Only
/// the phone's own speed is measured here: a slow phone over Wi-Fi can take
/// 0.75 s to get there on a good day and several seconds from doze.
const AGENT_ALIVE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long, from launch, the agent gets to be ready once it is alive. The
/// set-up behind it - a system context, an encoder that may need several
/// tries before it finds a size this phone takes, a mirror through whichever
/// door is open - is real work on a slow phone, and five seconds for all of
/// it, VM start included, is not enough for one over Wi-Fi.
const AGENT_READY_TIMEOUT: Duration = Duration::from_secs(30);
/// Where the agent lives on the phone, and the prefix of everything this
/// program writes there: the jar (`remote_agent_path`) and the stderr files
/// next to it (`agent_stderr_path`). `sweep_stale_agents` removes by this
/// prefix, so nothing that is not a session's own may ever carry it.
const REMOTE_AGENT_DIRECTORY: &str = "/data/local/tmp";
const REMOTE_AGENT_PREFIX: &str = "mirryl-connect-";

const CONTROL_MAGIC: [u8; 4] = *b"MYC1";
const CONTROL_VERSION: u8 = 1;
const CONTROL_KIND_POINTER: u8 = 1;
/// Pointer coordinates travel normalized to this range instead of in device
/// pixels; only the agent knows what the display measures right now, so it does
/// the scaling. Must match POINTER_SCALE in the Java agent.
const POINTER_SCALE: u32 = 10_000;
const CONTROL_KIND_KEY: u8 = 2;
const CONTROL_KIND_TEXT: u8 = 3;
/// Panel power. Action 0 gives the phone's screen back, 1 turns it off while
/// the mirror keeps running. The RESTORE lives in the agent, not here: a PC
/// switched off at the wall cannot ask for anything. See PanelPower.java.
const CONTROL_KIND_PANEL: u8 = 4;
/// "Still here." Sent once a second WHILE the phone's panel is being held off.
///
/// The phone gives the screen back when these stop, and that is what the
/// safety promise rests on - not on this socket closing tidily: a thread parked
/// in a blocking read on this very socket is enough to stop it closing in time.
/// Silence is a signal that no amount of PC-side cleverness can fake.
const CONTROL_KIND_PING: u8 = 5;
/// The engine's version as the workspace Cargo.toml declares it - the only
/// place it is written. Read by the session log and the console banner.
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Comfortably inside the agent's HOST_SILENCE_MS (panel) and thirty times
/// inside its HOST_SILENCE_EXIT_MS (the whole control client), so an ordinary
/// hiccup - a busy machine, a slow adb round trip - never reads as a dead PC.
const PANEL_PING_INTERVAL: Duration = Duration::from_millis(1000);
/// Two fingers at once. action: 0 begin, 1 move, 2 end, 3 cancel - see
/// ControlStreamMain.KIND_PINCH, which this must match exactly. This side
/// only validates the action is in range and passes it through; the agent
/// is the one that has to know what each value means.
///
/// Finger 1 rides in argument1/argument2 same as CONTROL_KIND_POINTER.
/// Finger 2 has no int argument left, so it is packed into argument3 as two
/// 16-bit halves - x2 high, y2 low. POINTER_SCALE (10 000) fits in 14 bits.
const CONTROL_KIND_PINCH: u8 = 6;
const PINCH_CANCEL: u8 = 3;
/// A real Android scroll event (ACTION_SCROLL/AXIS_VSCROLL), for the wheel -
/// see ControlStreamMain.KIND_SCROLL. argument3 is the scroll amount,
/// AXIS_VSCROLL's own units fixed-point at x1000 so a fraction of a notch
/// survives as a signed int. MUST MATCH SCROLL_FIXED_POINT in the agent.
const CONTROL_KIND_SCROLL: u8 = 7;
const SCROLL_FIXED_POINT: f32 = 1000.0;
/// Android's KEYCODE_WAKEUP: wakes a sleeping phone and does nothing to one
/// that is already awake, which is what makes it safe to send on every press.
const KEYCODE_WAKEUP: u32 = 224;
const CONTROL_RECORD_SIZE: usize = 24;
/// Must stay <= the agent's MAX_TEXT_BYTES, which rejects anything larger.
const MAX_CONTROL_TEXT_BYTES: usize = 4096;
/// The PC's clipboard text for the phone's clipboard; action
/// CLIPBOARD_PASTE also presses Ctrl+V once it is set. Same layout as
/// CONTROL_KIND_TEXT. See ClipboardBridge.java, which explains the feature.
const CONTROL_KIND_CLIPBOARD: u8 = 8;
const CLIPBOARD_PASTE: u8 = 1;
/// Largest copy carried either way, in UTF-8 bytes: 64 KB, about thirty pages
/// of text (a link is about 0.1 KB). MUST MATCH
/// ClipboardBridge.MAX_BYTES in the agent.
const MAX_CLIPBOARD_BYTES: usize = 64 * 1024;
/// Largest frame the window may send: the clipboard opcode, its flag byte and
/// the text. Was 256 until the clipboard came along, which also meant pasting
/// anything over ~250 bytes closed the control socket instead of typing it.
const MAX_WEBSOCKET_PAYLOAD: u64 = 2 + MAX_CLIPBOARD_BYTES as u64;
/// Android's META_CTRL_ON | META_CTRL_LEFT_ON - what a physical left Ctrl
/// puts on a key event.
const META_CTRL: u32 = 0x1000 | 0x2000;

struct ActiveVideo {
    child: Child,
    // Socket to the agent: carries the video stream device->host and the
    // one-byte keyframe requests host->device. The forward tunnel is removed
    // by the relay loop that created it, not here.
    input: TcpStream,
    _output: ChildStdout,
}

struct ActiveAudio {
    child: Child,
    stream: TcpStream,
    _output: ChildStdout,
}

struct ActiveControl {
    child: Child,
    input: TcpStream,
    _output: ChildStdout,
    sequence: u32,
    port: u16,
    tunnel: String,
}

#[derive(Clone, Default)]
struct Telemetry {
    queue_depth: u32,
    backlog_ms: u32,
    recoveries: u32,
    rendered_fps: u32,
    packets: u64,
    decoded: u64,
    rendered: u64,
    dropped: u64,
    pending_bytes: u32,
    packet_gap_ms: u32,
    decode_gap_ms: u32,
    render_gap_ms: u32,
    decode_p95_ms: u32,
    render_p95_ms: u32,
    decoder_errors: u32,
    canvas_errors: u32,
    source_freezes: u32,
    render_freezes: u32,
    audio_bytes: u64,
    audio_buffer_ms: u32,
    audio_drops: u64,
    control_reconnects: u32,
    /// Paint turns the browser gave in the last second. In a visible window
    /// it is the monitor's refresh; far below that means the browser is
    /// throttling, not that pictures are missing.
    raf_ticks: u32,
    /// El peor hueco entre dos turnos, en ms.
    raf_worst_gap_ms: u32,
    /// 1 if the viewer window was hidden.
    hidden: u32,
    /// 1 if it had keyboard focus.
    focused: u32,
}

#[derive(Clone, Default)]
struct HostMetrics {
    video_bytes: u64,
    video_reads: u64,
    video_packets: u64,
    video_sequence_gaps: u64,
    last_video_sequence: Option<u32>,
    last_video_pts_us: u64,
    last_video_read_elapsed_ms: u64,
    max_video_read_gap_ms: u64,
    android_video_fps_x10: u64,
    android_encode_age_us: u64,
    android_write_us: u64,
    android_fatal_errors: u64,
    audio_bytes: u64,
    audio_reads: u64,
    last_audio_read_elapsed_ms: u64,
    max_audio_read_gap_ms: u64,
    control_messages: u64,
}

/// How long the window stays open with nobody answering it once the phone is
/// gone. It closes on its own after this, so a cable pulled out by someone who
/// was simply finished does not leave a window and an engine running behind
/// them. It counts INACTIVITY: a reconnect attempt stops the clock.
const LINK_IDLE_CLOSE: Duration = Duration::from_secs(60);

/// How long one reconnect attempt keeps looking. In practice this is not time
/// spent connecting - a transport is either in `adb devices` or it is not - it
/// is time given to the human to push the cable back in, or to the phone to
/// show up on the network.
const RECONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// After the link drops, how long the engine keeps looking for the SAME phone
/// already attached on the OTHER kind of link, and moves the session onto it by
/// itself (`try_auto_switch`).
///
/// Without it, pulling the cable out of a USB mirror would leave the window
/// dead while the phone is already back on Wi-Fi, which reads as a bug. It is
/// not an automatic retry (see `LinkState`): nothing is retried, nothing is
/// dialled, the phone is never asked anything. Once a
/// second, `adb devices` is read - a local listing - and the session moves only
/// when a transport whose NAME proves it is this phone is already there. After
/// this window it is the panel's job again, and the window still closes itself
/// a minute after the drop if nobody answers.
const AUTO_SWITCH_WINDOW: Duration = Duration::from_secs(30);

/// How often a session running over Wi-Fi looks for a cable.
///
/// Only while the offer is unanswered: as soon as the user says yes or no the
/// checking stops, so it costs nothing for the rest of the session.
const USB_WATCH_INTERVAL: Duration = Duration::from_secs(5);

/// A cable has appeared while this session is mirroring over Wi-Fi.
///
/// Why offer at all: the device list shows the phone's BEST available link, so
/// plugging a cable in flips that row's icon to USB - while this window is
/// still mirroring over Wi-Fi. The list then says something that is not true of
/// the picture on screen, and the user reasonably believes they are on the
/// cable. Rather than let the interface lie, the window says out loud that a
/// cable is available and lets them decide.
///
/// Not automatic, deliberately - same rule as everywhere else: moving someone's
/// session to another link is their call. But it is *offered*, because staying
/// silent is what created the confusion in the first place.
#[derive(Default)]
struct UsbOffer {
    /// A USB transport for this phone is there right now.
    available: bool,
    /// The user answered "stay on Wi-Fi". Do not ask again this session.
    dismissed: bool,
}

/// What is known about the link to the phone.
///
/// The engine deliberately has NO automatic retry: with the cable pulled, a
/// retry loop would keep failing against a transport that no longer exists
/// and cannot come back - the phone has moved to its wireless name. Retrying
/// is something a person asks for, by clicking.
#[derive(Default)]
struct LinkState {
    /// When the phone stopped being reachable; `None` while all is well.
    down_since: Option<Instant>,
    /// A reconnect the user asked for is running. Stops the close countdown.
    attempting: bool,
    /// Why the last attempt failed, for the window to show.
    last_error: Option<String>,
}

struct ViewerContext {
    adb: PathBuf,
    /// The adb transport this session is talking to *right now*.
    ///
    /// Mutable because it is not a property of the phone but of how the phone
    /// happens to be attached. Unplugging the cable does not end the session,
    /// it changes this string: `R5CX12ABCDE` becomes something like
    /// `adb-R5CX12ABCDE-62Sel0._adb-tls-connect._tcp`. Everything that shells
    /// out to adb reads it through `serial()` so a reconnect moves the whole
    /// session at once.
    serial: Mutex<String>,
    /// The phone itself (`ro.serialno`), which never changes. This is what makes
    /// it possible to recognise the same device on the other transport: the
    /// wireless name carries it, so `adb-R5CX12ABCDE-...` and `R5CX12ABCDE` can
    /// be told to be one phone. (`adb get-serialno` is no use here - over a
    /// wireless transport it just echoes the transport name back.)
    hardware_serial: String,
    /// The `ip:port` the app has on file for this phone, when it is paired for
    /// wireless debugging at all. `None` is what lets the window answer "this
    /// phone is not paired over Wi-Fi" instead of the useless "Wi-Fi failed".
    wifi_address: Option<String>,
    link: Mutex<LinkState>,
    /// Whether a cable has turned up while this session runs over Wi-Fi.
    usb_offer: Mutex<UsbOffer>,
    /// Where to report which link this session is ACTUALLY using, so the
    /// app's device list can show the MIRROR's link instead of the phone's
    /// best available one. Without it the row flips to USB the moment a
    /// cable is plugged in, while this window is still on Wi-Fi.
    transport_file: Option<PathBuf>,
    /// Where "don't show the game-mode hint again" is remembered. Handed over
    /// by the app, exactly like transport_file above; this window WRITES to
    /// it (see dismiss-game-hint below), which none of the app's own files do.
    game_hint_file: Option<PathBuf>,
    /// When the shell-command wake was last spent, to keep it rare.
    last_shell_wake: Mutex<Option<Instant>>,
    token: String,
    /// The ephemeral port the local viewer is listening on. Kept here so the
    /// request handler can work out the ONE origin allowed to drive the phone
    /// (see origin_allowed): the page this process serves, and nothing else.
    port: u16,
    remote_agent_path: String,
    device_width: u32,
    device_height: u32,
    capture_width: u32,
    capture_height: u32,
    window_title: String,
    started: Instant,
    log: SessionLog,
    stopping: AtomicBool,
    /// Whether the phone's panel is being held off right now.
    ///
    /// Reported BY THE PHONE, never assumed here. The agent lets the mode go on
    /// its own when Android moves the display, so anything this side worked out
    /// for itself would go stale the first time the phone dozed - and what the
    /// window paints from this is a safety warning about the touchscreen still
    /// being live. A warning that outlives the state it describes teaches
    /// people to ignore it.
    panel_off: AtomicBool,
    /// Whether the picture fills the area, cutting what does not fit, instead
    /// of showing the whole screen with bars. Set from the page (Ctrl+F or the
    /// tab's Fill button) and kept here so the next page - the window after the
    /// tab, or the tab after the window - opens the same way.
    fill: AtomicBool,
    active_video: Mutex<Option<ActiveVideo>>,
    active_audio: Mutex<Option<ActiveAudio>>,
    active_control: Mutex<Option<ActiveControl>>,
    active_logcat: Mutex<Option<Child>>,
    telemetry: Mutex<Telemetry>,
    host_metrics: Mutex<HostMetrics>,
}

pub fn run(adb: &AdbClient, serial: &str) -> Result<(), String> {
    let (device_width, device_height) = read_device_size(adb, serial)?;
    let (capture_width, capture_height) = quality_capture_size(device_width, device_height);
    // The alias the user set in Device Manager, if the app handed one over -
    // it owns devices.json, this process only ever sees a bare serial/address,
    // so without this the window/log title falls back to the phone's raw
    // model name no matter what the user renamed the row to.
    let device_name = std::env::var("MIRRYL_DEVICE_ALIAS")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| read_device_name(adb, serial));
    let token = session_token()?;
    let remote_agent_path = remote_agent_path(&token);
    let log = SessionLog::create(serial)?;
    sweep_stale_agents(adb, serial, &token, &log);
    deploy_stream_agent(adb, serial, &remote_agent_path, &token)?;
    log.event(
        "session",
        "starting",
        &format!(
            "\"version\":\"{version}\",\"device\":\"{}\",\
\"device_width\":{device_width},\"device_height\":{device_height},\
\"capture_width\":{capture_width},\"capture_height\":{capture_height}",
            crate::diagnostics::json_escape(&device_name),
            // One source for the engine's version: the workspace Cargo.toml.
            version = ENGINE_VERSION
        ),
    );
    let listener = match TcpListener::bind(("127.0.0.1", 0)) {
        Ok(listener) => listener,
        Err(error) => {
            cleanup_stream_agent(adb, serial, &remote_agent_path);
            return Err(format!("could not open the local viewer: {error}"));
        }
    };
    if let Err(error) = listener.set_nonblocking(true) {
        cleanup_stream_agent(adb, serial, &remote_agent_path);
        return Err(format!("could not configure the viewer: {error}"));
    }
    let port = match listener.local_addr() {
        Ok(address) => address.port(),
        Err(error) => {
            cleanup_stream_agent(adb, serial, &remote_agent_path);
            return Err(format!("could not read the local port: {error}"));
        }
    };
    // Read once, at the start, over a link that is known to work: this is the
    // only moment the phone is guaranteed reachable, and it is the identity the
    // whole reconnect story hangs on. If the phone will not say, fall back to
    // the launch transport - correct for USB, and for Wi-Fi it still matches
    // the serial embedded in the mDNS name.
    let hardware_serial = transport_hardware_serial(adb.executable(), serial)
        .unwrap_or_else(|| serial.to_owned());
    // Handed over by the app, which is the side that owns the device registry.
    // The engine cannot work this out for itself: "not paired over Wi-Fi" and
    // "paired but not on the network right now" look identical from here.
    let wifi_address = std::env::var("MIRRYL_WIFI_ADDRESS")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let context = Arc::new(ViewerContext {
        adb: adb.executable().to_path_buf(),
        serial: Mutex::new(serial.to_owned()),
        hardware_serial,
        wifi_address,
        link: Mutex::new(LinkState::default()),
        usb_offer: Mutex::new(UsbOffer::default()),
        transport_file: std::env::var_os("MIRRYL_TRANSPORT_FILE").map(PathBuf::from),
        game_hint_file: std::env::var_os("MIRRYL_GAME_HINT_FILE").map(PathBuf::from),
        last_shell_wake: Mutex::new(None),
        token,
        port,
        remote_agent_path,
        device_width,
        device_height,
        capture_width,
        capture_height,
        window_title: format!("Mirryl Connect — {device_name}"),
        started: Instant::now(),
        log,
        stopping: AtomicBool::new(false),
        panel_off: AtomicBool::new(false),
        fill: AtomicBool::new(false),
        active_video: Mutex::new(None),
        active_audio: Mutex::new(None),
        active_control: Mutex::new(None),
        active_logcat: Mutex::new(None),
        telemetry: Mutex::new(Telemetry::default()),
        host_metrics: Mutex::new(HostMetrics::default()),
    });
    if let Err(error) = start_control_agent(&context) {
        stop_session(&context);
        cleanup_stream_agent(adb, serial, &context.remote_agent_path);
        return Err(error);
    }
    start_android_diagnostics(&context);
    start_panel_heartbeat(&context);
    // Say which link this session opened on, before anything can change it.
    publish_transport(&context);
    let wake = match adb.run(&["-s", serial, "shell", "input", "keyevent", "224"]) {
        Ok(result) => result,
        Err(error) => {
            stop_session(&context);
            cleanup_stream_agent(adb, serial, &context.remote_agent_path);
            return Err(error);
        }
    };
    if !wake.succeeded() {
        stop_session(&context);
        cleanup_stream_agent(adb, serial, &context.remote_agent_path);
        return Err(format!(
            "Android did not allow turning the screen on: {}",
            wake.combined_output()
        ));
    }

    let diagnostics_context = Arc::clone(&context);
    let diagnostics = thread::spawn(move || run_diagnostics(diagnostics_context));
    let server_context = Arc::clone(&context);
    let server = thread::spawn(move || serve(listener, server_context));
    let url = format!("http://127.0.0.1:{port}/{}/", context.token);
    println!("Local viewer: {url}");

    println!();
    println!(
        "Mirryl Connect {} diagnostics open for {} ({}x{}, 8 Mbps).",
        ENGINE_VERSION, device_name, capture_width, capture_height
    );
    println!("Independent session: {}", context.serial());
    println!("Live log: {}", context.log.path().display());
    context.log.event(
        "session",
        "ready",
        &format!("\"url\":\"{}\"", crate::diagnostics::json_escape(&url)),
    );

    let auto_stop = std::env::var("MIRADOR_AUTOSTOP_SECONDS")
        .ok()
        .and_then(|seconds| seconds.parse::<u64>().ok())
        .map(Duration::from_secs);
    // Inside one of the app's tabs: the app shows this page itself and asks for
    // a window of its own only when the user takes the tab out.
    let embedded = std::env::var_os("MIRRYL_EMBEDDED").is_some();
    if embedded {
        crate::native_window::tell_app(&format!("MIRRYL-READY {url}"));
    }
    let switch_context = Arc::clone(&context);
    let before_switch = move || release_viewer_streams(&switch_context);
    if embedded || std::env::var_os("MIRADOR_NO_OPEN").is_none() {
        if let Err(error) = crate::native_window::run_viewer_window(
            &url,
            &context.window_title,
            device_width,
            device_height,
            auto_stop,
            embedded,
            &before_switch,
        ) {
            stop_session(&context);
            let _ = server.join();
            let _ = diagnostics.join();
            cleanup_stream_agent(adb, serial, &context.remote_agent_path);
            return Err(error);
        }
    } else if let Some(duration) = auto_stop {
        thread::sleep(duration);
    } else {
        println!("Press Enter here to stop this mirror.");
        let mut ignored = String::new();
        let _ = std::io::stdin().read_line(&mut ignored);
    }

    // Teardown is timed per step: the window is already gone by now, so any
    // time spent here is the process outliving its window, and the only way to
    // keep that honest is to measure it rather than assume it is instant.
    let teardown = Instant::now();
    stop_session(&context);
    let stopped_ms = teardown.elapsed().as_millis() as u64;
    // No wake-up connection here on purpose: the listener is non-blocking and
    // the accept loop already re-checks `stopping` every 10 ms. By this point
    // the server thread has usually dropped the listener, so a connect would
    // dial a closed port and eat the full Windows connect timeout (about two
    // seconds of the teardown).
    let server_result = match server.join() {
        Ok(result) => result,
        Err(_) => Err("the viewer's local server ended unexpectedly".to_owned()),
    };
    let joined_ms = teardown.elapsed().as_millis() as u64;
    let _ = diagnostics.join();
    let diagnostics_ms = teardown.elapsed().as_millis() as u64;
    cleanup_stream_agent(adb, serial, &context.remote_agent_path);
    context.log.event(
        "session",
        "stopped",
        &format!(
            "\"stop_session_ms\":{},\"server_join_ms\":{},\
\"diagnostics_join_ms\":{},\
\"agent_cleanup_ms\":{},\"teardown_total_ms\":{}",
            stopped_ms,
            joined_ms.saturating_sub(stopped_ms),
            diagnostics_ms.saturating_sub(joined_ms),
            (teardown.elapsed().as_millis() as u64).saturating_sub(diagnostics_ms),
            teardown.elapsed().as_millis() as u64
        ),
    );
    server_result
}

/// Frees the video and audio streams before the page moves between the app's
/// tab and the mirror's own window.
///
/// Only one page may hold each stream (a second one gets 409), and the page
/// being left behind does not let go by itself: its connection only fails on
/// the next write, and a still screen sends nothing to write. Closing the
/// agent side makes each relay loop end now and clean up after itself; the
/// new page then starts fresh streams with a keyframe. The control channel is
/// left alone - it is not exclusive, and the session lives on.
///
/// Waits up to a second and a half for both relays to finish, so the new page
/// does not knock while they are still on their way out.
fn release_viewer_streams(context: &Arc<ViewerContext>) {
    if let Ok(video) = context.active_video.lock()
        && let Some(active) = video.as_ref()
    {
        let _ = active.input.shutdown(Shutdown::Both);
    }
    if let Ok(audio) = context.active_audio.lock()
        && let Some(active) = audio.as_ref()
    {
        let _ = active.stream.shutdown(Shutdown::Both);
    }
    let deadline = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < deadline {
        let video_busy = context.active_video.lock().map(|v| v.is_some()).unwrap_or(false);
        let audio_busy = context.active_audio.lock().map(|a| a.is_some()).unwrap_or(false);
        if !video_busy && !audio_busy {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    context.log.event("session", "viewer_switch", "");
}

fn stop_session(context: &Arc<ViewerContext>) {
    context.stopping.store(true, Ordering::SeqCst);
    // Shutting down the sockets is what actually unblocks the relay loops,
    // which sit in a blocking read; killing the adb child alone would not.
    if let Ok(mut video) = context.active_video.lock()
        && let Some(active) = video.as_mut()
    {
        let _ = active.input.shutdown(Shutdown::Both);
        let _ = active.child.kill();
    }
    if let Ok(mut audio) = context.active_audio.lock()
        && let Some(active) = audio.as_mut()
    {
        let _ = active.stream.shutdown(Shutdown::Both);
        let _ = active.child.kill();
    }
    let control_tunnel = context.active_control.lock().ok().and_then(|mut control| {
        let mut active = control.take()?;
        let _ = active.input.shutdown(Shutdown::Both);
        let _ = active.child.kill();
        let _ = active.child.wait();
        Some(active.tunnel)
    });
    if let Some(tunnel) = control_tunnel {
        remove_forward(context, "control", &tunnel);
    }
    if let Ok(mut logcat) = context.active_logcat.lock()
        && let Some(child) = logcat.as_mut()
    {
        let _ = child.kill();
    }

    // Kill the agents ON THE PHONE too. Everything above only tears down THIS
    // side: the sockets, the local adb children, the forward. Without reaching
    // the phone, the control agent's accept loop would simply go back to
    // waiting - a listener left behind with `shell` identity, able to inject
    // input. The phone side also times itself out; these three calls make the
    // ordinary case immediate instead of a five-minute wait.
    //
    // Safe to do here, unlike the teardown race warned about at
    // `restart_control`: `stopping` was set to `true` at the top of this
    // function, so no reconnect can be starting a replacement behind us.
    kill_remote_agents(context, "control", "ControlStreamMain");
    kill_remote_agents(context, "host_video", "VideoStreamMain");
    kill_remote_agents(context, "host_audio", "AudioStreamMain");
}

fn run_diagnostics(context: Arc<ViewerContext>) {
    let mut usb_watch_ticks = 0_u64;
    while !context.stopping.load(Ordering::SeqCst) {
        // Sleep in short slices instead of one full second: on teardown the
        // main thread joins this one, and a plain 1 s sleep made every close
        // wait up to a second for a thread with nothing left to do.
        for _ in 0..20 {
            if context.stopping.load(Ordering::SeqCst) {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        // Look for a cable, at a fifth of this loop's rate. Costs one
        // `adb devices`, and stops entirely once the user answers the offer.
        usb_watch_ticks += 1;
        if usb_watch_ticks >= USB_WATCH_INTERVAL.as_secs() {
            usb_watch_ticks = 0;
            refresh_usb_offer(&context);
        }
        // The phone went away on one link: is it already on the other?
        try_auto_switch(&context);
        // Close the window once the phone has been gone a minute with nobody
        // answering. The clock lives here rather than in the page because the
        // browser throttles timers in a window that is minimised or hidden -
        // exactly the window someone walked away from, which is the whole case
        // this is for. An attempt in flight holds it off.
        if let Ok(link) = context.link.lock()
            && let Some(since) = link.down_since
            && !link.attempting
            && since.elapsed() >= LINK_IDLE_CLOSE
        {
            drop(link);
            context.log.event("link", "idle_close", "");
            // The window first: it is what the user sees, and it lives on the
            // main thread's event loop, which this thread cannot reach any
            // other way. Tearing the session down without this left the window
            // on screen reading "closes on its own in 0s" until someone closed
            // it by hand.
            crate::native_window::request_close();
            stop_session(&context);
            return;
        }
        let host = context
            .host_metrics
            .lock()
            .map(|metrics| metrics.clone())
            .unwrap_or_default();
        let browser = context
            .telemetry
            .lock()
            .map(|telemetry| telemetry.clone())
            .unwrap_or_default();
        let elapsed_ms = context.started.elapsed().as_millis() as u64;
        let host_video_gap_ms = if host.last_video_read_elapsed_ms == 0 {
            0
        } else {
            elapsed_ms.saturating_sub(host.last_video_read_elapsed_ms)
        };
        let host_audio_gap_ms = if host.last_audio_read_elapsed_ms == 0 {
            0
        } else {
            elapsed_ms.saturating_sub(host.last_audio_read_elapsed_ms)
        };
        context.log.event(
            "pipeline",
            "snapshot",
            &format!(
                "\"host_video_bytes\":{},\"host_video_reads\":{},\
\"host_video_packets\":{},\"host_video_sequence_gaps\":{},\
\"host_last_video_pts_us\":{},\"host_video_gap_ms\":{},\
\"host_max_video_read_gap_ms\":{},\"host_audio_bytes\":{},\
\"host_audio_reads\":{},\"host_audio_gap_ms\":{},\
\"host_max_audio_read_gap_ms\":{},\"host_control_messages\":{},\
\"android_video_fps_x10\":{},\"android_encode_age_us\":{},\
\"android_write_us\":{},\"android_fatal_errors\":{},\
\"browser_packets\":{},\"browser_decoded\":{},\"browser_rendered\":{},\
\"browser_dropped\":{},\"browser_queue\":{},\"browser_pending_bytes\":{},\
\"browser_pts_backlog_ms\":{},\"browser_packet_gap_ms\":{},\
\"browser_decode_gap_ms\":{},\"browser_render_gap_ms\":{},\
\"browser_decode_p95_ms\":{},\"browser_render_p95_ms\":{},\
\"browser_fps\":{},\"browser_recoveries\":{},\"browser_decoder_errors\":{},\
\"browser_canvas_errors\":{},\"browser_source_freezes\":{},\
\"browser_render_freezes\":{},\"browser_audio_bytes\":{},\
\"browser_audio_buffer_ms\":{},\"browser_audio_drops\":{},\
\"browser_control_reconnects\":{},\"browser_raf_ticks\":{},\
\"browser_raf_worst_gap_ms\":{},\"browser_hidden\":{},\"browser_focused\":{}",
                host.video_bytes,
                host.video_reads,
                host.video_packets,
                host.video_sequence_gaps,
                host.last_video_pts_us,
                host_video_gap_ms,
                host.max_video_read_gap_ms,
                host.audio_bytes,
                host.audio_reads,
                host_audio_gap_ms,
                host.max_audio_read_gap_ms,
                host.control_messages,
                host.android_video_fps_x10,
                host.android_encode_age_us,
                host.android_write_us,
                host.android_fatal_errors,
                browser.packets,
                browser.decoded,
                browser.rendered,
                browser.dropped,
                browser.queue_depth,
                browser.pending_bytes,
                browser.backlog_ms,
                browser.packet_gap_ms,
                browser.decode_gap_ms,
                browser.render_gap_ms,
                browser.decode_p95_ms,
                browser.render_p95_ms,
                browser.rendered_fps,
                browser.recoveries,
                browser.decoder_errors,
                browser.canvas_errors,
                browser.source_freezes,
                browser.render_freezes,
                browser.audio_bytes,
                browser.audio_buffer_ms,
                browser.audio_drops,
                browser.control_reconnects,
                browser.raf_ticks,
                browser.raf_worst_gap_ms,
                browser.hidden,
                browser.focused
            ),
        );
    }
}

fn serve(listener: TcpListener, context: Arc<ViewerContext>) -> Result<(), String> {
    while !context.stopping.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let request_context = Arc::clone(&context);
                thread::spawn(move || {
                    let _ = handle_request(stream, request_context);
                });
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::Interrupted
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::TimedOut
                ) =>
            {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(format!("local server failure: {error}")),
        }
    }
    Ok(())
}

fn handle_request(mut stream: TcpStream, context: Arc<ViewerContext>) -> Result<(), String> {
    stream
        .set_nonblocking(false)
        .map_err(|error| format!("failed to configure the local connection: {error}"))?;
    let _ = stream.set_nodelay(true);
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|error| error.to_string())?;
    let (method, target, headers) = read_http_request(&stream)?;
    let prefix = format!("/{}/", context.token);
    if !target.starts_with(&prefix) {
        return respond(&mut stream, "404 Not Found", "text/plain", b"Not found");
    }
    let route = &target[prefix.len()..];

    // Anything that changes state - every POST, and the control WebSocket
    // that carries touches, keys and text to the phone - must come from the
    // page this process serves. The token in the path is not enough on its
    // own: a WebSocket handshake is not subject to CORS, and a "simple"
    // cross-site POST goes out without a preflight, so a page in any browser
    // on this machine that learned the token (it sits in the session log and
    // in a %TEMP% file name) could drive the phone. Origin is the only thing
    // the server side gets to check, and browsers always send it on both.
    // GETs stay open: they only read.
    if (method == "POST" || route == "control") && !origin_allowed(&headers, context.port) {
        context.log.event(
            "control",
            "origin_rejected",
            &format!(
                "\"origin\":\"{}\",\"route\":\"{}\"",
                crate::diagnostics::json_escape(headers.get("origin").map_or("missing", |v| v)),
                crate::diagnostics::json_escape(route)
            ),
        );
        return respond(&mut stream, "403 Forbidden", "text/plain", b"Forbidden");
    }

    if method == "GET" && route.is_empty() {
        let page = VIEWER_HTML
            .replace("__TOKEN__", &context.token)
            .replace("__WINDOW_TITLE__", &html_escape(&context.window_title))
            .replace("__DEVICE_WIDTH__", &context.device_width.to_string())
            .replace("__DEVICE_HEIGHT__", &context.device_height.to_string())
            .replace("__KEYBINDINGS__", &keybindings_json())
            .replace(
                "__GAME_HINT_DISMISSED__",
                if game_hint_dismissed(&context) { "true" } else { "false" },
            )
            .replace(
                "__FILL_MODE__",
                if context.fill.load(Ordering::Relaxed) { "true" } else { "false" },
            );
        return respond(
            &mut stream,
            "200 OK",
            "text/html; charset=utf-8",
            page.as_bytes(),
        );
    }
    if method == "GET" && route == "logo.png" {
        return respond(&mut stream, "200 OK", "image/png", MIRRYL_LOGO);
    }
    if method == "GET" && route == "panel" {
        // What the PHONE last said about its own panel. The window asks after
        // pressing the shortcut, and then only while it believes the screen is
        // off - a poll that switches itself off, like the link one above it.
        let body = format!(
            "{{\"off\":{}}}",
            context.panel_off.load(Ordering::Relaxed)
        );
        return respond(&mut stream, "200 OK", "application/json", body.as_bytes());
    }
    if method == "POST" && route.starts_with("view?") {
        let fill = query_value(&route[5..], "fill").as_deref() == Some("1");
        context.fill.store(fill, Ordering::Relaxed);
        return respond(&mut stream, "204 No Content", "text/plain", b"");
    }
    if method == "POST" && route == "dismiss-game-hint" {
        dismiss_game_hint(&context);
        return respond(&mut stream, "204 No Content", "text/plain", b"");
    }
    if method == "GET" && route == "stream" {
        return stream_video(stream, context);
    }
    if method == "GET" && route == "audio" {
        return stream_audio(stream, context);
    }
    if method == "GET" && route == "stats" {
        let body = if let Ok(telemetry) = context.telemetry.lock() {
            format!(
                "{{\"queue\":{},\"backlog_ms\":{},\"recoveries\":{},\"fps\":{},\
\"packets\":{},\"decoded\":{},\"rendered\":{},\"dropped\":{},\
\"packet_gap_ms\":{},\"decode_gap_ms\":{},\"render_gap_ms\":{},\
\"decode_p95_ms\":{},\"render_p95_ms\":{},\"source_freezes\":{},\
\"render_freezes\":{},\"audio_buffer_ms\":{}}}",
                telemetry.queue_depth,
                telemetry.backlog_ms,
                telemetry.recoveries,
                telemetry.rendered_fps,
                telemetry.packets,
                telemetry.decoded,
                telemetry.rendered,
                telemetry.dropped,
                telemetry.packet_gap_ms,
                telemetry.decode_gap_ms,
                telemetry.render_gap_ms,
                telemetry.decode_p95_ms,
                telemetry.render_p95_ms,
                telemetry.source_freezes,
                telemetry.render_freezes,
                telemetry.audio_buffer_ms
            )
        } else {
            "{}".to_owned()
        };
        return respond(&mut stream, "200 OK", "application/json", body.as_bytes());
    }
    if method == "GET"
        && route == "control"
        && headers
            .get("upgrade")
            .is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
    {
        let key = headers
            .get("sec-websocket-key")
            .ok_or_else(|| "WebSocket key ausente".to_owned())?;
        return control_websocket(stream, context, key);
    }
    if method == "POST" && route.starts_with("telemetry?") {
        update_telemetry(&context, &route[10..]);
        return respond(&mut stream, "204 No Content", "text/plain", b"");
    }
    if method == "GET" && route == "link" {
        return respond(
            &mut stream,
            "200 OK",
            "application/json",
            link_status_json(&context).as_bytes(),
        );
    }
    if method == "POST" && route.starts_with("usb?") {
        let action = query_value(&route[4..], "action");
        let body = if action.as_deref() == Some("switch") {
            match switch_to_usb(&context) {
                Ok(transport) => format!(
                    "{{\"ok\":true,\"transport\":\"{}\"}}",
                    crate::diagnostics::json_escape(&transport)
                ),
                Err((code, message)) => format!(
                    "{{\"ok\":false,\"reason\":\"{code}\",\"message\":\"{}\"}}",
                    crate::diagnostics::json_escape(&message)
                ),
            }
        } else {
            // "Stay on Wi-Fi": remembered for the rest of the session, so the
            // banner does not come back five seconds later to ask again.
            if let Ok(mut offer) = context.usb_offer.lock() {
                offer.dismissed = true;
                offer.available = false;
            }
            context.log.event("link", "usb_offer_declined", "");
            "{\"ok\":true,\"dismissed\":true}".to_owned()
        };
        return respond(&mut stream, "200 OK", "application/json", body.as_bytes());
    }
    if method == "POST" && route.starts_with("reconnect?") {
        let want_usb = query_value(&route[10..], "via").as_deref() != Some("wifi");
        let body = match attempt_reconnect(&context, want_usb) {
            Ok(transport) => format!(
                "{{\"ok\":true,\"transport\":\"{}\",\"kind\":\"{}\"}}",
                crate::diagnostics::json_escape(&transport),
                if is_usb_transport(&transport) {
                    "usb"
                } else {
                    "wifi"
                }
            ),
            Err((code, message)) => format!(
                "{{\"ok\":false,\"reason\":\"{code}\",\"message\":\"{}\"}}",
                crate::diagnostics::json_escape(&message)
            ),
        };
        return respond(&mut stream, "200 OK", "application/json", body.as_bytes());
    }
    if method == "POST" && route == "sync" {
        request_keyframe(&context)?;
        return respond(&mut stream, "204 No Content", "text/plain", b"");
    }
    if method == "POST" && route.starts_with("resize?") {
        // Only caller today is the Ctrl+R view-rotation toggle: rotating the
        // picture swaps its aspect, and the window has to follow or the
        // rotated image spills past its own frame. `window.resizeTo()` from
        // the page looked like it worked - the window opens at the right
        // size - but that is the host's own initial_viewer_size() doing it
        // before the page even loads; resizeTo() itself is a silent no-op
        // in this WebView2 build. Routed through the host instead, same as
        // every other action this page cannot do for itself.
        let query = &route[7..];
        let width = query_value(query, "w").and_then(|value| value.parse::<u32>().ok());
        let height = query_value(query, "h").and_then(|value| value.parse::<u32>().ok());
        if let (Some(width), Some(height)) = (width, height) {
            if width > 0 && height > 0 {
                crate::native_window::request_resize(width, height);
            }
        }
        return respond(&mut stream, "204 No Content", "text/plain", b"");
    }
    if method == "POST" && route.starts_with("key?") {
        let code = query_value(&route[4..], "code")
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|value| is_allowed_key(*value));
        if let Some(code) = code {
            if code == KEYCODE_WAKEUP {
                wake_screen_if_stalled(&context);
            }
            send_control(&context, CONTROL_KIND_KEY, 0, code, 0, 0)?;
            return respond(&mut stream, "204 No Content", "text/plain", b"");
        }
        return respond(&mut stream, "400 Bad Request", "text/plain", b"Invalid key");
    }
    respond(&mut stream, "404 Not Found", "text/plain", b"Not found")
}

/// Whether a request's Origin names the viewer page itself.
///
/// The window is opened on `http://127.0.0.1:{port}/{token}/` (see `run` and
/// native_window's with_url), so that is the origin the page reports; the
/// `localhost` spelling is accepted too in case the URL is ever written that
/// way. Nothing else matches - not another port, not https, not "null" (what
/// a sandboxed or file:// page sends) - and a MISSING header is refused too.
///
/// Be clear about what this does and does not stop. It reads one header that
/// the client chooses, so it keeps out only clients that cannot choose it: a
/// BROWSER page that somehow learned the token, since the browser stamps the
/// real Origin on WebSocket handshakes and cross-site POSTs and will not let
/// the page lie about it. It is no barrier at all to a local process running as
/// this same user - the main threat - because whatever
/// gave it the token gave it the port in the same URL, and it can simply send
/// `Origin: http://127.0.0.1:{port}`.
///
/// The ways the token leaks in the clear - stdout, the session log, the name of
/// the .jar in %TEMP%, the name of the log file - are not closed by this
/// check; do not read it as closing them.
///
/// Header names arrive lowercased from read_http_request.
fn origin_allowed(headers: &HashMap<String, String>, port: u16) -> bool {
    let Some(origin) = headers.get("origin") else {
        return false;
    };
    origin == &format!("http://127.0.0.1:{port}") || origin == &format!("http://localhost:{port}")
}

fn read_http_request(
    stream: &TcpStream,
) -> Result<(String, String, HashMap<String, String>), String> {
    let mut reader = BufReader::new(
        stream
            .try_clone()
            .map_err(|error| format!("failed to read the request: {error}"))?,
    );
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .map_err(|error| format!("solicitud local invalida: {error}"))?;
    let mut parts = request_line.split_ascii_whitespace();
    let method = parts.next().unwrap_or("").to_owned();
    let target = parts.next().unwrap_or("").to_owned();
    let mut headers = HashMap::new();
    loop {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .map_err(|error| format!("cabecera local invalida: {error}"))?;
        if line == "\r\n" || line == "\n" || line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    Ok((method, target, headers))
}

struct VideoStreamObserver {
    header: [u8; 24],
    header_length: usize,
    payload_remaining: usize,
}

impl VideoStreamObserver {
    fn new() -> Self {
        Self {
            header: [0; 24],
            header_length: 0,
            payload_remaining: 0,
        }
    }

    fn observe(&mut self, mut bytes: &[u8], context: &Arc<ViewerContext>) {
        while !bytes.is_empty() {
            if self.payload_remaining > 0 {
                let consumed = self.payload_remaining.min(bytes.len());
                self.payload_remaining -= consumed;
                bytes = &bytes[consumed..];
                continue;
            }
            let needed = self.header.len() - self.header_length;
            let consumed = needed.min(bytes.len());
            self.header[self.header_length..self.header_length + consumed]
                .copy_from_slice(&bytes[..consumed]);
            self.header_length += consumed;
            bytes = &bytes[consumed..];
            if self.header_length != self.header.len() {
                continue;
            }
            self.header_length = 0;
            if self.header[..4] != *b"MYV1"
                || self.header[4] != 1
                || u16::from_be_bytes([self.header[6], self.header[7]]) != 24
            {
                context
                    .log
                    .event("host_video", "invalid_header", "\"fatal\":true");
                return;
            }
            let payload_length = u32::from_be_bytes(self.header[8..12].try_into().unwrap());
            if payload_length > 16 * 1024 * 1024 {
                context.log.event(
                    "host_video",
                    "invalid_payload",
                    &format!("\"payload_bytes\":{payload_length}"),
                );
                return;
            }
            let sequence = u32::from_be_bytes(self.header[12..16].try_into().unwrap());
            let pts_us = u64::from_be_bytes(self.header[16..24].try_into().unwrap());
            note_video_packet(context, sequence, pts_us);
            self.payload_remaining = payload_length as usize;
        }
    }
}

fn note_video_read(context: &Arc<ViewerContext>, bytes: usize) {
    if let Ok(mut metrics) = context.host_metrics.lock() {
        let now_ms = context.started.elapsed().as_millis() as u64;
        if metrics.last_video_read_elapsed_ms > 0 {
            let gap = now_ms.saturating_sub(metrics.last_video_read_elapsed_ms);
            metrics.max_video_read_gap_ms = metrics.max_video_read_gap_ms.max(gap);
        }
        metrics.last_video_read_elapsed_ms = now_ms;
        metrics.video_reads = metrics.video_reads.saturating_add(1);
        metrics.video_bytes = metrics.video_bytes.saturating_add(bytes as u64);
    }
}

fn note_video_packet(context: &Arc<ViewerContext>, sequence: u32, pts_us: u64) {
    if let Ok(mut metrics) = context.host_metrics.lock() {
        if let Some(previous) = metrics.last_video_sequence
            && sequence != previous.wrapping_add(1)
        {
            metrics.video_sequence_gaps = metrics.video_sequence_gaps.saturating_add(1);
        }
        metrics.last_video_sequence = Some(sequence);
        metrics.last_video_pts_us = pts_us;
        metrics.video_packets = metrics.video_packets.saturating_add(1);
    }
}

fn note_audio_read(context: &Arc<ViewerContext>, bytes: usize) {
    if let Ok(mut metrics) = context.host_metrics.lock() {
        let now_ms = context.started.elapsed().as_millis() as u64;
        if metrics.last_audio_read_elapsed_ms > 0 {
            let gap = now_ms.saturating_sub(metrics.last_audio_read_elapsed_ms);
            metrics.max_audio_read_gap_ms = metrics.max_audio_read_gap_ms.max(gap);
        }
        metrics.last_audio_read_elapsed_ms = now_ms;
        metrics.audio_reads = metrics.audio_reads.saturating_add(1);
        metrics.audio_bytes = metrics.audio_bytes.saturating_add(bytes as u64);
    }
}

fn stream_video(mut stream: TcpStream, context: Arc<ViewerContext>) -> Result<(), String> {
    {
        let video = context
            .active_video
            .lock()
            .map_err(|_| "estado de captura bloqueado".to_owned())?;
        if video.is_some() {
            return respond(
                &mut stream,
                "409 Conflict",
                "text/plain",
                b"A video stream is already active",
            );
        }
    }
    // Start the agent BEFORE committing the 200. It is the step that can turn
    // out impossible on this Android, and once the 200 for a video stream is
    // out the only thing left to send is video - there is no way to say "this
    // phone can't", and an unsupported phone would look like an endless black
    // reconnect. Clearing leftovers still comes first.
    kill_remote_agents(&context, "host_video", "VideoStreamMain");
    let socket_name = format!("mirryl_video_{}", context.token);
    let agent_arguments = format!(
        "{} {} 8000000 {}",
        context.capture_width, context.capture_height, context.token
    );
    let agent = match start_socket_agent(
        &context,
        "host_video",
        "VideoStreamMain",
        &socket_name,
        &agent_arguments,
    ) {
        Ok(agent) => agent,
        Err(error) => {
            if let Some(reason) = error.strip_prefix(UNSUPPORTED_MARKER) {
                context.log.message("host_video", "unsupported", reason);
                return respond(
                    &mut stream,
                    "415 Unsupported Media Type",
                    "text/plain; charset=utf-8",
                    reason.as_bytes(),
                );
            }
            // Answered, not dropped. A connection closed with no reply reads
            // as a network error in the window, which retried it for ever
            // behind a black picture; a 503 with the reason lets the window
            // show what happened and decide how many times to knock.
            context
                .log
                .message("host_video", "agent_start_error", &error);
            return respond(
                &mut stream,
                "503 Service Unavailable",
                "text/plain; charset=utf-8",
                error.as_bytes(),
            );
        }
    };
    stream
        .write_all(
            b"HTTP/1.1 200 OK\r\n\
Content-Type: application/vnd.mirryl.video\r\n\
Cache-Control: no-store, no-transform\r\n\
X-Mirryl-Video: MYV1\r\n\
X-Content-Type-Options: nosniff\r\n\
Connection: close\r\n\r\n",
        )
        .map_err(|error| format!("failed to start the HTTP stream: {error}"))?;
    context.log.event(
        "host_video",
        "stream_starting",
        &format!(
            "\"width\":{},\"height\":{},\"bitrate\":8000000",
            context.capture_width, context.capture_height
        ),
    );
    let tunnel = agent.tunnel.clone();
    let mut socket = agent
        .stream
        .try_clone()
        .map_err(|error| format!("could not duplicate the video socket: {error}"))?;
    {
        let mut active = context
            .active_video
            .lock()
            .map_err(|_| "estado de captura bloqueado".to_owned())?;
        *active = Some(ActiveVideo {
            child: agent.child,
            input: agent.stream,
            _output: agent._output,
        });
    }
    let mut buffer = [0_u8; 64 * 1024];
    let mut observer = VideoStreamObserver::new();
    loop {
        if context.stopping.load(Ordering::SeqCst) {
            break;
        }
        let count = match socket.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) => {
                context
                    .log
                    .message("host_video", "adb_read_error", &error.to_string());
                break;
            }
        };
        note_video_read(&context, count);
        observer.observe(&buffer[..count], &context);
        if let Err(error) = stream
            .write_all(&buffer[..count])
            .and_then(|_| stream.flush())
        {
            context
                .log
                .message("host_video", "browser_write_error", &error.to_string());
            break;
        }
    }
    if let Ok(mut active) = context.active_video.lock()
        && let Some(mut video) = active.take()
    {
        let _ = video.input.shutdown(Shutdown::Both);
        let _ = video.child.kill();
        let _ = video.child.wait();
    }
    remove_forward(&context, "host_video", &tunnel);
    context.log.event("host_video", "stream_stopped", "");
    Ok(())
}

/// Killing the local `adb exec-out` process does NOT kill the `app_process` it
/// started on the phone: verified on a real device, where every closed stream
/// left its agent behind, blocked on a write nobody would ever read. With the
/// viewer reconnecting after a dropped link, those would pile up one per
/// attempt - several encoders capturing the same screen at once.
///
/// Called immediately BEFORE spawning a replacement, never after a teardown:
/// doing it on teardown races the reconnect that follows ~400 ms later, and the
/// pkill lands on the newborn agent instead of the dead one - which reproduces
/// as a stream that restarts and dies forever.
///
/// Matches on the agent class AND the session token: the class keeps it from
/// touching this session's other streams (killing by token alone would take the
/// control channel down with it), and the token keeps it from touching another
/// window mirroring a different device. The pattern deliberately contains no
/// spaces - `adb shell` re-joins argv, so a spaced pattern would be split.
fn kill_remote_agents(context: &Arc<ViewerContext>, component: &'static str, agent_class: &str) {
    let pattern = format!("{}.*{}", agent_class, context.token);
    let result = adb_command(&context.adb)
        .args(["-s", &context.serial(), "shell", "pkill", "-f", &pattern])
        .output();
    // pkill exits 1 when nothing matched - the normal case when the agent died
    // with the link. Only a failure to run adb at all is worth recording.
    if let Err(error) = result {
        context
            .log
            .message(component, "agent_cleanup_error", &error.to_string());
    }
}

fn stream_audio(mut stream: TcpStream, context: Arc<ViewerContext>) -> Result<(), String> {
    {
        let audio = context
            .active_audio
            .lock()
            .map_err(|_| "estado de audio bloqueado".to_owned())?;
        if audio.is_some() {
            return respond(
                &mut stream,
                "409 Conflict",
                "text/plain",
                b"An audio stream is already active",
            );
        }
    }
    // Start before committing the 200, same reasoning as video: a phone whose
    // Android denies a shell process any audio capture (every release before
    // 13) answers with 415 and a reason, and the viewer stops asking for sound
    // for the rest of the session instead of relaunching the agent every few
    // seconds. The picture is a separate stream and is unaffected.
    kill_remote_agents(&context, "host_audio", "AudioStreamMain");
    let socket_name = format!("mirryl_audio_{}", context.token);
    let agent = match start_socket_agent(
        &context,
        "host_audio",
        "AudioStreamMain",
        &socket_name,
        &context.token,
    ) {
        Ok(agent) => agent,
        Err(error) => {
            if let Some(reason) = error.strip_prefix(UNSUPPORTED_MARKER) {
                context.log.message("host_audio", "unsupported", reason);
                return respond(
                    &mut stream,
                    "415 Unsupported Media Type",
                    "text/plain; charset=utf-8",
                    reason.as_bytes(),
                );
            }
            context
                .log
                .message("host_audio", "agent_start_error", &error);
            return respond(
                &mut stream,
                "503 Service Unavailable",
                "text/plain; charset=utf-8",
                error.as_bytes(),
            );
        }
    };
    stream
        .write_all(
            b"HTTP/1.1 200 OK\r\n\
Content-Type: application/octet-stream\r\n\
Cache-Control: no-store, no-transform\r\n\
X-Audio-Format: s16le;rate=48000;channels=2\r\n\
X-Content-Type-Options: nosniff\r\n\
Connection: close\r\n\r\n",
        )
        .map_err(|error| format!("failed to start the audio: {error}"))?;
    context.log.event("host_audio", "stream_starting", "");
    let tunnel = agent.tunnel.clone();
    let mut socket = agent
        .stream
        .try_clone()
        .map_err(|error| format!("could not duplicate the audio socket: {error}"))?;
    {
        let mut active = context
            .active_audio
            .lock()
            .map_err(|_| "estado de audio bloqueado".to_owned())?;
        *active = Some(ActiveAudio {
            child: agent.child,
            stream: agent.stream,
            _output: agent._output,
        });
    }
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        if context.stopping.load(Ordering::SeqCst) {
            break;
        }
        let count = match socket.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) => {
                context
                    .log
                    .message("host_audio", "adb_read_error", &error.to_string());
                break;
            }
        };
        note_audio_read(&context, count);
        if let Err(error) = stream
            .write_all(&buffer[..count])
            .and_then(|_| stream.flush())
        {
            context
                .log
                .message("host_audio", "browser_write_error", &error.to_string());
            break;
        }
    }
    if let Ok(mut active) = context.active_audio.lock()
        && let Some(mut audio) = active.take()
    {
        let _ = audio.stream.shutdown(Shutdown::Both);
        let _ = audio.child.kill();
        let _ = audio.child.wait();
    }
    remove_forward(&context, "host_audio", &tunnel);
    context.log.event("host_audio", "stream_stopped", "");
    Ok(())
}

fn start_android_diagnostics(context: &Arc<ViewerContext>) {
    let mut child = match adb_command(&context.adb)
        .args([
            "-s",
            &context.serial(),
            "logcat",
            "-v",
            "epoch",
            "-T",
            "1",
            "MirrylVideo:I",
            "MirrylAudio:I",
            "MirrylControl:I",
            "*:S",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            context
                .log
                .message("android", "logcat_start_error", &error.to_string());
            return;
        }
    };
    if let Err(error) = crate::attach_to_engine_job(&child) {
        context.log.message("android", "job_attach_error", &error);
    }
    let Some(output) = child.stdout.take() else {
        let _ = child.kill();
        context
            .log
            .event("android", "logcat_output_unavailable", "");
        return;
    };
    let token = context.token.clone();
    let log_context = Arc::clone(context);
    thread::spawn(move || {
        let reader = BufReader::new(output);
        for line in reader.lines() {
            let Ok(line) = line else {
                break;
            };
            if line.contains(&token) {
                update_android_metrics(&log_context, &line);
                log_context.log.message("android", "agent", &line);
            }
        }
    });
    if let Ok(mut active) = context.active_logcat.lock() {
        *active = Some(child);
        context.log.event("android", "logcat_ready", "");
    } else {
        let _ = child.kill();
    }
}

fn update_android_metrics(context: &Arc<ViewerContext>, line: &str) {
    let Ok(mut metrics) = context.host_metrics.lock() else {
        return;
    };
    if line.contains("MirrylVideo:") && line.contains(" tick ") {
        if let Some(value) = metric_from_log_line(line, "fps_x10=") {
            metrics.android_video_fps_x10 = value;
        }
        if let Some(value) = metric_from_log_line(line, "max_encode_age_us=") {
            metrics.android_encode_age_us = value;
        }
        if let Some(value) = metric_from_log_line(line, "max_write_us=") {
            metrics.android_write_us = value;
        }
    }
    if line.contains(" fatal ") {
        metrics.android_fatal_errors = metrics.android_fatal_errors.saturating_add(1);
    }
}

fn metric_from_log_line(line: &str, key: &str) -> Option<u64> {
    let value = line.split_once(key)?.1;
    let digits: String = value
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

struct AgentSocket {
    child: Child,
    stream: TcpStream,
    _output: ChildStdout,
    port: u16,
    tunnel: String,
}

/// Launches a stream agent that serves its data over a localabstract socket
/// and connects to it through `adb forward` — the transport the control
/// channel always used. Streaming over a forwarded socket instead of the
/// agent's stdout skips the adb shell-protocol relay and its buffering, which
/// costs latency on every frame.
///
/// The agent writes a single 0x7f byte to stdout once its socket is
/// listening; creating the forward before that byte would connect to nothing.
fn start_socket_agent(
    context: &Arc<ViewerContext>,
    component: &'static str,
    agent_class: &str,
    socket_name: &str,
    agent_arguments: &str,
) -> Result<AgentSocket, String> {
    // With the phone gone, do not touch adb at all. This guarantee belongs in
    // the host rather than in the window: whatever the viewer decides to do, a
    // session whose phone has left cannot go on spawning processes.
    if context.link_is_down() {
        return Err("the phone is not connected".to_owned());
    }
    let result =
        connect_stream_agent(context, component, agent_class, socket_name, agent_arguments);
    if result.is_err() {
        note_agent_failure(context);
    }
    result
}

fn connect_stream_agent(
    context: &Arc<ViewerContext>,
    component: &'static str,
    agent_class: &str,
    socket_name: &str,
    agent_arguments: &str,
) -> Result<AgentSocket, String> {
    // The agent's stderr goes to a file on the phone, not to nowhere. When the
    // agent dies before it can say why - a VM that could not load the jar, a
    // crash outside the agent's own reporting - that file is the only account
    // of it, and it is read back into the error below. It cannot share
    // stdout: the first byte of stdout is a protocol, and some builds let the
    // VM print warnings before the agent runs.
    let stderr_path = agent_stderr_path(&context.remote_agent_path, agent_class);
    let agent_command = format!(
        "CLASSPATH={} app_process / dev.mirador.agent.{} {} 2>{}",
        context.remote_agent_path, agent_class, agent_arguments, stderr_path
    );
    let started = Instant::now();
    let mut child = adb_command(&context.adb)
        .args([
            "-s",
            &context.serial(),
            "exec-out",
            "sh",
            "-c",
            &agent_command,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("The agent could not be started on the phone: {error}"))?;
    // Dies with the engine, so a crashed engine cannot leave this launcher
    // holding the forward's socket open on the PC (see attach_to_engine_job).
    if let Err(error) = crate::attach_to_engine_job(&child) {
        context.log.message(component, "job_attach_error", &error);
    }
    let mut output = child
        .stdout
        .take()
        .ok_or_else(|| "The agent's output could not be read".to_owned())?;
    // What the agent says on stdout before its socket exists: an optional
    // "alive" first, then the answer - ready, or unsupported plus a reason.
    enum AgentWord {
        Alive,
        Answer(std::io::Result<()>, u8, ChildStdout),
    }
    let (word_sender, word_receiver) = mpsc::sync_channel(2);
    thread::spawn(move || {
        loop {
            let mut byte = [0_u8; 1];
            let result = output.read_exact(&mut byte);
            if result.is_ok() && byte[0] == ALIVE_BYTE {
                if word_sender.send(AgentWord::Alive).is_err() {
                    return;
                }
                continue;
            }
            let _ = word_sender.send(AgentWord::Answer(result, byte[0], output));
            return;
        }
    });
    // Two clocks. Until the agent says it is alive, only the phone's VM
    // start-up is being timed, and it gets the short budget. After that the
    // agent's own set-up is being timed, against the long one - both counted
    // from launch, so the total can never exceed the long budget.
    let mut alive = false;
    let (result, ready, output) = loop {
        let budget = if alive {
            AGENT_READY_TIMEOUT
        } else {
            AGENT_ALIVE_TIMEOUT
        };
        let remaining = budget.saturating_sub(started.elapsed());
        match word_receiver.recv_timeout(remaining) {
            Ok(AgentWord::Alive) => {
                if !alive {
                    alive = true;
                    context.log.event(
                        component,
                        "agent_alive",
                        &format!("\"after_ms\":{}", started.elapsed().as_millis()),
                    );
                }
            }
            Ok(AgentWord::Answer(result, byte, output)) => break (result, [byte], output),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let detail = agent_stderr_tail(context, component, &stderr_path);
                return Err(if alive {
                    format!(
                        "The phone's agent started but was not ready after {} seconds{detail}",
                        AGENT_READY_TIMEOUT.as_secs()
                    )
                } else {
                    format!(
                        "The phone did not start the agent within {} seconds{detail}",
                        AGENT_ALIVE_TIMEOUT.as_secs()
                    )
                });
            }
        }
    };
    if let Err(error) = result {
        let _ = child.kill();
        let _ = child.wait();
        let detail = agent_stderr_tail(context, component, &stderr_path);
        return Err(format!(
            "The phone's agent exited before it was ready ({error}){detail}"
        ));
    }
    if ready[0] == UNSUPPORTED_BYTE {
        // The agent decided this Android gives it no way to do its job, and
        // wrote one line saying why. Read that line and hand it back tagged so
        // the caller shows it to the person instead of retrying: a door the OS
        // keeps shut does not open on the next attempt, and relaunching it
        // would only give a black window that never recovers.
        let mut reason = Vec::new();
        let mut byte = [0_u8; 1];
        let mut output = output;
        while reason.len() < 1024 {
            match output.read(&mut byte) {
                Ok(0) => break,
                Ok(_) if byte[0] == b'\n' => break,
                Ok(_) => reason.push(byte[0]),
                Err(_) => break,
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        let text = String::from_utf8_lossy(&reason).trim().to_owned();
        return Err(format!(
            "{UNSUPPORTED_MARKER}{}",
            if text.is_empty() { "no compatible con este Android".to_owned() } else { text }
        ));
    }
    if ready[0] != 0x7f {
        let _ = child.kill();
        let _ = child.wait();
        let detail = agent_stderr_tail(context, component, &stderr_path);
        return Err(format!(
            "The phone's agent answered with something unexpected (byte {:#04x}){detail}",
            ready[0]
        ));
    }
    let remote_socket = format!("localabstract:{socket_name}");
    let forward = adb_command(&context.adb)
        .args(["-s", &context.serial(), "forward", "tcp:0", &remote_socket])
        .output()
        .map_err(|error| format!("could not create the ADB tunnel: {error}"))?;
    if !forward.status.success() {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!(
            "ADB could not create the tunnel: {}",
            String::from_utf8_lossy(&forward.stderr).trim()
        ));
    }
    let port = match String::from_utf8_lossy(&forward.stdout)
        .trim()
        .parse::<u16>()
    {
        Ok(port) if port > 0 => port,
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "ADB devolvio un puerto invalido: {}",
                String::from_utf8_lossy(&forward.stdout).trim()
            ));
        }
    };
    let tunnel = format!("tcp:{port}");
    let stream = match connect_control_socket(port, Duration::from_secs(3)) {
        Ok(stream) => stream,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            remove_forward(context, component, &tunnel);
            return Err(error);
        }
    };
    context.log.event(
        component,
        "agent_ready",
        &format!(
            "\"transport\":\"adb-forward-localabstract\",\"local_port\":{port},\
\"socket\":\"{}\",\"after_ms\":{}",
            crate::diagnostics::json_escape(socket_name),
            started.elapsed().as_millis()
        ),
    );
    Ok(AgentSocket {
        child,
        stream,
        _output: output,
        port,
        tunnel,
    })
}

/// Move everything that does NOT restart itself onto the new transport.
///
/// Video and audio need nothing: the window re-requests them, so each request
/// starts a fresh agent by itself. These two are started once when the session
/// opens, and after a reconnect they were still attached to the link that died.
fn resume_after_reconnect(context: &Arc<ViewerContext>, old_serial: &str) {
    // The jar is pushed once per session and every relaunch reuses it. If it
    // went away meanwhile - something cleared /data/local/tmp, or another
    // session's sweep caught the gap between a pkill and the relaunch - every
    // restart below would fail with restart_error. ~25 KB, idempotent, and
    // only on the reconnect path.
    let adb = AdbClient::from_executable(context.adb.clone());
    match deploy_stream_agent(&adb, &context.serial(), &context.remote_agent_path, &context.token) {
        Ok(()) => context.log.event("session", "agent_redeployed", ""),
        Err(error) => context.log.message("session", "agent_redeploy_error", &error),
    }
    restart_control_agent(context, old_serial);
    // Not user-visible, but this is the phone's own account of what the agents
    // are doing - and a session that has just changed link is exactly when it
    // is worth having. It went silent from the reconnect onwards.
    if let Ok(mut logcat) = context.active_logcat.lock()
        && let Some(mut child) = logcat.take()
    {
        let _ = child.kill();
        let _ = child.wait();
    }
    start_android_diagnostics(context);
}

/// Put the control channel back on the transport the session just moved to.
///
/// Control is the odd one out: it is opened ONCE when the session starts, so
/// after a reconnect it would still be the agent that died with the old link -
/// the window reopening its WebSocket forever, and a mirror whose picture
/// comes back while touch and keyboard stay dead.
fn restart_control_agent(context: &Arc<ViewerContext>, old_serial: &str) {
    let previous = context
        .active_control
        .lock()
        .ok()
        .and_then(|mut control| control.take());
    if let Some(mut active) = previous {
        let _ = active.input.shutdown(Shutdown::Both);
        let _ = active.child.kill();
        let _ = active.child.wait();
        // With the OLD transport: adb keys forwards by device, and the old link
        // is usually gone anyway - this is best effort, to avoid leaving the
        // listener behind when it is not.
        remove_forward_for(context, "control", old_serial, &active.tunnel);
    }
    // Killing the adb client above does NOT kill the agent on the phone. Over a
    // link that has died that makes no difference - the agent went with it - but
    // a hand-over to the cable happens while the old link is still up, so the
    // old agent keeps running and keeps holding `mirryl_control_<token>`. The
    // replacement then cannot bind that name and exits, which surfaces as
    // "the Android agent did not start: failed to fill whole buffer".
    //
    // Video and audio clear their agent before starting a new one, and control
    // has to as well, or a USB switch comes back with a live picture and dead
    // input - the picture returning is what makes it look like it worked.
    kill_remote_agents(context, "control", "ControlStreamMain");
    if let Err(first) = start_control_agent(context) {
        // pkill signals; it does not wait for the process to die and release
        // the socket name. One retry covers that gap.
        thread::sleep(Duration::from_millis(400));
        if let Err(second) = start_control_agent(context) {
            context
                .log
                .message("control", "restart_error", &format!("{first} | retry: {second}"));
        }
    }
}

fn start_control_agent(context: &Arc<ViewerContext>) -> Result<(), String> {
    context.log.event("control", "agent_starting", "");
    let socket_name = format!("mirryl_control_{}", context.token);
    let agent = start_socket_agent(
        context,
        "control",
        "ControlStreamMain",
        &socket_name,
        &context.token,
    )?;
    // A fresh control channel means a fresh agent, and a fresh agent is holding
    // nothing. Cleared before the reader starts so a window that reconnects
    // never inherits the last session's warning.
    context.panel_off.store(false, Ordering::Relaxed);
    if let Ok(reader) = agent.stream.try_clone() {
        start_status_reader(context, reader);
    }
    let mut active = context
        .active_control
        .lock()
        .map_err(|_| "estado de control bloqueado".to_owned())?;
    *active = Some(ActiveControl {
        child: agent.child,
        input: agent.stream,
        _output: agent._output,
        sequence: 0,
        port: agent.port,
        tunnel: agent.tunnel,
    });
    Ok(())
}

/// Tells the phone this PC is still alive, once a second, for as long as the
/// session lasts.
///
/// Not only while the phone holds its panel off: the control agent drops any
/// client silent for 30 s (HOST_SILENCE_EXIT_MS, so a dead PC whose adb
/// children keep the socket open cannot leave it listening for ever), and with
/// the panel on and nobody touching the mirror for half a minute it would
/// close the channel under us, every later tap or key hitting a closed socket.
/// One 20-byte record a second is the price; the agent treats a ping as
/// "nothing to do" whatever the panel is doing. See CONTROL_KIND_PING for why
/// the panel promise rests here and not on "the socket will close".
fn start_panel_heartbeat(context: &Arc<ViewerContext>) {
    let context = Arc::clone(context);
    thread::spawn(move || {
        while !context.stopping.load(Ordering::Relaxed) {
            thread::sleep(PANEL_PING_INTERVAL);
            // A failure here is a channel that is already gone, which the phone
            // is about to work out for itself from the silence. Nothing to do.
            let _ = send_control(&context, CONTROL_KIND_PING, 0, 0, 0, 0);
        }
    });
}

/// Listens to the phone on the control socket.
///
/// The socket has always been able to carry traffic both ways; only one
/// direction was ever used. This is the other one: the agent reports what the
/// panel is doing, including when it gives the panel back WITHOUT being asked,
/// which is the case no amount of bookkeeping on this side could have known
/// about.
///
/// Its own thread with its own clone of the socket, because the writing side is
/// behind a mutex that a command may be holding at any moment, and a status
/// message must not have to wait on somebody's keystroke.
fn start_status_reader(context: &Arc<ViewerContext>, mut stream: TcpStream) {
    const STATUS_MAGIC: [u8; 4] = *b"MYS1";
    const STATUS_VERSION: u8 = 1;
    const STATUS_KIND_PANEL: u8 = 1;
    const STATUS_KIND_CLIPBOARD: u8 = 2;
    const STATUS_RECORD_SIZE: usize = 8;

    let context = Arc::clone(context);
    thread::spawn(move || {
        // No read timeout: this thread's whole job is to wait, and it ends when
        // the socket does - which is also when the session it belongs to ends.
        let _ = stream.set_read_timeout(None);
        let mut record = [0_u8; STATUS_RECORD_SIZE];
        loop {
            if stream.read_exact(&mut record).is_err() {
                break;
            }
            if record[..4] != STATUS_MAGIC || record[4] != STATUS_VERSION {
                // Not something this build understands. Giving up rather than
                // hunting for the next good record on purpose: the stream is
                // fixed-size frames, so one bad one means the two sides no
                // longer agree where records start, and guessing from there
                // would turn noise into state.
                context
                    .log
                    .message("control", "status_desync", "unexpected record");
                break;
            }
            if record[5] == STATUS_KIND_PANEL {
                let off = record[6] != 0;
                context.panel_off.store(off, Ordering::Relaxed);
                context
                    .log
                    .message("control", "panel_state", if off { "off" } else { "on" });
            } else if record[5] == STATUS_KIND_CLIPBOARD {
                // The phone's clipboard changed: header, then the byte count,
                // then the text - see StatusWriter.onPhoneClipboard.
                let mut length = [0_u8; 4];
                if stream.read_exact(&mut length).is_err() {
                    break;
                }
                let length = u32::from_be_bytes(length) as usize;
                if length > MAX_CLIPBOARD_BYTES {
                    // The agent never sends more; a count past it means the
                    // two sides disagree, and the bytes after it are not
                    // records. Same reasoning as the desync above.
                    context
                        .log
                        .message("control", "status_desync", "clipboard too large");
                    break;
                }
                let mut text = vec![0_u8; length];
                if stream.read_exact(&mut text).is_err() {
                    break;
                }
                let Ok(text) = String::from_utf8(text) else {
                    continue;
                };
                // The size only, never the text: these logs are kept.
                let outcome = match set_pc_clipboard(&text) {
                    Ok(()) => length.to_string(),
                    Err(error) => format!("{length} failed: {error}"),
                };
                context.log.message("control", "clipboard_to_pc", &outcome);
            }
        }
        // However this ended - socket closed, agent gone, session over - the
        // phone is not being held any more: the agent gives the panel back when
        // it loses this socket. Leaving the flag set would leave the window
        // warning about a screen that is on.
        context.panel_off.store(false, Ordering::Relaxed);
    });
}

/// Puts the phone's copied text on the Windows clipboard, as Unicode text.
///
/// Written by the engine and not by the mirror page on purpose: the page would
/// need clipboard permission (which it does not get, see native_window.rs)
/// and a fresh keypress to write, and a copy made with the
/// phone's own "Copy" button has neither.
///
/// A few short retries, because OpenClipboard fails while another program
/// holds the clipboard, and that never lasts long.
#[cfg(windows)]
fn set_pc_clipboard(text: &str) -> Result<(), String> {
    use windows::Win32::Foundation::{GlobalFree, HANDLE};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
    use windows::Win32::System::Ole::CF_UNICODETEXT;

    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0);
    let mut opened = false;
    for _ in 0..10 {
        if unsafe { OpenClipboard(None) }.is_ok() {
            opened = true;
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    if !opened {
        return Err("clipboard busy".to_owned());
    }
    let result = (|| unsafe {
        EmptyClipboard().map_err(|error| error.to_string())?;
        let memory = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2).map_err(|error| error.to_string())?;
        let target = GlobalLock(memory) as *mut u16;
        if target.is_null() {
            let _ = GlobalFree(Some(memory));
            return Err("GlobalLock failed".to_owned());
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), target, wide.len());
        let _ = GlobalUnlock(memory);
        // Once SetClipboardData succeeds the memory belongs to Windows; only
        // on failure is it still ours to free.
        if let Err(error) = SetClipboardData(u32::from(CF_UNICODETEXT.0), Some(HANDLE(memory.0))) {
            let _ = GlobalFree(Some(memory));
            return Err(error.to_string());
        }
        Ok(())
    })();
    let _ = unsafe { CloseClipboard() };
    result
}

/// Linux: through the window loop, which owns GTK (native_window.rs).
#[cfg(target_os = "linux")]
fn set_pc_clipboard(text: &str) -> Result<(), String> {
    crate::native_window::request_set_clipboard(text)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn set_pc_clipboard(_text: &str) -> Result<(), String> {
    Err("not supported on this system".to_owned())
}

fn connect_control_socket(port: u16, timeout: Duration) -> Result<TcpStream, String> {
    let deadline = Instant::now() + timeout;
    loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(stream) => {
                let _ = stream.set_nodelay(true);
                let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                return Ok(stream);
            }
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                thread::sleep(Duration::from_millis(50));
            }
            Err(error) => {
                return Err(format!(
                    "could not connect the local control socket: {error}"
                ));
            }
        }
    }
}

fn remove_forward(context: &Arc<ViewerContext>, component: &'static str, tunnel: &str) {
    remove_forward_for(context, component, &context.serial(), tunnel);
}

/// Dropping a forward needs the transport it was CREATED on: adb keys them by
/// device, so asking the phone's new transport to drop a listener that belonged
/// to the old one only answers "listener not found" and leaves it behind.
fn remove_forward_for(
    context: &Arc<ViewerContext>,
    component: &'static str,
    serial: &str,
    tunnel: &str,
) {
    match adb_command(&context.adb)
        .args(["-s", serial, "forward", "--remove", tunnel])
        .output()
    {
        Ok(output) if output.status.success() => {}
        Ok(output) => context.log.message(
            component,
            "forward_remove_error",
            String::from_utf8_lossy(&output.stderr).trim(),
        ),
        Err(error) => {
            context
                .log
                .message(component, "forward_remove_error", &error.to_string());
        }
    }
}

/// Types text on the device.
///
/// Sent as characters rather than key codes on purpose: there is no Android key
/// code for "ñ" or "@", and the letter codes would still need the right meta
/// state for case. The agent hands the string to Android's KeyCharacterMap,
/// which produces exactly the events a physical keyboard would.
///
/// The record is the usual fixed header with `argument1` holding the UTF-8 byte
/// count, immediately followed by those bytes.
fn send_control_text(context: &Arc<ViewerContext>, text: &str) -> Result<(), String> {
    let payload = text.as_bytes();
    if payload.is_empty() || payload.len() > MAX_CONTROL_TEXT_BYTES {
        return Ok(());
    }
    send_control_record(
        context,
        CONTROL_KIND_TEXT,
        0,
        payload.len() as u32,
        0,
        0,
        payload,
    )
}

fn send_control(
    context: &Arc<ViewerContext>,
    kind: u8,
    action: u8,
    argument1: u32,
    argument2: u32,
    argument3: u32,
) -> Result<(), String> {
    send_control_record(context, kind, action, argument1, argument2, argument3, &[])
}

fn send_control_record(
    context: &Arc<ViewerContext>,
    kind: u8,
    action: u8,
    argument1: u32,
    argument2: u32,
    argument3: u32,
    payload: &[u8],
) -> Result<(), String> {
    let mut control = context
        .active_control
        .lock()
        .map_err(|_| "estado de control bloqueado".to_owned())?;
    let active = control
        .as_mut()
        .ok_or_else(|| "the control channel is not active".to_owned())?;
    if active
        .child
        .try_wait()
        .map_err(|error| format!("failed to query Android control: {error}"))?
        .is_some()
    {
        return Err("the Android control channel ended".to_owned());
    }
    let mut record = [0_u8; CONTROL_RECORD_SIZE];
    record[..4].copy_from_slice(&CONTROL_MAGIC);
    record[4] = CONTROL_VERSION;
    record[5] = kind;
    record[6] = action;
    record[8..12].copy_from_slice(&argument1.to_be_bytes());
    record[12..16].copy_from_slice(&argument2.to_be_bytes());
    record[16..20].copy_from_slice(&argument3.to_be_bytes());
    record[20..24].copy_from_slice(&active.sequence.to_be_bytes());
    active.sequence = active.sequence.wrapping_add(1);
    // Header and payload go out as ONE write: the agent reads the header and
    // then blocks for exactly `argument1` more bytes, so a reconnect landing
    // between the two would leave it waiting on a payload that never arrives.
    let mut frame = Vec::with_capacity(record.len() + payload.len());
    frame.extend_from_slice(&record);
    frame.extend_from_slice(payload);
    if let Err(first_error) = active
        .input
        .write_all(&frame)
        .and_then(|_| active.input.flush())
    {
        let _ = active.input.shutdown(Shutdown::Both);
        active.input = connect_control_socket(active.port, Duration::from_secs(1)).map_err(
            |reconnect_error| {
                format!(
                    "failed to send Android control ({first_error}) and to reconnect \
the socket ({reconnect_error})"
                )
            },
        )?;
        active
            .input
            .write_all(&frame)
            .and_then(|_| active.input.flush())
            .map_err(|error| {
                format!("failed to resend Android control after reconnecting: {error}")
            })?;
        context.log.event("control", "socket_reconnected", "");
    }
    if let Ok(mut metrics) = context.host_metrics.lock() {
        metrics.control_messages = metrics.control_messages.saturating_add(1);
    }
    Ok(())
}

fn request_keyframe(context: &Arc<ViewerContext>) -> Result<(), String> {
    let mut video = context
        .active_video
        .lock()
        .map_err(|_| "estado de captura bloqueado".to_owned())?;
    let active = video
        .as_mut()
        .ok_or_else(|| "the video is not active yet".to_owned())?;
    active
        .input
        .write_all(b"K")
        .and_then(|_| active.input.flush())
        .map_err(|error| format!("failed to request a keyframe: {error}"))
}

fn control_websocket(
    mut stream: TcpStream,
    context: Arc<ViewerContext>,
    key: &str,
) -> Result<(), String> {
    context.log.event("control", "websocket_open", "");
    let accept = websocket_accept(key);
    let response = format!(
        "HTTP/1.1 101 Switching Protocols\r\n\
Upgrade: websocket\r\n\
Connection: Upgrade\r\n\
Sec-WebSocket-Accept: {accept}\r\n\r\n"
    );
    stream
        .write_all(response.as_bytes())
        .map_err(|error| format!("failed to open the WebSocket: {error}"))?;
    let _ = stream.set_read_timeout(None);
    loop {
        let Some((opcode, payload)) = read_websocket_frame(&mut stream)? else {
            context.log.event("control", "websocket_closed", "");
            return Ok(());
        };
        match opcode {
            2 => handle_control_payload(&context, &payload)?,
            8 => {
                context.log.event("control", "websocket_closed", "");
                return Ok(());
            }
            9 => write_websocket_frame(&mut stream, 10, &payload)?,
            10 => {}
            _ => {}
        }
    }
}

fn handle_control_payload(context: &Arc<ViewerContext>, payload: &[u8]) -> Result<(), String> {
    let Some(kind) = payload.first().copied() else {
        return Ok(());
    };
    if kind == 1 && payload.len() == 10 {
        let action = payload[1];
        if action > 3 {
            return Ok(());
        }
        let x = f32::from_be_bytes(payload[2..6].try_into().unwrap());
        let y = f32::from_be_bytes(payload[6..10].try_into().unwrap());
        if !x.is_finite()
            || !y.is_finite()
            || !(0.0..=1.0).contains(&x)
            || !(0.0..=1.0).contains(&y)
        {
            return Ok(());
        }
        // Sent NORMALIZED (0..POINTER_SCALE), not in device pixels.
        //
        // Not scaled by the phone's physical size from `wm size`, which is
        // always portrait-shaped and never changes: on a turned phone every
        // tap would land in the wrong place, and anything past the shorter
        // side off the screen entirely. Only the agent knows where the display
        // is pointing right now, so only the agent can do this scaling.
        let device_x = (x * POINTER_SCALE as f32).round() as u32;
        let device_y = (y * POINTER_SCALE as f32).round() as u32;
        return send_control(context, CONTROL_KIND_POINTER, action, device_x, device_y, 0);
    }
    if kind == 6 && payload.len() == 18 {
        // [kind][action][x1 f32][y1 f32][x2 f32][y2 f32], all normalized
        // 0..1 same as the single-finger pointer above.
        let action = payload[1];
        if action > PINCH_CANCEL {
            return Ok(());
        }
        let x1 = f32::from_be_bytes(payload[2..6].try_into().unwrap());
        let y1 = f32::from_be_bytes(payload[6..10].try_into().unwrap());
        let x2 = f32::from_be_bytes(payload[10..14].try_into().unwrap());
        let y2 = f32::from_be_bytes(payload[14..18].try_into().unwrap());
        for value in [x1, y1, x2, y2] {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Ok(());
            }
        }
        let device_x1 = (x1 * POINTER_SCALE as f32).round() as u32;
        let device_y1 = (y1 * POINTER_SCALE as f32).round() as u32;
        // Packed into argument3 - see CONTROL_KIND_PINCH for why finger 2
        // has nowhere else to ride.
        let device_x2 = (x2 * POINTER_SCALE as f32).round() as u32;
        let device_y2 = (y2 * POINTER_SCALE as f32).round() as u32;
        let argument3 = (device_x2 << 16) | device_y2;
        return send_control(context, CONTROL_KIND_PINCH, action, device_x1, device_y1, argument3);
    }
    if kind == 7 && payload.len() == 13 {
        // [kind][x f32][y f32][delta f32] - no action byte, a wheel tick is
        // always the same one-shot event, never a down/move/up sequence.
        let x = f32::from_be_bytes(payload[1..5].try_into().unwrap());
        let y = f32::from_be_bytes(payload[5..9].try_into().unwrap());
        let delta = f32::from_be_bytes(payload[9..13].try_into().unwrap());
        if !x.is_finite()
            || !y.is_finite()
            || !delta.is_finite()
            || !(0.0..=1.0).contains(&x)
            || !(0.0..=1.0).contains(&y)
        {
            return Ok(());
        }
        let device_x = (x * POINTER_SCALE as f32).round() as u32;
        let device_y = (y * POINTER_SCALE as f32).round() as u32;
        // Fixed-point, signed - see CONTROL_KIND_SCROLL. The cast to u32
        // keeps the two's-complement bit pattern intact; the agent reads it
        // back with a plain signed readInt().
        let scaled_delta = (delta * SCROLL_FIXED_POINT).round() as i32;
        return send_control(
            context,
            CONTROL_KIND_SCROLL,
            0,
            device_x,
            device_y,
            scaled_delta as u32,
        );
    }
    if kind == 2 && payload.len() == 6 {
        let code = u32::from_be_bytes(payload[2..6].try_into().unwrap());
        // payload[1] is 1 for Ctrl+<key>: only the five editing shortcuts, so a
        // page cannot reach anything else on the phone with Ctrl held.
        if payload[1] == 1 {
            if is_ctrl_shortcut_key(code) {
                return send_control(context, CONTROL_KIND_KEY, 0, code, META_CTRL, 0);
            }
            return Ok(());
        }
        if is_allowed_key(code) {
            if code == KEYCODE_WAKEUP {
                wake_screen_if_stalled(context);
            }
            return send_control(context, CONTROL_KIND_KEY, 0, code, 0, 0);
        }
        return Ok(());
    }
    if kind == 3 {
        let _ = request_keyframe(context);
        return Ok(());
    }
    if kind == 5 && payload.len() == 2 && payload[1] <= 2 {
        // Panel power: 0 gives the screen back, 1 turns it off, 2 flips
        // whichever it is. The window sends 2, because the agent lets go of
        // the mode on its own when Android moves the display - so this side
        // must never assume it knows which way the screen currently is.
        let action = payload[1];
        context.log.message(
            "control",
            "panel_power",
            match action {
                0 => "on",
                1 => "off",
                _ => "toggle",
            },
        );
        return send_control(context, CONTROL_KIND_PANEL, action, 0, 0, 0);
    }
    if kind == 4 && payload.len() > 1 {
        // Typed text, UTF-8 after the opcode byte. Invalid sequences are
        // dropped rather than lossily converted: silently typing a replacement
        // character into someone's password field is worse than typing nothing.
        let Ok(text) = std::str::from_utf8(&payload[1..]) else {
            return Ok(());
        };
        return send_control_text(context, text);
    }
    if kind == 8 && payload.len() > 2 {
        // Ctrl+V: [8][flags][UTF-8 text]. The text goes on the phone's
        // clipboard and, with CLIPBOARD_PASTE, the agent pastes it. Same
        // rule as typed text for invalid sequences.
        let text = &payload[2..];
        if text.len() > MAX_CLIPBOARD_BYTES || std::str::from_utf8(text).is_err() {
            return Ok(());
        }
        let action = payload[1] & CLIPBOARD_PASTE;
        // The size only, never the text: these logs are kept.
        context
            .log
            .message("control", "clipboard_to_phone", &text.len().to_string());
        return send_control_record(
            context,
            CONTROL_KIND_CLIPBOARD,
            action,
            text.len() as u32,
            0,
            0,
            text,
        );
    }
    Ok(())
}

fn read_websocket_frame(stream: &mut TcpStream) -> Result<Option<(u8, Vec<u8>)>, String> {
    let mut first = [0_u8; 2];
    match stream.read_exact(&mut first) {
        Ok(()) => {}
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(format!("failed to read the WebSocket: {error}")),
    }
    if first[0] & 0x80 == 0 {
        return Err("WebSocket fragmentado no admitido".to_owned());
    }
    let opcode = first[0] & 0x0f;
    let masked = first[1] & 0x80 != 0;
    let mut length = u64::from(first[1] & 0x7f);
    if length == 126 {
        let mut extended = [0_u8; 2];
        stream
            .read_exact(&mut extended)
            .map_err(|error| error.to_string())?;
        length = u64::from(u16::from_be_bytes(extended));
    } else if length == 127 {
        let mut extended = [0_u8; 8];
        stream
            .read_exact(&mut extended)
            .map_err(|error| error.to_string())?;
        length = u64::from_be_bytes(extended);
    }
    if !masked || length > MAX_WEBSOCKET_PAYLOAD {
        return Err("trama WebSocket invalida".to_owned());
    }
    let mut mask = [0_u8; 4];
    stream
        .read_exact(&mut mask)
        .map_err(|error| error.to_string())?;
    let mut payload = vec![0_u8; length as usize];
    stream
        .read_exact(&mut payload)
        .map_err(|error| error.to_string())?;
    for (index, byte) in payload.iter_mut().enumerate() {
        *byte ^= mask[index % 4];
    }
    Ok(Some((opcode, payload)))
}

fn write_websocket_frame(stream: &mut TcpStream, opcode: u8, payload: &[u8]) -> Result<(), String> {
    if payload.len() > 125 {
        return Err("WebSocket response too large".to_owned());
    }
    let header = [0x80 | opcode, payload.len() as u8];
    stream
        .write_all(&header)
        .and_then(|_| stream.write_all(payload))
        .map_err(|error| format!("failed to answer the WebSocket: {error}"))
}

fn websocket_accept(key: &str) -> String {
    let mut input = key.as_bytes().to_vec();
    input.extend_from_slice(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    base64_encode(&sha1(&input))
}

fn sha1(input: &[u8]) -> [u8; 20] {
    let mut message = input.to_vec();
    let bit_length = (message.len() as u64) * 8;
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_length.to_be_bytes());
    let mut h = [
        0x67452301_u32,
        0xefcdab89,
        0x98badcfe,
        0x10325476,
        0xc3d2e1f0,
    ];
    for chunk in message.chunks_exact(64) {
        let mut words = [0_u32; 80];
        for (index, word) in words[..16].iter_mut().enumerate() {
            let offset = index * 4;
            *word = u32::from_be_bytes(chunk[offset..offset + 4].try_into().unwrap());
        }
        for index in 16..80 {
            words[index] =
                (words[index - 3] ^ words[index - 8] ^ words[index - 14] ^ words[index - 16])
                    .rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (index, word) in words.iter().enumerate() {
            let (function, constant) = match index {
                0..=19 => ((b & c) | ((!b) & d), 0x5a827999),
                20..=39 => (b ^ c ^ d, 0x6ed9eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1bbcdc),
                _ => (b ^ c ^ d, 0xca62c1d6),
            };
            let next = a
                .rotate_left(5)
                .wrapping_add(function)
                .wrapping_add(e)
                .wrapping_add(constant)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = next;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut output = [0_u8; 20];
    for (index, word) in h.iter().enumerate() {
        output[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    output
}

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let value = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        output.push(TABLE[((value >> 18) & 0x3f) as usize] as char);
        output.push(TABLE[((value >> 12) & 0x3f) as usize] as char);
        output.push(if chunk.len() > 1 {
            TABLE[((value >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            TABLE[(value & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    output
}

fn update_telemetry(context: &Arc<ViewerContext>, query: &str) {
    if let Ok(mut telemetry) = context.telemetry.lock() {
        telemetry.queue_depth = query_u32(query, "queue").unwrap_or(0).min(1000);
        telemetry.backlog_ms = query_u32(query, "backlog").unwrap_or(0).min(60_000);
        telemetry.recoveries = query_u32(query, "recoveries").unwrap_or(0);
        telemetry.rendered_fps = query_u32(query, "fps").unwrap_or(0).min(240);
        telemetry.packets = query_u64(query, "packets").unwrap_or(0);
        telemetry.decoded = query_u64(query, "decoded").unwrap_or(0);
        telemetry.rendered = query_u64(query, "rendered").unwrap_or(0);
        telemetry.dropped = query_u64(query, "dropped").unwrap_or(0);
        telemetry.pending_bytes = query_u32(query, "pending")
            .unwrap_or(0)
            .min(16 * 1024 * 1024);
        telemetry.packet_gap_ms = query_u32(query, "packet_gap").unwrap_or(0).min(60_000);
        telemetry.decode_gap_ms = query_u32(query, "decode_gap").unwrap_or(0).min(60_000);
        telemetry.render_gap_ms = query_u32(query, "render_gap").unwrap_or(0).min(60_000);
        telemetry.decode_p95_ms = query_u32(query, "decode_p95").unwrap_or(0).min(60_000);
        telemetry.render_p95_ms = query_u32(query, "render_p95").unwrap_or(0).min(60_000);
        telemetry.decoder_errors = query_u32(query, "decoder_errors").unwrap_or(0);
        telemetry.canvas_errors = query_u32(query, "canvas_errors").unwrap_or(0);
        telemetry.source_freezes = query_u32(query, "source_freezes").unwrap_or(0);
        telemetry.render_freezes = query_u32(query, "render_freezes").unwrap_or(0);
        telemetry.audio_bytes = query_u64(query, "audio_bytes").unwrap_or(0);
        telemetry.audio_buffer_ms = query_u32(query, "audio_buffer").unwrap_or(0).min(10_000);
        telemetry.audio_drops = query_u64(query, "audio_drops").unwrap_or(0);
        telemetry.control_reconnects = query_u32(query, "control_reconnects").unwrap_or(0);
        telemetry.raf_ticks = query_u32(query, "raf_ticks").unwrap_or(0).min(1_000);
        telemetry.raf_worst_gap_ms = query_u32(query, "raf_worst_gap").unwrap_or(0).min(60_000);
        telemetry.hidden = query_u32(query, "hidden").unwrap_or(0).min(1);
        telemetry.focused = query_u32(query, "focused").unwrap_or(0).min(1);
    }
}

fn respond(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> Result<(), String> {
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\n\
Content-Length: {}\r\nCache-Control: no-store\r\n\
X-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(header.as_bytes())
        .and_then(|_| stream.write_all(body))
        .map_err(|error| format!("failed to answer the viewer: {error}"))
}

fn query_value<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|part| {
        let (candidate, value) = part.split_once('=')?;
        (candidate == key).then_some(value)
    })
}

fn query_u32(query: &str, key: &str) -> Option<u32> {
    query_value(query, key)?.parse().ok()
}

fn query_u64(query: &str, key: &str) -> Option<u64> {
    query_value(query, key)?.parse().ok()
}

/// Waking a phone whose injected WAKEUP did not take.
///
/// The window sends KEYCODE_WAKEUP down the control channel on every press,
/// which costs nothing and is idempotent. But that path is an event injected by
/// the agent, and the one PROVEN to wake this phone is the shell command the
/// session already runs when it opens - which is why restarting the mirror
/// would otherwise be the way to get the screen back.
///
/// So the shell command is kept as a backstop, spent only when no picture has
/// arrived for a while. That signal cannot tell a sleeping screen from a still
/// one, and it does not have to: on a still screen the worst that happens is
/// one harmless idempotent command, not a wrong action.
fn wake_screen_if_stalled(context: &Arc<ViewerContext>) {
    const STALLED_MS: u64 = 3000;
    const MIN_GAP_BETWEEN_SHELL_WAKES: Duration = Duration::from_secs(5);
    let now_ms = context.started.elapsed().as_millis() as u64;
    let stalled = match context.host_metrics.lock() {
        Ok(metrics) => {
            let last = metrics.last_video_read_elapsed_ms;
            last == 0 || now_ms.saturating_sub(last) >= STALLED_MS
        }
        Err(_) => false,
    };
    if !stalled {
        return;
    }
    // Second brake, on top of the window's own: tapping away at a screen that
    // simply is not moving must not turn into one process spawn per press.
    match context.last_shell_wake.lock() {
        Ok(mut last) => {
            if last.is_some_and(|at: Instant| at.elapsed() < MIN_GAP_BETWEEN_SHELL_WAKES) {
                return;
            }
            *last = Some(Instant::now());
        }
        Err(_) => return,
    }
    // On its own thread: this is a process spawn plus a round trip, and it must
    // not hold up the control channel that the same press is travelling down.
    let context = Arc::clone(context);
    thread::spawn(move || {
        let _ = adb_command(&context.adb)
            .args([
                "-s",
                &context.serial(),
                "shell",
                "input",
                "keyevent",
                "224",
            ])
            .output();
        context.log.event("control", "wake_shell", "");
    });
}

fn is_allowed_key(code: u32) -> bool {
    matches!(
        code,
        3        // HOME
        | 4      // BACK
        | 19..=23 // DPAD up/down/left/right/center
        | 24 | 25 // VOLUME up/down
        | 26     // POWER
        | 61     // TAB
        | 62     // SPACE
        | 66     // ENTER
        | 67     // DEL (backspace)
        | 92 | 93 // PAGE_UP / PAGE_DOWN
        | 111    // ESCAPE
        | 112    // FORWARD_DEL (delete)
        | 122 | 123 // MOVE_HOME / MOVE_END
        | 187    // APP_SWITCH (recents)
        | 224    // WAKEUP
    )
}

/// The keys that may travel with Ctrl held: A (select all), C (copy),
/// V (paste), X (cut), Z (undo) - Android key codes.
fn is_ctrl_shortcut_key(code: u32) -> bool {
    matches!(code, 29 | 31 | 50 | 52 | 54)
}

fn read_device_size(adb: &AdbClient, serial: &str) -> Result<(u32, u32), String> {
    let result = adb.run(&["-s", serial, "shell", "wm", "size"])?;
    if !result.succeeded() {
        return Err(format!(
            "could not read the screen size: {}",
            result.combined_output()
        ));
    }
    for token in result.stdout.split_ascii_whitespace().rev() {
        if let Some((width, height)) = token.split_once('x')
            && let (Ok(width), Ok(height)) = (width.parse::<u32>(), height.parse::<u32>())
            && width > 0
            && height > 0
        {
            return Ok((width, height));
        }
    }
    Err(format!(
        "Android returned an unknown screen size: {}",
        result.stdout.trim()
    ))
}

fn quality_capture_size(width: u32, height: u32) -> (u32, u32) {
    const MAX_LONG_EDGE: u32 = 2340;
    let long_edge = width.max(height);
    if long_edge <= MAX_LONG_EDGE {
        return (width & !1, height & !1);
    }
    let scaled = |value: u32| {
        let result = (u64::from(value) * u64::from(MAX_LONG_EDGE) / u64::from(long_edge)) as u32;
        result.max(2) & !1
    };
    (scaled(width), scaled(height))
}

fn read_device_name(adb: &AdbClient, serial: &str) -> String {
    if let Ok(result) = adb.run(&["-s", serial, "shell", "getprop", "ro.product.model"])
        && result.succeeded()
    {
        let name: String = result
            .stdout
            .trim()
            .chars()
            .filter(|character| !character.is_control())
            .take(80)
            .collect();
        if !name.is_empty() && name != "null" {
            return name;
        }
    }
    serial.chars().take(80).collect()
}

fn deploy_stream_agent(
    adb: &AdbClient,
    serial: &str,
    remote_path: &str,
    token: &str,
) -> Result<(), String> {
    let local_agent = std::env::temp_dir().join(format!("mirryl-connect-agent-{token}.jar"));
    std::fs::write(&local_agent, MIRRYL_STREAM_AGENT)
        .map_err(|error| format!("could not prepare the Android agent: {error}"))?;
    let local = local_agent.to_string_lossy().into_owned();
    let result = adb.run(&["-s", serial, "push", &local, remote_path]);
    let _ = std::fs::remove_file(&local_agent);
    let result = result?;
    if !result.succeeded() {
        return Err(format!(
            "could not deploy the Android agent: {}",
            result.combined_output()
        ));
    }
    Ok(())
}

fn cleanup_stream_agent(adb: &AdbClient, serial: &str, remote_path: &str) {
    // The jar and every agent's stderr file next to it (see
    // `agent_stderr_path`); the phone's shell expands the pattern.
    let stderr_files = format!("{remote_path}.*.err");
    let _ = adb.run(&["-s", serial, "shell", "rm", "-f", remote_path, &stderr_files]);
}

/// Removes what earlier sessions left in the agent's directory on the phone:
/// their jar and the `.err` files next to it (see `agent_stderr_path`).
/// `cleanup_stream_agent` takes those away when a session ends properly; when
/// it does not - the process killed, the PC switched off, the cable pulled for
/// good - they stay, one family per session, for ever: dozens of jars on one
/// phone, ~25 KB each.
///
/// Only files whose token no process on the phone carries in its command line
/// are removed. Every agent is launched with its session token as an argument
/// (`kill_remote_agents` relies on the same fact), so a token absent from `ps`
/// means no agent of that session is alive; a session whose agents ARE alive -
/// a second window on the same phone, say - keeps its jar, which it needs to
/// relaunch an agent after a dropped link. This session's own files are left
/// alone by name as well, and when either listing cannot be read nothing is
/// removed at all: a doubt costs a few kilobytes, a wrong guess costs another
/// window its reconnect. What remains are the moments when another session on
/// the same phone has no agent alive: between its push and its first agent,
/// between a pkill and the relaunch on reconnect, or after its control agent
/// left on its own idle timeout. The jar is pushed once per session and
/// relaunches reuse it, so a sweep in one of those moments would cost that
/// session its reconnect. Two windows on one phone is not a supported setup
/// (the app keeps one window per device), which is why this is accepted
/// rather than solved by re-pushing the jar on every relaunch.
///
/// Runs before this session's own push, so the two never compete; two adb
/// round trips when there is nothing to remove.
fn sweep_stale_agents(adb: &AdbClient, serial: &str, token: &str, log: &SessionLog) {
    let run = |arguments: &[&str]| match adb.run(arguments) {
        Ok(result) if result.succeeded() => Some(result.stdout),
        Ok(result) => {
            log.message("session", "agent_sweep_error", &result.combined_output());
            None
        }
        Err(error) => {
            log.message("session", "agent_sweep_error", &error);
            None
        }
    };
    let Some(listing) = run(&["-s", serial, "shell", "ls", REMOTE_AGENT_DIRECTORY]) else {
        return;
    };
    // -w is what keeps toybox's ps from cutting lines at 80 columns, and the
    // token sits past that on the video agent's line. The check below is
    // weaker than that: it only proves ps printed arguments at all (its own
    // `-o ARGS` is short enough to survive truncation). A ps that showed only
    // process names would otherwise clear every token at once.
    let Some(processes) = run(&["-s", serial, "shell", "ps", "-A", "-w", "-o", "ARGS"]) else {
        return;
    };
    if !processes.contains("-o ARGS") {
        log.message(
            "session",
            "agent_sweep_error",
            "ps did not show whole command lines",
        );
        return;
    }
    let stale = stale_agent_files(&listing, &processes, token);
    // `removed` counts the files of the batches rm accepted whole: rm -f keeps
    // going past an operand it cannot remove but exits non-zero, so a batch
    // with one such file counts as zero. A lower bound, not a tally - the
    // number that matters is the listing at the next session's sweep.
    let mut removed = 0;
    // A few files per call: adb sends the whole command line to the phone in
    // one packet, and the first sweep of a phone can have dozens of them.
    for batch in stale.chunks(16) {
        let mut arguments = vec!["-s", serial, "shell", "rm", "-f"];
        arguments.extend(batch.iter().map(String::as_str));
        if run(&arguments).is_some() {
            removed += batch.len();
        }
    }
    // The PC side of the same leak: a session that did not end properly also
    // leaves its three `adb forward` listeners in the adb server, one per
    // agent, still listed the next morning. Same rule as the files - a token no process
    // on the phone carries is a session that is gone - and same lower bound.
    let (forwards_stale, mut forwards_removed) = (
        run(&["-s", serial, "forward", "--list"])
            .map(|list| stale_forwards(&list, serial, &processes, token))
            .unwrap_or_default(),
        0,
    );
    for local in &forwards_stale {
        if run(&["-s", serial, "forward", "--remove", local]).is_some() {
            forwards_removed += 1;
        }
    }
    log.event(
        "session",
        "agent_sweep",
        &format!(
            "\"stale\":{},\"removed\":{removed},\"forwards_stale\":{},\"forwards_removed\":{forwards_removed}",
            stale.len(),
            forwards_stale.len()
        ),
    );
}

/// Which `adb forward --list` entries the sweep may remove, as their local
/// side (`tcp:NNN`): those on this transport whose remote side is one of this
/// engine's sockets (`localabstract:mirryl_<kind>_<token>`) and whose token
/// is neither this session's nor carried by any listed process. Lines are
/// `<serial> <local> <remote>`, and the serial can contain a space (an mDNS
/// rename, error 47), so they are read from the right.
fn stale_forwards(list: &str, serial: &str, processes: &str, own_token: &str) -> Vec<String> {
    list.lines()
        .filter_map(|line| {
            let (device, local, remote) = split_last_two_fields(line)?;
            if device != serial || !local.starts_with("tcp:") {
                return None;
            }
            let socket = remote.strip_prefix("localabstract:mirryl_")?;
            let token = socket.rsplit('_').next().unwrap_or("");
            if token.is_empty() || token == own_token || processes.contains(token) {
                return None;
            }
            Some(local.to_owned())
        })
        .collect()
}

/// Which of the files listed in the agent's directory the sweep may remove:
/// those with the agent's prefix whose token is neither this session's nor
/// carried by any of the listed processes. Full paths, in listing order.
fn stale_agent_files(listing: &str, processes: &str, own_token: &str) -> Vec<String> {
    listing
        .split_whitespace()
        .filter(|name| {
            // The names this code writes are hex, dots, dashes and class
            // names. adb re-joins argv and the phone's shell expands it, so a
            // name with a glob character - which nothing here creates - would
            // become a pattern over the live files; it is left alone.
            name.chars()
                .all(|character| character.is_ascii_alphanumeric() || ".-_".contains(character))
        })
        .filter_map(|name| {
            let token = name.strip_prefix(REMOTE_AGENT_PREFIX)?;
            let token = token.split('.').next().unwrap_or("");
            // An empty token is "carried" by everything; keeping such a file
            // is the right answer for a name this code never writes.
            if token.is_empty() || token == own_token || processes.contains(token) {
                return None;
            }
            Some(format!("{REMOTE_AGENT_DIRECTORY}/{name}"))
        })
        .collect()
}

/// The secret in the viewer's URL: 20 bytes from the OS CSPRNG, as 40 hex
/// characters. This is the only thing that gates the local HTTP server, so it
/// has to be unguessable; the previous clock-plus-PID value was the same
/// length but nearly reconstructible from the session log's file name
/// (unix_ms and pid). The length is kept so nothing built on the format -
/// remote_agent_path, the page, the logs - has to change.
fn session_token() -> Result<String, String> {
    let mut bytes = [0u8; 20];
    getrandom::getrandom(&mut bytes)
        .map_err(|error| format!("could not generate the session token: {error}"))?;
    let mut token = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        token.push_str(&format!("{byte:02x}"));
    }
    Ok(token)
}

// ---------- The link to the phone ----------

/// What the window needs to draw its overlay: whether the phone is there, over
/// which kind of link, how long is left before this window closes itself, and
/// why the last attempt failed.
fn link_status_json(context: &Arc<ViewerContext>) -> String {
    let serial = context.serial();
    // Offered only while the mirror is actually WORKING: with the phone gone
    // the reconnect panel owns the window, and two banners at once would ask the
    // user to solve two problems that are really one.
    let usb_offer = context
        .usb_offer
        .lock()
        .map(|offer| offer.available && !offer.dismissed)
        .unwrap_or(false);
    let (down_since, attempting, last_error) = match context.link.lock() {
        Ok(link) => (link.down_since, link.attempting, link.last_error.clone()),
        Err(_) => (None, false, None),
    };
    // While an attempt runs the clock is stopped, so report the full minute
    // rather than a countdown that appears frozen for 30 seconds.
    let closing_in_ms = match (down_since, attempting) {
        (Some(since), false) => LINK_IDLE_CLOSE.saturating_sub(since.elapsed()).as_millis() as u64,
        (Some(_), true) => LINK_IDLE_CLOSE.as_millis() as u64,
        (None, _) => 0,
    };
    format!(
        "{{\"state\":\"{}\",\"kind\":\"{}\",\"attempting\":{},\
\"closing_in_ms\":{},\"wifi_paired\":{},\"usb_offer\":{},\"error\":\"{}\"}}",
        if down_since.is_some() { "down" } else { "up" },
        if is_usb_transport(&serial) {
            "usb"
        } else {
            "wifi"
        },
        attempting,
        closing_in_ms,
        context.wifi_address.is_some(),
        usb_offer,
        crate::diagnostics::json_escape(last_error.as_deref().unwrap_or(""))
    )
}

impl ViewerContext {
    /// The transport to hand to `adb -s`. Always read through here: it changes
    /// under everyone's feet when a reconnect moves the session to the other
    /// kind of link.
    fn serial(&self) -> String {
        match self.serial.lock() {
            Ok(serial) => serial.clone(),
            // A poisoned lock means some other thread panicked mid-session.
            // The launch transport is the best guess left and beats panicking
            // again on top of it.
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn link_is_down(&self) -> bool {
        self.link
            .lock()
            .map(|link| link.down_since.is_some())
            .unwrap_or(false)
    }
}

/// A wireless transport is either `ip:port` or the mDNS name
/// (`adb-<SERIAL>-xxxx._adb-tls-connect._tcp`). A USB one is the bare hardware
/// serial, with neither a colon nor the `_tcp` suffix. Same rule the app uses.
fn is_usb_transport(name: &str) -> bool {
    !name.contains(':') && !name.ends_with("._tcp")
}

/// The states adb prints after a transport name in `adb devices`.
const ADB_STATES: [&str; 10] = [
    "device",
    "offline",
    "unauthorized",
    "authorizing",
    "connecting",
    "bootloader",
    "recovery",
    "sideload",
    "host",
    "no permissions",
];

/// One line of `adb devices` / `adb devices -l`, split into (transport, state).
///
/// The transport name is NOT one whitespace-free token: when a phone
/// re-announces Wireless debugging while its old announcement is still alive,
/// mDNS renames it to `adb-<serial>-xxxx (2)._adb-tls-connect._tcp`, with a
/// SPACE. Plain `adb devices` separates the
/// columns with a tab but `-l` pads with spaces, so find the state keyword and
/// take everything before it. Same function as the app's main.rs, and the same
/// rule as `parseAdbLine` in its main.js. Header and adb chatter give None.
fn parse_adb_device_line(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    let mut start = 0;
    while let Some(offset) = line[start..].find(char::is_whitespace) {
        let gap = start + offset;
        let rest = line[gap..].trim_start();
        for state in ADB_STATES {
            if let Some(after) = rest.strip_prefix(state) {
                if after.is_empty() || after.starts_with(char::is_whitespace) {
                    let name = line[..gap].trim_end();
                    if !name.is_empty() {
                        return Some((name, state));
                    }
                }
            }
        }
        start = line.len() - rest.len();
    }
    None
}

/// A line of three columns whose FIRST may contain spaces, read from the
/// right: `adb mdns services` (`<instance>\t<service>\t<ip:port>`) and
/// `adb forward --list` (`<serial> <local> <remote>`). The last two columns
/// never contain whitespace; the first does after an mDNS rename. Same
/// function as the app's main.rs.
fn split_last_two_fields(line: &str) -> Option<(&str, &str, &str)> {
    let (rest, last) = line.trim().rsplit_once(char::is_whitespace)?;
    let (first, middle) = rest.trim_end().rsplit_once(char::is_whitespace)?;
    let first = first.trim_end();
    if first.is_empty() || middle.is_empty() || last.is_empty() {
        return None;
    }
    Some((first, middle, last))
}

/// The transports in an `adb devices` listing that are ready to be used.
///
/// Only `device` counts, never `offline` or `unauthorized`, and on `-l` output
/// the trailing `device:m2q` field must not be mistaken for the state. The name
/// can contain a space, so the line is split at the state, not at the first
/// space: `parse_adb_device_line`.
fn parse_transports(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter_map(parse_adb_device_line)
        .filter(|(_, state)| *state == "device")
        .map(|(name, _)| name.to_owned())
        .collect()
}

/// Every transport adb currently has in the `device` state.
fn list_transports(adb: &std::path::Path) -> Vec<String> {
    let Ok(output) = adb_command(adb).args(["devices"]).output() else {
        return Vec::new();
    };
    parse_transports(&String::from_utf8_lossy(&output.stdout))
}

/// The outcome of looking for one particular phone in a listing.
#[derive(Debug, PartialEq)]
enum TransportPick {
    /// The name itself claims it: a USB transport is normally the hardware
    /// serial, and a wireless mDNS name is built from it
    /// (`adb-<SERIAL>-xxxx._adb-tls-connect._tcp`), so the serial is PARSED out
    /// of the name and compared whole - a name that merely contains ours is not
    /// ours. Nothing is asked of a `Certain`, so this bar has to be exact.
    Certain(String),
    /// Names that could be this phone but do not say so - a bare `ip:port`
    /// identifies nobody. Every one of them has to be asked, in order, because
    /// with two phones on the network the first is not necessarily ours.
    Ask(Vec<String>),
    None,
}

/// Pick this phone's transport of the requested kind out of a listing.
///
/// Never guesses. A name that does not prove whose it is becomes a question for
/// the phone, not an assumption - mirroring somebody else's screen because two
/// devices were on the bench would be a bad way to find out.
///
/// The name has to EQUAL our serial, or PARSE to it. A substring test would let
/// a stranger through without a single question: `adb devices` lists wireless
/// transports as `adb-<SERIAL>-<token>._adb-tls-connect._tcp`, the advertiser
/// picks that name, and `adb-XR5CX12ABCDEX-9xQq11._adb-tls-connect._tcp`
/// contains `R5CX12ABCDE`. That would come back `Certain`, and `find_transport`
/// never asks a `Certain` who it is - only the `Ask` arm calls
/// `transport_hardware_serial`. Anything that is not exactly ours falls to
/// `Ask`, where the phone itself answers.
fn match_transport(transports: &[String], hardware_serial: &str, want_usb: bool) -> TransportPick {
    let mut ask = Vec::new();
    for transport in transports {
        if is_usb_transport(transport) != want_usb {
            continue;
        }
        // The bare-equality arm still matters: on a degraded link the "serial"
        // we were given can be an `ip:port`, and then the transport name is it.
        if transport == hardware_serial
            || mdns_instance_serial(transport) == Some(hardware_serial)
        {
            return TransportPick::Certain(transport.clone());
        }
        ask.push(transport.clone());
    }
    if ask.is_empty() {
        TransportPick::None
    } else {
        TransportPick::Ask(ask)
    }
}

/// This phone's CURRENT wireless address, out of an `adb mdns services` listing.
///
/// Lines look like:
///   `adb-R5CX12ABCDE-62Sel0   _adb-tls-connect._tcp   192.168.0.4:44065`
///
/// The pairing service (`_adb-tls-pairing`) advertises itself the same way and
/// is a different port that refuses connections, so the service name has to be
/// checked, not just the serial.
///
/// The line is read by field, and the serial parsed out of the instance name
/// has to EQUAL ours: anyone on the Wi-Fi chooses the name they advertise, so a
/// substring match (`adb-XR5CX12ABCDEX-...` contains `R5CX12ABCDE`) would let a
/// stranger's name pass for this phone's.
///
/// An exact name is still only a name - the serial is broadcast in the clear,
/// so anyone within earshot can advertise it verbatim. Do not read the rest of
/// the code as making up for that: `find_transport` asks
/// `transport_hardware_serial` only on the `Ask` arm, so a transport whose name
/// parses to our serial is taken on the name alone. What the whole-serial rule
/// buys is that a name has to CLAIM to be this phone, which is a deliberate
/// forgery rather than the accident of a common substring.
fn parse_mdns_address(listing: &str, hardware_serial: &str) -> Option<String> {
    listing.lines().find_map(|line| {
        // From the right: a renamed instance (`adb-<serial>-xxxx (2)`) has a
        // space in it, and splitting on whitespace skipped that line - which
        // is the phone's CURRENT address - and fell through to the stale one.
        let (instance, service, address) = split_last_two_fields(line)?;
        // Real listings print the bare type; tolerate a trailing dot in case
        // an adb build prints the fully qualified one.
        if service.trim_end_matches('.') != "_adb-tls-connect._tcp" {
            return None;
        }
        if mdns_instance_serial(instance) != Some(hardware_serial) {
            return None;
        }
        address.contains(':').then(|| address.to_owned())
    })
}

/// The hardware serial an `_adb-tls-connect` instance name claims:
/// `adb-<serial>-<token>` -> `<serial>`. Same rule as `parse_adb_serial` in the
/// app's qr_pairing.rs: drop the `adb-` prefix, cut at the LAST `-` (the token
/// after it is a per-session random suffix; Android serials carry no `-`).
/// A fully qualified name (`...._adb-tls-connect._tcp.local.`) is trimmed to
/// its first label first. None for anything that does not fit the shape.
fn mdns_instance_serial(instance: &str) -> Option<&str> {
    let label = instance.split('.').next().unwrap_or(instance);
    let rest = label.strip_prefix("adb-")?;
    let (serial, _token) = rest.rsplit_once('-')?;
    (!serial.is_empty()).then_some(serial)
}

/// Ask adb where this phone is on the network right now.
///
/// This is what makes reconnecting over Wi-Fi quick. The address the app has on
/// file goes stale as a matter of routine - the port changes every time
/// wireless debugging restarts, and the session logs show three different ones
/// for the same phone - so dialling the saved one usually fails, and the
/// reconnect was left waiting out its 30 seconds for adb's own discovery to get
/// round to it. That is what made Wi-Fi need two or three tries while USB was
/// instant. Asking outright costs about 80 ms.
fn mdns_address(adb: &std::path::Path, hardware_serial: &str) -> Option<String> {
    let output = adb_command(adb).args(["mdns", "services"]).output().ok()?;
    parse_mdns_address(&String::from_utf8_lossy(&output.stdout), hardware_serial)
}

/// The phone behind a transport. `getprop`, not `adb get-serialno`: over a
/// wireless transport the latter echoes the transport name straight back.
fn transport_hardware_serial(adb: &std::path::Path, transport: &str) -> Option<String> {
    let output = adb_command(adb)
        .args(["-s", transport, "shell", "getprop", "ro.serialno"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!value.is_empty()).then_some(value)
}

/// Find this same phone on a transport of the requested kind, if it is there.
///
/// Matching is by hardware serial, which is the only thing that survives the
/// change of link. The wireless name usually carries it, so that costs nothing;
/// a bare `ip:port` does not, and only then is the phone asked.
fn find_transport(context: &Arc<ViewerContext>, want_usb: bool) -> Option<String> {
    let transports = list_transports(&context.adb);
    match match_transport(&transports, &context.hardware_serial, want_usb) {
        TransportPick::Certain(transport) => Some(transport),
        // Asking is left until nothing cheaper matched, because each question
        // costs a round trip to a phone.
        TransportPick::Ask(candidates) => candidates.into_iter().find(|candidate| {
            transport_hardware_serial(&context.adb, candidate).as_deref()
                == Some(context.hardware_serial.as_str())
        }),
        TransportPick::None => None,
    }
}

/// Record that the phone is gone: the window puts up its overlay and the close
/// countdown starts. Called only after adb confirms the transport really is
/// missing, so a stream that failed for some other reason does not end the
/// session.
fn mark_link_down(context: &Arc<ViewerContext>) {
    if let Ok(mut link) = context.link.lock()
        && link.down_since.is_none()
    {
        link.down_since = Some(Instant::now());
        context.log.event(
            "link",
            "down",
            &format!(
                "\"transport\":\"{}\"",
                crate::diagnostics::json_escape(&context.serial())
            ),
        );
    }
}

/// A stream failed to start. Tell "the phone is gone" apart from "that one
/// attempt went wrong" - only the first should stop the session.
fn note_agent_failure(context: &Arc<ViewerContext>) {
    let serial = context.serial();
    if !list_transports(&context.adb).iter().any(|name| *name == serial) {
        mark_link_down(context);
    }
}

/// Tell the app which link this session is on.
///
/// Written at start and again on every change, so the device list can describe
/// the MIRROR's link rather than the phone's best available one. Those two stop
/// being the same thing the moment a cable is plugged into a Wi-Fi session, and
/// showing the second while the first is true is what made the list claim a USB
/// mirror that did not exist.
///
/// Best effort: a session that cannot write the file still mirrors fine, it
/// just cannot correct the icon.
fn publish_transport(context: &Arc<ViewerContext>) {
    let Some(path) = &context.transport_file else {
        return;
    };
    let kind = if is_usb_transport(&context.serial()) {
        "usb"
    } else {
        "wifi"
    };
    let _ = std::fs::write(path, kind);
}

/// Look for a cable while running over Wi-Fi, and remember whether one is there.
///
/// Cheap on purpose: `adb devices` only, and only while the offer is still
/// unanswered. It accepts a match ONLY when the transport name itself proves
/// the phone (`Certain`) - asking an `ip:port` candidate costs a round trip to
/// the device, and this runs on a timer, so an offer nobody asked for must
/// never become repeated traffic to the phone.
fn refresh_usb_offer(context: &Arc<ViewerContext>) {
    // Nothing to offer when already on the cable, or when the phone is gone -
    // that second case belongs to the reconnect panel, not to this.
    if is_usb_transport(&context.serial()) || context.link_is_down() {
        return;
    }
    if context
        .usb_offer
        .lock()
        .map(|offer| offer.dismissed)
        .unwrap_or(true)
    {
        return;
    }
    let transports = list_transports(&context.adb);
    let available = matches!(
        match_transport(&transports, &context.hardware_serial, true),
        TransportPick::Certain(_)
    );
    if let Ok(mut offer) = context.usb_offer.lock()
        && offer.available != available
    {
        offer.available = available;
        context.log.event(
            "link",
            if available {
                "usb_offer_available"
            } else {
                "usb_offer_gone"
            },
            "",
        );
    }
}

/// Move a RUNNING session onto the cable.
///
/// Not a reconnect: nothing is broken. The phone is reachable on both links, so
/// this is a deliberate hand-over of a working mirror.
///
/// The order matters, and the two halves need opposite treatment. Control and
/// logcat are opened once per session, so they must be rebuilt on the new
/// transport by hand. Video and audio are the reverse: the window re-requests
/// them, so the way to move them is to tear the current ones down and let it
/// ask again, which it does within a second. Shutting the socket is what
/// unblocks their relay loops - killing the adb child alone leaves them parked
/// in a blocking read.
fn switch_to_usb(context: &Arc<ViewerContext>) -> Result<String, (&'static str, String)> {
    let transports = list_transports(&context.adb);
    let TransportPick::Certain(usb) = match_transport(&transports, &context.hardware_serial, true)
    else {
        return Err((
            "not_found",
            "The cable is not connected any more. Nothing changed - this window is still on Wi-Fi."
                .to_owned(),
        ));
    };
    let old_serial = context.serial();
    if let Ok(mut serial) = context.serial.lock() {
        *serial = usb.clone();
    }
    context.log.event(
        "link",
        "switched_to_usb",
        &format!(
            "\"from\":\"{}\"",
            crate::diagnostics::json_escape(&old_serial)
        ),
    );
    resume_after_reconnect(context, &old_serial);
    publish_transport(context);
    if let Ok(mut video) = context.active_video.lock()
        && let Some(active) = video.as_mut()
    {
        let _ = active.input.shutdown(Shutdown::Both);
        let _ = active.child.kill();
    }
    if let Ok(mut audio) = context.active_audio.lock()
        && let Some(active) = audio.as_mut()
    {
        let _ = active.stream.shutdown(Shutdown::Both);
        let _ = active.child.kill();
    }
    // Answered: stop watching, stop offering.
    if let Ok(mut offer) = context.usb_offer.lock() {
        offer.available = false;
        offer.dismissed = true;
    }
    Ok(usb)
}

/// Reconnect on the requested kind of link, for up to `RECONNECT_TIMEOUT`.
/// Returns the transport it landed on, or a `(code, message)` for the window.
fn attempt_reconnect(
    context: &Arc<ViewerContext>,
    want_usb: bool,
) -> Result<String, (&'static str, String)> {
    // Nothing to try, and no point making the user watch 30 seconds of nothing
    // to be told so: the app never gave us an address for this phone.
    if !want_usb && context.wifi_address.is_none() {
        return Err((
            "unpaired",
            "This phone isn't paired for Wi-Fi. Pair it in Mirryl first.".to_owned(),
        ));
    }
    // Kept for the teardown of the old control channel: adb needs the transport
    // a forward was created on in order to drop it.
    let old_serial = context.serial();
    if let Ok(mut link) = context.link.lock() {
        // `try_auto_switch` may have got there first. Already
        // back: say so instead of tearing a working session down again. Still
        // landing: do not start a second hand-over beside it - the window's
        // poll sees the link come back on its own a moment later.
        if link.down_since.is_none() {
            drop(link);
            return Ok(context.serial());
        }
        if link.attempting {
            return Err((
                "busy",
                "Already moving this window to the phone's other connection…".to_owned(),
            ));
        }
        link.attempting = true;
        link.last_error = None;
    }
    context.log.event(
        "link",
        "reconnect_starting",
        &format!("\"via\":\"{}\"", if want_usb { "usb" } else { "wifi" }),
    );

    let deadline = Instant::now() + RECONNECT_TIMEOUT;
    let mut last_dial: Option<(String, Instant)> = None;
    let outcome = loop {
        if context.stopping.load(Ordering::SeqCst) {
            break Err(("stopped", "The session was closed.".to_owned()));
        }
        if let Some(transport) = find_transport(context, want_usb) {
            break Ok(transport);
        }
        // Over Wi-Fi the phone is usually reachable but not connected, and
        // `adb connect` is what puts it in the list. Where to dial comes from
        // discovery first and only then from the address on file, which is
        // stale more often than not.
        //
        // Retried, unlike before, but not hammered: the same address is dialled
        // again only every couple of seconds, and a newly discovered one right
        // away. The phone often turns up on the network a few seconds in - and
        // waiting through that is exactly what this loop is for.
        if !want_usb
            && let Some(address) = mdns_address(&context.adb, &context.hardware_serial)
                .or_else(|| context.wifi_address.clone())
        {
            let due = match &last_dial {
                Some((dialled, at)) => {
                    *dialled != address || at.elapsed() >= Duration::from_secs(2)
                }
                None => true,
            };
            if due {
                last_dial = Some((address.clone(), Instant::now()));
                let _ = adb_command(&context.adb).args(["connect", &address]).output();
                // Straight back to the listing: a successful dial shows up
                // there immediately, and waiting half a second on top of it is
                // half a second of the user watching a spinner for nothing.
                continue;
            }
        }
        if Instant::now() >= deadline {
            break Err(if want_usb {
                (
                    "not_found",
                    "Couldn't find the phone over USB. Check that the cable is plugged in and USB debugging is still on."
                        .to_owned(),
                )
            } else {
                (
                    "not_found",
                    "Couldn't connect over Wi-Fi. Check that the phone is on the same network with Wireless debugging on."
                        .to_owned(),
                )
            });
        }
        thread::sleep(Duration::from_millis(500));
    };

    match &outcome {
        Ok(transport) => land_on_transport(context, transport, &old_serial, "click"),
        Err((code, message)) => {
            if let Ok(mut link) = context.link.lock() {
                link.attempting = false;
                link.last_error = Some(message.clone());
                // Losing an attempt restarts the countdown rather than letting
                // the window vanish the moment the user was told it failed.
                link.down_since = Some(Instant::now());
            }
            context.log.message("link", "reconnect_failed", code);
        }
    }
    outcome
}

/// Put a session whose link dropped onto `transport`, the same phone on a link
/// that works. Shared by the reconnect a person asks for ("click") and the one
/// the engine does by itself ("auto"), so the two cannot drift apart.
///
/// The link is marked up BEFORE the agents are restarted, and it has to be:
/// `start_socket_agent` refuses to touch adb while the link is down, and the
/// other order would bring every automatic switch back with picture and no
/// touch (a `control/restart_error` saying the phone is not connected). A
/// window that does open control a moment early just retries every 300 ms
/// until the agent is
/// there. `attempting` stays set until the end, which holds the close
/// countdown and keeps a click from starting a second hand-over.
fn land_on_transport(context: &Arc<ViewerContext>, transport: &str, old_serial: &str, how: &str) {
    if let Ok(mut serial) = context.serial.lock() {
        *serial = transport.to_owned();
    }
    if let Ok(mut link) = context.link.lock() {
        link.down_since = None;
        link.last_error = None;
    }
    resume_after_reconnect(context, old_serial);
    publish_transport(context);
    if let Ok(mut link) = context.link.lock() {
        link.attempting = false;
    }
    context.log.event(
        "link",
        "reconnected",
        &format!(
            "\"transport\":\"{}\",\"how\":\"{how}\"",
            crate::diagnostics::json_escape(transport)
        ),
    );
}

/// The phone left one link and is already on the other - or back on the same
/// one: move the session there without making anyone click. See
/// AUTO_SWITCH_WINDOW for why this is safe where an automatic retry is not.
///
/// Back on the SAME link: after a short Wi-Fi drop a mirrored phone comes back
/// into `adb devices` under its same mDNS name some seconds later, and the
/// window would otherwise sit on the panel until its minute ran out and close.
/// So: resume by itself if the phone is back within that minute. Same rule as
/// the switch - only a listing
/// is read, nothing is dialled - so it is allowed for the whole minute the
/// window stays open, not just AUTO_SWITCH_WINDOW.
///
/// Only a `Certain` match counts - a transport whose name carries this phone's
/// serial. An `ip:port` would have to be asked, and asking is traffic to a
/// phone on a timer, which is exactly what this must never become.
fn try_auto_switch(context: &Arc<ViewerContext>) {
    let down_for = match context.link.lock() {
        Ok(link) if !link.attempting => link.down_since.map(|since| since.elapsed()),
        _ => None,
    };
    let Some(down_for) = down_for else {
        return;
    };
    let old_serial = context.serial();
    let old_is_usb = is_usb_transport(&old_serial);
    let transports = list_transports(&context.adb);
    // The other kind of link first, as before: if the cable went in, that is
    // where the user wants the window.
    let other = if down_for <= AUTO_SWITCH_WINDOW {
        match match_transport(&transports, &context.hardware_serial, !old_is_usb) {
            TransportPick::Certain(transport) => Some(transport),
            _ => None,
        }
    } else {
        None
    };
    let (transport, event) = match other {
        Some(transport) => (transport, "auto_switch"),
        None => match match_transport(&transports, &context.hardware_serial, old_is_usb) {
            TransportPick::Certain(transport) => (transport, "auto_resume"),
            _ => return,
        },
    };
    // Claim it under the lock: a click in the window between the check above
    // and here would otherwise start a second hand-over beside this one.
    match context.link.lock() {
        Ok(mut link) if !link.attempting && link.down_since.is_some() => {
            link.attempting = true;
        }
        _ => return,
    }
    context.log.event(
        "link",
        event,
        &format!(
            "\"from\":\"{}\",\"to\":\"{}\"",
            crate::diagnostics::json_escape(&old_serial),
            crate::diagnostics::json_escape(&transport)
        ),
    );
    land_on_transport(context, &transport, &old_serial, "auto");
}

fn adb_command(executable: &std::path::Path) -> Command {
    let mut command = Command::new(executable);
    crate::hide_process_window(&mut command);
    command
}

fn remote_agent_path(token: &str) -> String {
    format!("{REMOTE_AGENT_DIRECTORY}/{REMOTE_AGENT_PREFIX}{token}.jar")
}

/// Where an agent's stderr lands on the phone: next to the jar, one file per
/// agent class, so `cleanup_stream_agent` can remove them all with one pattern.
fn agent_stderr_path(remote_agent_path: &str, agent_class: &str) -> String {
    format!("{remote_agent_path}.{agent_class}.err")
}

/// What an agent that died wrote to stderr, as a suffix for the error shown
/// to the person, or nothing if there was nothing. The whole text goes to the
/// session log; the person gets the tail. The file is removed afterwards.
fn agent_stderr_tail(context: &Arc<ViewerContext>, component: &'static str, path: &str) -> String {
    const SHOWN_CHARS: usize = 500;
    const LOGGED_CHARS: usize = 4000;
    let output = adb_command(&context.adb)
        .args(["-s", &context.serial(), "shell", "cat", path])
        .output();
    let _ = adb_command(&context.adb)
        .args(["-s", &context.serial(), "shell", "rm", "-f", path])
        .output();
    let Ok(output) = output else {
        return String::new();
    };
    let text = collapse_whitespace(&String::from_utf8_lossy(&output.stdout));
    if text.is_empty() {
        return String::new();
    }
    context
        .log
        .message(component, "agent_stderr", &tail_chars(&text, LOGGED_CHARS));
    format!(". The agent said: {}", tail_chars(&text, SHOWN_CHARS))
}

/// Runs of whitespace, newlines included, folded to one space.
fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The last `count` characters of `text`, on a character boundary.
fn tail_chars(text: &str, count: usize) -> String {
    let total = text.chars().count();
    if total <= count {
        return text.to_owned();
    }
    text.chars().skip(total - count).collect()
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// ---------- Game-mode hint ----------
// Whether to show the strip explaining game mode at the bottom of the window.
// "Close" (in the page) hides it for THIS session only, no file involved -
// look for gameHintClosed in the viewer script. "Don't show again" is the one
// that reaches here: it has to survive into windows this session has not
// opened yet, which means a file, the same way every other cross-session
// preference in this codebase works.

/// Whether the marker file exists. Missing, unreadable, or no path handed
/// over at all all read the same way: show the hint. Corrupt is not a state
/// this file can be in - nothing is ever written to it but its own existence.
fn game_hint_dismissed(context: &Arc<ViewerContext>) -> bool {
    context
        .game_hint_file
        .as_ref()
        .is_some_and(|path| path.exists())
}

/// Creates the marker file. Best-effort: a failed write here costs the user
/// one more strip next time, not a broken window, so nothing here is allowed
/// to turn into a request error.
fn dismiss_game_hint(context: &Arc<ViewerContext>) {
    let Some(path) = context.game_hint_file.as_ref() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, b"1");
}

// ---------- Keyboard shortcuts ----------
// The user's shortcuts, saved by the APP (keybindings.json) and pointed at by
// MIRRYL_KEYBINDINGS_FILE. Read once, when this window is served: changing a
// shortcut applies to mirror windows opened afterwards, and the settings screen
// says as much rather than pretending otherwise.

/// What every action is bound to when the app has never saved anything - a
/// fresh install, or the engine run on its own from a terminal.
///
/// TWIN OF `SHORTCUT_ACTIONS` in the app's main.js. Change one, change the
/// other; there is no build step that would catch them drifting apart.
const DEFAULT_KEYBINDINGS: &[(&str, &str)] = &[
    ("panel_power", "Ctrl+S"),
    // Picked in testing, tried from Settings and then made the
    // shipped defaults - middle-click also reaches Home directly (see
    // HOME_KEY in the viewer), so F1 is a second way to get there.
    ("home", "F1"),
    ("recent_apps", "F2"),
    // Ctrl+R by default, rebindable like everything else here.
    ("rotate_view", "Ctrl+R"),
    // Ctrl+F: free on the phone side (only A, C, V, X and Z go through).
    ("fill_screen", "Ctrl+F"),
    ("game_mode", "F12"),
    ("swipe_up", "W"),
    ("swipe_down", "S"),
    ("swipe_left", "A"),
    ("swipe_right", "D"),
    // Unbound by default, deliberately: their obvious keys are W/A/S/D, which
    // the swipes above already claim, and one key cannot mean both at once.
    // Whoever wants held-key panning instead of flicks re-points W/A/S/D at
    // these from Settings - moving them off swipe_up/etc first, which is what
    // frees the keys up to bind here without hitting the clash check.
    ("drag_up", ""),
    ("drag_down", ""),
    ("drag_left", ""),
    ("drag_right", ""),
];

/// The shortcuts as a JSON object, ready to be pasted into the viewer's script.
fn keybindings_json() -> String {
    let mut bindings: Vec<(String, String)> = DEFAULT_KEYBINDINGS
        .iter()
        .map(|(action, key)| ((*action).to_owned(), (*key).to_owned()))
        .collect();
    if let Some(path) = std::env::var_os("MIRRYL_KEYBINDINGS_FILE")
        && let Ok(text) = std::fs::read_to_string(PathBuf::from(path))
    {
        for (action, key) in parse_keybindings(&text) {
            match bindings.iter_mut().find(|(known, _)| *known == action) {
                Some(entry) => entry.1 = key,
                // An action this build has never heard of - a newer app, an
                // older engine. Carried through rather than dropped: it costs
                // nothing here and the viewer simply never looks it up.
                None => bindings.push((action, key)),
            }
        }
    }
    let body = bindings
        .iter()
        .map(|(action, key)| format!("\"{action}\":\"{key}\""))
        .collect::<Vec<_>>()
        .join(",");
    format!("{{{body}}}")
}

/// Pulls "action":"shortcut" pairs out of a flat JSON object.
///
/// Hand-written because the engine has no JSON library and this does not
/// justify one: the file is a single flat object of text to text, written by
/// Mirryl itself.
///
/// Every pair is CHECKED before it is accepted, and that is not paranoia about
/// the app - it is that whatever comes back gets pasted straight into a
/// `<script>` in the viewer. Refusing `<`, `>`, quotes and backslashes means
/// nothing read here can close that script tag or break out of the string,
/// whatever ends up in the file. Anything that fails the check is skipped and
/// the default for that action stands.
fn parse_keybindings(text: &str) -> Vec<(String, String)> {
    const MAX_BINDING_CHARS: usize = 40;
    let mut pairs = Vec::new();
    // Strings cannot contain an escape (backslashes are refused below), so
    // "text between quotes" is the whole of the parsing needed here.
    let mut strings = text.split('"').skip(1).step_by(2);
    while let (Some(action), Some(key)) = (strings.next(), strings.next()) {
        let action_ok = !action.is_empty()
            && action.len() <= MAX_BINDING_CHARS
            && action
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_');
        let key_ok = !key.is_empty()
            && key.chars().count() <= MAX_BINDING_CHARS
            && key
                .chars()
                .all(|character| !character.is_control() && !"<>\"\\".contains(character));
        if action_ok && key_ok {
            pairs.push((action.to_owned(), key.to_owned()));
        }
    }
    pairs
}

const VIEWER_HTML: &str = include_str!("../../../assets/viewer-v070.html");
const MIRRYL_LOGO: &[u8] = include_bytes!("../../../assets/Mirryl.png");
const MIRRYL_STREAM_AGENT: &[u8] = include_bytes!("../../../assets/mirryl-stream-agent.jar");

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{
        CONTROL_KIND_CLIPBOARD, CONTROL_KIND_KEY, MAX_CLIPBOARD_BYTES, MAX_WEBSOCKET_PAYLOAD,
        TransportPick, is_allowed_key, is_ctrl_shortcut_key, base64_encode, is_usb_transport, match_transport,
        origin_allowed, parse_keybindings, parse_mdns_address, parse_transports,
        quality_capture_size, remote_agent_path, session_token, sha1, stale_agent_files,
        stale_forwards, websocket_accept, parse_adb_device_line, split_last_two_fields,
    };

    /// The serial of the phone every one of these cases is about.
    const PHONE: &str = "R5CX12ABCDE";
    /// Its wireless name, copied from a real `adb devices`.
    const PHONE_WIFI: &str = "adb-R5CX12ABCDE-62Sel0._adb-tls-connect._tcp";

    fn names(listing: &str) -> Vec<String> {
        parse_transports(listing)
    }

    #[test]
    fn reads_the_shortcuts_the_app_saves() {
        assert_eq!(
            parse_keybindings("{\"panel_power\":\"Ctrl+Shift+O\"}"),
            vec![("panel_power".to_owned(), "Ctrl+Shift+O".to_owned())]
        );
    }

    /// Whatever this returns is pasted into a `<script>` in the viewer, so a
    /// shortcut carrying a closing tag has to be dropped rather than escaped:
    /// there is no escaping that survives being read as HTML first.
    #[test]
    fn refuses_shortcuts_that_could_close_the_script_tag() {
        assert!(
            parse_keybindings("{\"panel_power\":\"</script><script>alert(1)\"}").is_empty()
        );
        assert!(parse_keybindings("{\"panel_power\":\"Ctrl+\\\\\"}").is_empty());
        assert!(parse_keybindings("{\"bad name\":\"Ctrl+O\"}").is_empty());
    }

    /// A half-written or truncated file must not take the defaults down with
    /// it: whatever survives the check is used, the rest simply is not there.
    #[test]
    fn skips_broken_entries_and_keeps_the_rest() {
        assert_eq!(
            parse_keybindings("{\"a b\":\"Ctrl+A\",\"panel_power\":\"F9\",\"trailing\""),
            vec![("panel_power".to_owned(), "F9".to_owned())]
        );
        assert!(parse_keybindings("").is_empty());
        assert!(parse_keybindings("{\"panel_power\":\"\"}").is_empty());
    }

    #[test]
    fn only_usable_transports_are_listed() {
        let listing = "List of devices attached\n\
R5CX12ABCDE\tdevice\n\
192.168.0.9:5555\toffline\n\
ZY22000TST\tunauthorized\n\
\n";
        assert_eq!(names(listing), vec!["R5CX12ABCDE".to_owned()]);
    }

    /// `adb devices -l` puts a `device:m2q` field at the end of the line. Split
    /// on whitespace and take the SECOND token, or that field reads as a state.
    #[test]
    fn long_listing_state_is_read_from_the_right_column() {
        let listing = "List of devices attached\n\
adb-R5CX12ABCDE-62Sel0._adb-tls-connect._tcp device product:m2quew model:SM_S947U1 device:m2q\n";
        assert_eq!(names(listing), vec![PHONE_WIFI.to_owned()]);
    }

    #[test]
    fn usb_and_wireless_transports_are_told_apart() {
        assert!(is_usb_transport("R5CX12ABCDE"));
        assert!(!is_usb_transport("192.168.0.4:44673"));
        assert!(!is_usb_transport(PHONE_WIFI));
    }

    /// The cable is pulled: the phone must be found on Wi-Fi, and NOT on USB.
    #[test]
    fn wireless_name_identifies_the_same_phone() {
        let transports = names(&format!("List of devices attached\n{PHONE_WIFI}\tdevice\n"));
        assert_eq!(
            match_transport(&transports, PHONE, false),
            TransportPick::Certain(PHONE_WIFI.to_owned())
        );
        assert_eq!(match_transport(&transports, PHONE, true), TransportPick::None);
    }

    /// Both links up at once - each kind must pick its own, never the other.
    #[test]
    fn each_kind_picks_its_own_transport() {
        let transports = names(&format!(
            "List of devices attached\n{PHONE}\tdevice\n{PHONE_WIFI}\tdevice\n"
        ));
        assert_eq!(
            match_transport(&transports, PHONE, true),
            TransportPick::Certain(PHONE.to_owned())
        );
        assert_eq!(
            match_transport(&transports, PHONE, false),
            TransportPick::Certain(PHONE_WIFI.to_owned())
        );
    }

    /// A bare ip:port names no phone, so it can only be a candidate to ask.
    #[test]
    fn plain_address_has_to_be_asked() {
        let transports = names("List of devices attached\n192.168.0.4:44673\tdevice\n");
        assert_eq!(
            match_transport(&transports, PHONE, false),
            TransportPick::Ask(vec!["192.168.0.4:44673".to_owned()])
        );
    }

    /// With two phones answering on plain addresses, EVERY candidate has to be
    /// offered. Keeping only the first meant that if a stranger's phone came
    /// first in the listing, ours was never even asked about.
    #[test]
    fn every_unnamed_address_is_offered_in_order() {
        let transports = names(
            "List of devices attached\n\
192.168.0.9:5555\tdevice\n\
192.168.0.4:44673\tdevice\n",
        );
        assert_eq!(
            match_transport(&transports, PHONE, false),
            TransportPick::Ask(vec![
                "192.168.0.9:5555".to_owned(),
                "192.168.0.4:44673".to_owned(),
            ])
        );
    }

    /// Someone else's phone must never come back as a certainty. It may still
    /// be worth asking - a USB transport name is usually the hardware serial
    /// but is not promised to be - and the answer is what rejects it.
    #[test]
    fn another_phone_is_never_a_certainty() {
        let transports = names(
            "List of devices attached\n\
ZY22000TST\tdevice\n\
adb-ZY22000TST-9xQq11._adb-tls-connect._tcp\tdevice\n",
        );
        for want_usb in [true, false] {
            assert!(!matches!(
                match_transport(&transports, PHONE, want_usb),
                TransportPick::Certain(_)
            ));
        }
    }

    /// A name that merely CONTAINS our serial is not ours. The stranger above
    /// (`ZY22000TST`) does not overlap, so it never exercised this; a name whose
    /// serial has ours inside it does. `adb devices` prints wireless transports
    /// in exactly this shape and the advertiser picks the name, so this is a
    /// name anybody on the Wi-Fi can put up. It must not come back `Certain`:
    /// `find_transport` never questions a `Certain`, so the reconnect loop would
    /// have mirrored the stranger's screen without asking anything.
    #[test]
    fn a_name_that_only_contains_our_serial_is_not_a_certainty() {
        let transports = names(
            "List of devices attached\n\
adb-XR5CX12ABCDEX-9xQq11._adb-tls-connect._tcp\tdevice\n",
        );
        assert_eq!(
            match_transport(&transports, PHONE, false),
            TransportPick::Ask(vec![
                "adb-XR5CX12ABCDEX-9xQq11._adb-tls-connect._tcp".to_owned()
            ])
        );
        // And it must not shadow the real one when both are listed.
        let transports = names(&format!(
            "List of devices attached\n\
adb-XR5CX12ABCDEX-9xQq11._adb-tls-connect._tcp\tdevice\n\
{PHONE_WIFI}\tdevice\n"
        ));
        assert_eq!(
            match_transport(&transports, PHONE, false),
            TransportPick::Certain(PHONE_WIFI.to_owned())
        );
    }

    /// Ours among strangers, on both kinds of link.
    #[test]
    fn the_right_phone_is_picked_out_of_several() {
        let transports = names(&format!(
            "List of devices attached\n\
ZY22000TST\tdevice\n\
{PHONE}\tdevice\n\
adb-ZY22000TST-9xQq11._adb-tls-connect._tcp\tdevice\n\
{PHONE_WIFI}\tdevice\n"
        ));
        assert_eq!(
            match_transport(&transports, PHONE, true),
            TransportPick::Certain(PHONE.to_owned())
        );
        assert_eq!(
            match_transport(&transports, PHONE, false),
            TransportPick::Certain(PHONE_WIFI.to_owned())
        );
    }

    /// Real output of `adb mdns services`. This is the phone's CURRENT port;
    /// the app may have `:44673` on file from an earlier session, which is why
    /// dialling the saved address fails.
    #[test]
    fn discovery_gives_the_current_wireless_address() {
        let listing = "List of discovered mdns services\n\
adb-R5CX12ABCDE-62Sel0\t_adb-tls-connect._tcp\t192.168.0.4:44065\n";
        assert_eq!(
            parse_mdns_address(listing, PHONE),
            Some("192.168.0.4:44065".to_owned())
        );
    }

    /// The pairing service advertises the same phone on a different port that
    /// refuses ordinary connections. Matching on the serial alone would dial it.
    #[test]
    fn the_pairing_service_is_not_a_connect_address() {
        let listing = "List of discovered mdns services\n\
adb-R5CX12ABCDE-62Sel0\t_adb-tls-pairing._tcp\t192.168.0.4:37021\n";
        assert_eq!(parse_mdns_address(listing, PHONE), None);
    }

    /// Someone else's phone advertising on the same network is not an address
    /// for ours.
    #[test]
    fn discovery_ignores_other_phones() {
        let listing = "List of discovered mdns services\n\
adb-ZY22000TST-9xQq11\t_adb-tls-connect._tcp\t192.168.0.9:41111\n";
        assert_eq!(parse_mdns_address(listing, PHONE), None);
    }

    /// The advertised name is chosen by whoever advertises it. One whose serial
    /// merely CONTAINS ours is not ours: the match is on the whole serial, not
    /// on a substring of the line.
    #[test]
    fn discovery_rejects_a_serial_that_only_contains_ours() {
        let listing = "List of discovered mdns services\n\
adb-XR5CX12ABCDEX-abc123\t_adb-tls-connect._tcp\t192.168.0.9:41111\n";
        assert_eq!(parse_mdns_address(listing, PHONE), None);
        // And the serial is not found in the wrong field either: a line whose
        // address field carries our serial as a hostname is still not ours.
        let listing = "List of discovered mdns services\n\
adb-ZY22000TST-9xQq11\t_adb-tls-connect._tcp\tR5CX12ABCDE:41111\n";
        assert_eq!(parse_mdns_address(listing, PHONE), None);
    }

    /// Ours among others, and with the pairing entry in the way.
    #[test]
    fn discovery_picks_our_connect_entry_out_of_several() {
        let listing = "List of discovered mdns services\n\
adb-ZY22000TST-9xQq11\t_adb-tls-connect._tcp\t192.168.0.9:41111\n\
adb-R5CX12ABCDE-62Sel0\t_adb-tls-pairing._tcp\t192.168.0.4:37021\n\
adb-R5CX12ABCDE-62Sel0\t_adb-tls-connect._tcp\t192.168.0.4:44065\n";
        assert_eq!(
            parse_mdns_address(listing, PHONE),
            Some("192.168.0.4:44065".to_owned())
        );
    }

    /// Nothing discovered: fall back to the saved address rather than crash.
    #[test]
    fn empty_discovery_gives_nothing() {
        assert_eq!(
            parse_mdns_address("List of discovered mdns services\n", PHONE),
            None
        );
    }

    /// Nothing attached at all: the honest answer is "not here", not a panic.
    #[test]
    fn empty_listing_finds_nothing() {
        let transports = names("List of devices attached\n\n");
        assert!(transports.is_empty());
        assert_eq!(match_transport(&transports, PHONE, true), TransportPick::None);
    }

    #[test]
    fn quality_profile_scales_large_portrait_screen_to_1080p_class() {
        assert_eq!(quality_capture_size(1440, 3120), (1080, 2340));
    }

    #[test]
    fn quality_profile_preserves_smaller_even_screen() {
        assert_eq!(quality_capture_size(720, 1280), (720, 1280));
    }

    #[test]
    fn websocket_accept_matches_rfc_example() {
        assert_eq!(
            websocket_accept("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn cryptographic_helpers_cover_padding_variants() {
        assert_eq!(base64_encode(&sha1(b"abc")), "qZk+NkcGgWq6PiVxeFDCbJzQ2J0=");
        assert_eq!(CONTROL_KIND_KEY, 2);
    }

    #[test]
    fn clipboard_limits_and_ctrl_keys_match_the_agent() {
        // ClipboardBridge.MAX_BYTES and KIND_CLIPBOARD in the agent.
        assert_eq!(MAX_CLIPBOARD_BYTES, 65_536);
        assert_eq!(CONTROL_KIND_CLIPBOARD, 8);
        // A full-size copy plus the opcode and flag byte must fit in a frame.
        assert_eq!(MAX_WEBSOCKET_PAYLOAD, 65_538);
        // A, C, V, X, Z - and nothing else travels with Ctrl held.
        for code in [29, 31, 50, 52, 54] {
            assert!(is_ctrl_shortcut_key(code));
            assert!(!is_allowed_key(code));
        }
        for code in [3, 4, 26, 30, 32, 66, 224] {
            assert!(!is_ctrl_shortcut_key(code));
        }
    }

    #[test]
    fn every_token_produces_a_distinct_remote_agent_path() {
        let first = remote_agent_path("000000000000000018c5ab500000000000001111");
        let second = remote_agent_path("000000000000000018c5ab500000000000002222");
        assert_ne!(first, second);
        assert!(first.ends_with("00001111.jar"));
        assert!(second.ends_with("00002222.jar"));
    }

    /// The sweep takes only what no session can still be using: files of other
    /// tokens that no process carries. The process listing is shaped like
    /// toybox's `ps -A -w -o ARGS`, with the shell that runs the ps in it.
    #[test]
    fn sweep_keeps_own_and_live_tokens_and_takes_the_rest() {
        let own = "1111111111111111111111111111111111111111";
        let live = "2222222222222222222222222222222222222222";
        let dead = "3333333333333333333333333333333333333333";
        let listing = format!(
            "panelprobe.jar\nmirryl-connect-{own}.jar\nmirryl-connect-{live}.jar\n\
mirryl-connect-{live}.jar.VideoStreamMain.err\nmirryl-connect-{dead}.jar\n\
mirryl-connect-{dead}.jar.ControlStreamMain.err\nmirryl-connect-.jar\n\
mirryl-connect-*.jar\n"
        );
        // The three shapes a live session really has in ps: the `sh -c` that
        // launches an agent (token in CLASSPATH and in the stderr redirect),
        // the control agent (token as its only argument) and the video agent
        // (token last, after the size and bitrate).
        let processes = format!(
            "ARGS\ninit\nsh -c ps -A -w -o ARGS\n\
sh -c CLASSPATH=/data/local/tmp/mirryl-connect-{live}.jar app_process / \
dev.mirador.agent.ControlStreamMain {live} \
2>/data/local/tmp/mirryl-connect-{live}.jar.ControlStreamMain.err\n\
app_process / dev.mirador.agent.ControlStreamMain {live}\n\
app_process / dev.mirador.agent.VideoStreamMain 1080 2340 8000000 {live}\n"
        );
        assert_eq!(
            stale_agent_files(&listing, &processes, own),
            vec![
                format!("/data/local/tmp/mirryl-connect-{dead}.jar"),
                format!("/data/local/tmp/mirryl-connect-{dead}.jar.ControlStreamMain.err"),
            ]
        );
    }

    /// 40 hex characters, so the places that embed it can rely on the shape -
    /// and no two sessions may ever share one.
    #[test]
    fn session_token_is_forty_hex_and_never_repeats() {
        let first = session_token().expect("token");
        let second = session_token().expect("token");
        assert_eq!(first.len(), 40);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(first, second);
    }

    fn headers_with_origin(origin: Option<&str>) -> HashMap<String, String> {
        let mut headers = HashMap::new();
        if let Some(origin) = origin {
            headers.insert("origin".to_owned(), origin.to_owned());
        }
        headers
    }

    #[test]
    fn origin_check_accepts_only_the_viewer_page_itself() {
        assert!(origin_allowed(&headers_with_origin(Some("http://127.0.0.1:4321")), 4321));
        assert!(origin_allowed(&headers_with_origin(Some("http://localhost:4321")), 4321));
    }

    /// The forwards of a dead session go; this session's, a live session's,
    /// another device's and anything that is not one of our sockets stay.
    #[test]
    fn stale_forwards_keeps_own_live_and_foreign_entries() {
        let list = "\
R5CX12ABCDE tcp:54290 localabstract:mirryl_control_deadbeef\n\
R5CX12ABCDE tcp:64016 localabstract:mirryl_video_deadbeef\n\
R5CX12ABCDE tcp:56751 localabstract:mirryl_control_own000\n\
R5CX12ABCDE tcp:50608 localabstract:mirryl_audio_alive11\n\
R5CX12ABCDE tcp:7777 localabstract:something_else\n\
R5CX12ABCDE tcp:8888 tcp:8888\n\
emulator-5554 tcp:9999 localabstract:mirryl_control_deadbeef\n";
        let processes = "ARGS\napp_process / dev.mirador.agent.AudioStreamMain alive11\n";
        let stale = stale_forwards(list, "R5CX12ABCDE", processes, "own000");
        assert_eq!(stale, vec!["tcp:54290".to_owned(), "tcp:64016".to_owned()]);
        assert!(stale_forwards("", "R5CX12ABCDE", processes, "own000").is_empty());
    }

    /// An mDNS rename puts a SPACE in the transport name. These are the
    /// listings adb prints for it.
    const PHONE_WIFI_RENAMED: &str = "adb-R5CX12ABCDE-62Sel0 (2)._adb-tls-connect._tcp";

    #[test]
    fn a_renamed_transport_is_listed_under_its_whole_name() {
        let plain = "List of devices attached\n\
adb-R5CX12ABCDE-62Sel0 (2)._adb-tls-connect._tcp\tdevice\n\
emulator-5554\tdevice\n";
        let long = "List of devices attached\n\
adb-R5CX12ABCDE-62Sel0 (2)._adb-tls-connect._tcp device product:m2quew model:SM_S947U1 device:m2q transport_id:1\n\
emulator-5554          device product:sdk_gphone64_x86_64 model:sdk_gphone64_x86_64 device:emu64xa transport_id:4\n";
        let expected = vec![PHONE_WIFI_RENAMED.to_owned(), "emulator-5554".to_owned()];
        assert_eq!(names(plain), expected);
        assert_eq!(names(long), expected);
        assert!(!is_usb_transport(PHONE_WIFI_RENAMED));
        // A line adb prints before the header is not a transport either.
        assert!(names("* daemon started successfully\nList of devices attached\n").is_empty());
        assert_eq!(parse_adb_device_line("SER offline device:m2q"), Some(("SER", "offline")));
    }

    /// The renamed instance is the phone's CURRENT address; reading by
    /// whitespace skipped it and returned the stale one below it.
    #[test]
    fn discovery_reads_a_renamed_instance() {
        let listing = "List of discovered mdns services\n\
adb-R5CX12ABCDE-62Sel0 (2)\t_adb-tls-connect._tcp\t192.168.0.4:36613\n\
adb-R5CX12ABCDE-62Sel0\t_adb-tls-connect._tcp\t192.168.0.4:34419\n";
        assert_eq!(parse_mdns_address(listing, PHONE), Some("192.168.0.4:36613".to_owned()));
    }

    #[test]
    fn stale_forwards_of_a_renamed_transport() {
        let list = "\
adb-R5CX12ABCDE-62Sel0 (2)._adb-tls-connect._tcp tcp:54290 localabstract:mirryl_control_deadbeef\n\
adb-R5CX12ABCDE-62Sel0 (2)._adb-tls-connect._tcp tcp:56751 localabstract:mirryl_control_own000\n";
        let stale = stale_forwards(list, PHONE_WIFI_RENAMED, "", "own000");
        assert_eq!(stale, vec!["tcp:54290".to_owned()]);
        assert_eq!(split_last_two_fields("only two"), None);
    }

    /// A missing header is a hand-written client, not a browser; the rest are
    /// what another page, another server, or a sandboxed frame would send.
    #[test]
    fn origin_check_rejects_everything_else() {
        assert!(!origin_allowed(&headers_with_origin(None), 4321));
        assert!(!origin_allowed(&headers_with_origin(Some("http://127.0.0.1:4322")), 4321));
        assert!(!origin_allowed(&headers_with_origin(Some("http://192.168.0.4:4321")), 4321));
        assert!(!origin_allowed(&headers_with_origin(Some("https://127.0.0.1:4321")), 4321));
        assert!(!origin_allowed(&headers_with_origin(Some("null")), 4321));
        assert!(!origin_allowed(&headers_with_origin(Some("")), 4321));
    }
}

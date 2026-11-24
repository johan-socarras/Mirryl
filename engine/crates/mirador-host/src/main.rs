#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::env;
use std::process::{Command, ExitCode};

use mirador_core::adb::AdbClient;

mod diagnostics;
mod native_window;
mod viewer;

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("Error: {error}");
            #[cfg(windows)]
            if env::var_os("MIRADOR_NO_DIALOG").is_none() {
                native_window::show_error(&error);
            }
            ExitCode::FAILURE
        }
    }
}

/// Shown to anyone who runs the engine by hand.
///
/// Not an error the way a crash is - it is a person who found an .exe in a
/// folder and double-clicked it - so it says what to do instead.
const NOT_A_STANDALONE_APP: &str = "Mirryl Connect is the mirroring engine that Mirryl uses.

It is not meant to be opened on its own. Open Mirryl and click your device there.";

/// The engine does exactly one thing: mirror the device it is given. Pairing,
/// connecting and everything else belong to the Mirryl app.
fn run() -> Result<u8, String> {
    let arguments: Vec<String> = env::args().skip(1).collect();
    if arguments.len() != 2 || arguments[0] != "mirror" {
        return Err(NOT_A_STANDALONE_APP.to_owned());
    }
    #[cfg(target_os = "linux")]
    end_session_on_signals();
    let adb = AdbClient::discover()?;
    viewer::run(&adb, &arguments[1])?;
    Ok(0)
}

fn hide_process_window(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

/// Ties a long-lived `adb` child to this process: when the engine goes away -
/// however it goes away - Windows kills the child with it.
///
/// Why: an engine killed outright would leave its `adb exec-out ... app_process`
/// and `adb logcat` children running for hours. Between them they keep the
/// control forward's TCP socket open, so the phone-side agent never sees EOF
/// and never idles out (see ControlStreamMain's HOST_SILENCE_EXIT_MS for the
/// phone-side half). Two orphan `adb.exe` per crashed session is a leak on its
/// own.
///
/// One job object for the whole process, created on first use and never
/// closed: the kernel closes it when the engine dies, and
/// KILL_ON_JOB_CLOSE does the rest. SILENT_BREAKAWAY_OK keeps the job to the
/// children themselves: should one of them have to start the adb server
/// (it is normally up already), the server is a grandchild and escapes, so a
/// crashed engine can never take the adb server - and every other app's
/// connections - down with it. Failure to attach is logged by the caller and
/// otherwise ignored: the children then simply outlive a crashed engine.
#[cfg(windows)]
pub fn attach_to_engine_job(child: &std::process::Child) -> Result<(), String> {
    use std::os::windows::io::AsRawHandle;
    use std::sync::OnceLock;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
    };

    static JOB: OnceLock<Result<usize, String>> = OnceLock::new();
    let job = JOB.get_or_init(|| unsafe {
        let handle = CreateJobObjectW(None, windows::core::PCWSTR::null()).map_err(|e| e.to_string())?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags =
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK;
        SetInformationJobObject(
            handle,
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
        .map_err(|e| e.to_string())?;
        Ok(handle.0 as usize)
    });
    let job = job.clone()?;
    unsafe {
        AssignProcessToJobObject(HANDLE(job as *mut core::ffi::c_void), HANDLE(child.as_raw_handle()))
            .map_err(|e| e.to_string())
    }
}

#[cfg(not(windows))]
pub fn attach_to_engine_job(_child: &std::process::Child) -> Result<(), String> {
    Ok(())
}

/// Linux has no job object, so an engine that is simply terminated would leave
/// behind what only its own teardown removes: its `adb` children, the port
/// forwards and the agents on the phone. SIGTERM, SIGINT and SIGHUP end the
/// session the way closing its window does, so the teardown runs. A second
/// signal ends the process at once, so Ctrl+C still works on an
/// engine that is not running its window loop. SIGKILL cannot be caught: that
/// one still leaves them, until the phone's agent times out on its own.
#[cfg(target_os = "linux")]
fn end_session_on_signals() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static SIGNALLED: AtomicBool = AtomicBool::new(false);

    extern "C" fn on_signal(_signal: libc::c_int) {
        // Only atomic stores and _exit: both are safe inside a signal handler.
        if SIGNALLED.swap(true, Ordering::SeqCst) {
            unsafe { libc::_exit(130) };
        }
        native_window::request_close();
    }

    let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        unsafe { libc::signal(signal, handler) };
    }
}

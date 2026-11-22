//! The bridge between AI agents and the running window manager.
//!
//! `gromak mcp` (see [`crate::mcp`]) is started by an agent's MCP client and
//! talks to the running gromak through a local named pipe,
//! `\\.\pipe\gromak-<session>`. Each connection carries one JSON request line
//! and gets one JSON reply line. Remote clients are rejected, and the pipe's
//! default security only lets the same user (and administrators) write to
//! it, so this is no more power than any program of the user already has.
//!
//! The pipe runs on its own thread. Requests that touch windows are handed
//! to the main thread (`WM_APP_AGENT`, handled by `Wm::on_agent`), so the
//! window manager never runs on two threads. A launch waits here, off the
//! main thread, for the new app's first window.

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::ptr::{null, null_mut};
use std::sync::mpsc::{channel, Sender};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{FlushFileBuffers, ReadFile, WriteFile, PIPE_ACCESS_DUPLEX};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

/// How long a request may wait for the main thread.
const MAIN_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a launch waits for the app's first window by default.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(15);

/// A request for the main thread, posted as the `lparam` of `WM_APP_AGENT`.
pub struct Call {
    pub request: Value,
    pub reply: Sender<Value>,
}

/// The pipe's name: one per Windows session, so other users' gromaks on
/// the same machine do not collide.
pub fn pipe_name() -> String {
    use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    let mut session = 0u32;
    unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) };
    format!(r"\\.\pipe\gromak-{session}")
}

pub fn error(message: impl Into<String>) -> Value {
    json!({ "ok": false, "error": message.into() })
}

/// Starts the pipe server thread.
pub fn start() {
    let spawned = std::thread::Builder::new().name("agent-pipe".into()).spawn(|| {
        let name: Vec<u16> = pipe_name().encode_utf16().chain(Some(0)).collect();
        loop {
            let pipe = unsafe {
                CreateNamedPipeW(
                    name.as_ptr(),
                    PIPE_ACCESS_DUPLEX,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    PIPE_UNLIMITED_INSTANCES,
                    64 * 1024,
                    64 * 1024,
                    0,
                    null(),
                )
            };
            if pipe == INVALID_HANDLE_VALUE {
                warn!("agent: could not create the pipe (error {})", unsafe { GetLastError() });
                return;
            }
            let connected =
                unsafe { ConnectNamedPipe(pipe, null_mut()) } != 0 || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
            if !connected {
                unsafe { CloseHandle(pipe) };
                continue;
            }
            // Serve each client on its own thread: a launch can take seconds.
            let pipe = pipe as usize;
            let _ = std::thread::Builder::new().name("agent-client".into()).spawn(move || serve(pipe as HANDLE));
        }
    });
    if let Err(e) = spawned {
        warn!("agent: could not start the pipe thread: {e}");
    }
}

/// A connected pipe as `Read`/`Write`.
struct Pipe(HANDLE);

impl Read for Pipe {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut read = 0u32;
        let ok = unsafe { ReadFile(self.0, buf.as_mut_ptr(), buf.len() as u32, &mut read, null_mut()) };
        if ok == 0 && read == 0 {
            // Broken pipe and friends end the stream.
            return Ok(0);
        }
        Ok(read as usize)
    }
}

impl Write for Pipe {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut written = 0u32;
        if unsafe { WriteFile(self.0, buf.as_ptr(), buf.len() as u32, &mut written, null_mut()) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(written as usize)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        unsafe { FlushFileBuffers(self.0) };
        Ok(())
    }
}

fn serve(handle: HANDLE) {
    let mut pipe = Pipe(handle);
    let reply = match read_line(&mut pipe).and_then(|line| serde_json::from_str::<Value>(&line).ok()) {
        Some(request) => handle_request(request),
        None => error("expected one JSON request line"),
    };
    let mut text = reply.to_string();
    text.push('\n');
    let _ = pipe.write_all(text.as_bytes());
    let _ = pipe.flush();
    // Closing (not disconnecting) lets the client read the reply to its end:
    // a disconnect would discard it with ERROR_PIPE_NOT_CONNECTED.
    unsafe { CloseHandle(handle) };
}

/// Reads bytes up to the first newline (requests are one line of JSON).
fn read_line(r: &mut impl Read) -> Option<String> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = r.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..n]);
        if bytes.contains(&b'\n') || bytes.len() > 1 << 20 {
            break;
        }
    }
    let line = bytes.split(|&b| b == b'\n').next()?.to_vec();
    String::from_utf8(line).ok().filter(|l| !l.trim().is_empty())
}

/// Runs a request on the main thread and waits for its reply.
fn on_main(request: Value) -> Value {
    let (tx, rx) = channel();
    let call = Box::into_raw(Box::new(Call { request, reply: tx })) as isize;
    if !crate::post_to_ctrl_checked(crate::WM_APP_AGENT, 0, call) {
        // Not posted: take the box back so it is freed.
        drop(unsafe { Box::from_raw(call as *mut Call) });
        return error("gromak is not ready");
    }
    rx.recv_timeout(MAIN_TIMEOUT).unwrap_or_else(|_| error("gromak did not answer in time"))
}

fn handle_request(request: Value) -> Value {
    if request["op"] != "launch" {
        return on_main(request);
    }
    // A launch: note the windows already there, start the app on the main
    // thread, then wait here for its windows to show up.
    let workspace = request["workspace"].clone();
    let ids = |reply: &Value| -> Vec<String> {
        reply["windows"]
            .as_array()
            .map_or_else(Vec::new, |w| w.iter().filter_map(|w| w["id"].as_str().map(String::from)).collect())
    };
    let before = on_main(json!({ "op": "windows", "workspace": workspace }));
    if before["ok"] != true {
        return before;
    }
    let before = ids(&before);
    let launched = on_main(request.clone());
    if launched["ok"] != true {
        return launched;
    }
    let timeout = request["timeout_ms"].as_u64().map_or(LAUNCH_TIMEOUT, Duration::from_millis);
    let deadline = Instant::now() + timeout;
    let mut new_windows: Vec<Value> = Vec::new();
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(250));
        let now = on_main(json!({ "op": "windows", "workspace": workspace }));
        new_windows = now["windows"]
            .as_array()
            .map(|w| {
                w.iter()
                    .filter(|w| w["id"].as_str().is_some_and(|id| !before.iter().any(|b| b == id)))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        if !new_windows.is_empty() {
            // Give the app a moment to settle its title and size.
            std::thread::sleep(Duration::from_millis(400));
            let settled = on_main(json!({ "op": "windows", "workspace": workspace }));
            new_windows = settled["windows"]
                .as_array()
                .map(|w| {
                    w.iter()
                        .filter(|w| w["id"].as_str().is_some_and(|id| !before.iter().any(|b| b == id)))
                        .cloned()
                        .collect()
                })
                .unwrap_or(new_windows);
            break;
        }
    }
    let mut reply = launched;
    reply["windows"] = Value::Array(new_windows.clone());
    if new_windows.is_empty() {
        reply["note"] = json!(
            "No window appeared on the workspace yet. The app may still be starting, or it opened its window \
             somewhere gromak does not manage; check gromak_windows again in a moment."
        );
    }
    reply
}

/// Sends one request to the running gromak (used by `gromak mcp`).
pub fn request(request: &Value) -> Result<Value, String> {
    let name = pipe_name();
    let mut pipe = open_pipe(&name)?;
    let mut line = request.to_string();
    line.push('\n');
    pipe.write_all(line.as_bytes()).map_err(|e| format!("writing to gromak: {e}"))?;
    // The reply is one line; stop there rather than waiting for the end.
    let text = read_line(&mut pipe).ok_or("gromak closed the connection without replying")?;
    serde_json::from_str(text.trim()).map_err(|e| format!("bad reply from gromak: {e}"))
}

fn open_pipe(name: &str) -> Result<std::fs::File, String> {
    // All pipe instances can be busy for a moment; retry briefly.
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match std::fs::OpenOptions::new().read(true).write(true).open(name) {
            Ok(file) => return Ok(file),
            Err(e) if e.raw_os_error() == Some(231) && Instant::now() < deadline => {
                // ERROR_PIPE_BUSY
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err("gromak is not running (start gromak first)".into())
            }
            Err(e) => return Err(format!("connecting to gromak: {e}")),
        }
    }
}

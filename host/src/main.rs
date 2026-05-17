//! Native-messaging host for browser-ext.
//!
//! The browser spawns one instance of this process per extension connection
//! and talks to it over stdio using the WebExtension native-messaging framing
//! (a little-endian u32 length prefix followed by a JSON body).
//!
//! On top of that this host also runs a Unix-socket server. CLI clients
//! connect to the socket, send a single JSON request, and get a single JSON
//! response. The host tags each request with a unique id, forwards it to the
//! extension over stdout, and routes the matching reply back to the waiting
//! client.
//!
//! Chrome and Firefox each spawn their own host instance, so the socket path
//! is namespaced by browser (detected from the launch arguments) to keep the
//! two from colliding.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Pending CLI requests keyed by request id, each holding the channel the
/// stdin reader uses to hand the extension's reply back to the client thread.
type Pending = Arc<Mutex<HashMap<u64, Sender<Value>>>>;

fn runtime_dir() -> PathBuf {
    if let Ok(dir) = env::var("XDG_RUNTIME_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    env::temp_dir()
}

/// Pick a socket path namespaced by the browser that launched us. Chrome
/// passes the extension origin (chrome-extension://...) as an argument;
/// Firefox passes the extension id and a manifest path instead.
fn socket_path() -> PathBuf {
    let browser = if env::args().any(|a| a.starts_with("chrome-extension://")) {
        "chrome"
    } else {
        "firefox"
    };
    runtime_dir().join(format!("browser-ext-{browser}.sock"))
}

/// Read one native-messaging frame from `r`.
fn read_frame(r: &mut impl Read) -> io::Result<Value> {
    let mut len_bytes = [0u8; 4];
    r.read_exact(&mut len_bytes)?;
    let len = u32::from_le_bytes(len_bytes) as usize;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    serde_json::from_slice(&buf)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Write one native-messaging frame to `w`.
fn write_frame(w: &mut impl Write, msg: &Value) -> io::Result<()> {
    let data = serde_json::to_vec(msg)?;
    w.write_all(&(data.len() as u32).to_le_bytes())?;
    w.write_all(&data)?;
    w.flush()
}

/// Serve one CLI client: read its request line, forward it to the extension,
/// wait for the matching reply, and write that reply back.
fn handle_client(
    mut stream: UnixStream,
    pending: Pending,
    next_id: Arc<AtomicU64>,
    to_ext: Sender<Value>,
) -> io::Result<()> {
    let mut line = String::new();
    BufReader::new(stream.try_clone()?).read_line(&mut line)?;
    let line = line.trim();
    if line.is_empty() {
        return Ok(());
    }

    let req: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            let _ = writeln!(stream, "{}", json!({ "error": format!("bad request: {e}") }));
            return Ok(());
        }
    };

    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(json!({}));

    let id = next_id.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::channel();
    pending.lock().unwrap().insert(id, tx);

    if to_ext
        .send(json!({ "id": id, "method": method, "params": params }))
        .is_err()
    {
        pending.lock().unwrap().remove(&id);
        let _ = writeln!(stream, "{}", json!({ "error": "extension not connected" }));
        return Ok(());
    }

    // Wait for the extension, with a bound so a wedged extension can't pin a
    // client forever.
    let reply = match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(v) => v,
        Err(_) => {
            pending.lock().unwrap().remove(&id);
            json!({ "error": "timed out waiting for extension" })
        }
    };

    writeln!(stream, "{reply}")?;
    Ok(())
}

fn main() {
    let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
    let next_id = Arc::new(AtomicU64::new(1));

    // Single writer thread owns stdout: both replies meant for the extension
    // and forwarded CLI requests funnel through this channel.
    let (to_ext, ext_rx) = mpsc::channel::<Value>();
    thread::spawn(move || {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        for msg in ext_rx {
            if write_frame(&mut out, &msg).is_err() {
                break;
            }
        }
    });

    // Unix-socket server for CLI clients.
    let sock = socket_path();
    let _ = fs::remove_file(&sock);
    match UnixListener::bind(&sock) {
        Ok(listener) => {
            eprintln!("listening on {}", sock.display());
            let pending = Arc::clone(&pending);
            let next_id = Arc::clone(&next_id);
            let to_ext = to_ext.clone();
            thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let pending = Arc::clone(&pending);
                    let next_id = Arc::clone(&next_id);
                    let to_ext = to_ext.clone();
                    thread::spawn(move || {
                        if let Err(e) = handle_client(stream, pending, next_id, to_ext) {
                            eprintln!("client error: {e}");
                        }
                    });
                }
            });
        }
        Err(e) => {
            // Another browser's host already owns the socket; this instance
            // can still serve its extension, just not CLI clients.
            eprintln!("could not bind {}: {e}", sock.display());
        }
    }

    // Main thread reads native-messaging frames from the extension and routes
    // each reply to the CLI client waiting on its id.
    let stdin = io::stdin();
    let mut handle = stdin.lock();
    loop {
        let msg = match read_frame(&mut handle) {
            Ok(v) => v,
            Err(_) => break, // browser closed the port
        };
        if let Some(id) = msg.get("id").and_then(Value::as_u64) {
            if let Some(tx) = pending.lock().unwrap().remove(&id) {
                let _ = tx.send(msg);
            }
        }
    }

    let _ = fs::remove_file(&sock);
}

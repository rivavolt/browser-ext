//! `browser` — query and control browser tabs from the command line.
//!
//! The command shape is `browser <group> <verb> [args]`, e.g.
//! `browser tabs list`, `browser tabs content <id>`, `browser windows list`.
//!
//! Each invocation connects to a browser-ext native-messaging host over a
//! Unix socket, sends one JSON request, and prints the JSON reply. Output is
//! JSON by default; `--plain` switches to a line-oriented form for shells.

use serde_json::{json, Value};
use std::env;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::exit;

const USAGE: &str = "\
browser — query and control browser tabs

usage:
  browser [--plain] [--browser chrome|firefox] <group> <verb> [args]

groups & verbs:
  tabs list                 list all tabs
  tabs content <id>         readable text of a tab
  tabs open [url]           open a new tab, optionally at a url
  tabs navigate <id> <url>  navigate a tab to a url
  tabs activate <id>        focus a tab and its window
  tabs eval <id> <js>       run JS in a tab, print the result as JSON
  tabs close <id>...        close one or more tabs by id
  windows list              list all windows

options:
  --plain                   line-oriented output instead of JSON
  --browser <name>          target browser (default: chrome)
  -h, --help                show this help";

fn runtime_dir() -> PathBuf {
    match env::var("XDG_RUNTIME_DIR") {
        Ok(d) if !d.is_empty() => PathBuf::from(d),
        _ => env::temp_dir(),
    }
}

fn socket_path(browser: &str) -> PathBuf {
    runtime_dir().join(format!("browser-ext-{browser}.sock"))
}

fn die(msg: impl AsRef<str>) -> ! {
    eprintln!("browser: {}", msg.as_ref());
    exit(1);
}

/// Send one request to the native host and return the parsed reply.
fn request(browser: &str, method: &str, params: Value) -> Value {
    let sock = socket_path(browser);
    let mut stream = UnixStream::connect(&sock).unwrap_or_else(|e| {
        die(format!(
            "cannot reach {browser} host at {} ({e}); is the browser running with the extension installed?",
            sock.display()
        ))
    });

    let req = json!({ "method": method, "params": params });
    let mut line = req.to_string();
    line.push('\n');
    stream
        .write_all(line.as_bytes())
        .unwrap_or_else(|e| die(format!("write failed: {e}")));

    let mut resp = String::new();
    BufReader::new(stream)
        .read_line(&mut resp)
        .unwrap_or_else(|e| die(format!("read failed: {e}")));

    let reply: Value = serde_json::from_str(resp.trim())
        .unwrap_or_else(|e| die(format!("bad reply from host: {e}")));

    if let Some(err) = reply.get("error").and_then(Value::as_str) {
        die(err);
    }
    reply.get("result").cloned().unwrap_or(Value::Null)
}

fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap());
}

fn main() {
    let mut plain = false;
    let mut browser = "chrome".to_string();
    let mut rest: Vec<String> = Vec::new();

    // Parse flags; everything after them is the group/verb/args.
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--plain" => plain = true,
            "--browser" => {
                browser = args
                    .next()
                    .unwrap_or_else(|| die("--browser needs a value"));
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            _ => {
                rest.push(arg);
                rest.extend(args.by_ref());
                break;
            }
        }
    }

    if rest.is_empty() {
        eprintln!("{USAGE}");
        exit(2);
    }

    let group = rest[0].as_str();
    let verb = rest.get(1).map(String::as_str).unwrap_or("");
    let args = &rest[2.min(rest.len())..];

    match (group, verb) {
        ("tabs", "list") => {
            let result = request(&browser, "tabs.list", json!({}));
            if plain {
                for t in result.as_array().map(Vec::as_slice).unwrap_or(&[]) {
                    println!(
                        "{}\t{}\t{}",
                        t["id"], t["title"].as_str().unwrap_or(""), t["url"].as_str().unwrap_or("")
                    );
                }
            } else {
                print_json(&result);
            }
        }

        ("tabs", "content") => {
            let id = args
                .first()
                .unwrap_or_else(|| die("tabs content needs a tab id"));
            let tab_id: i64 = id
                .parse()
                .unwrap_or_else(|_| die(format!("invalid tab id: {id}")));
            let result = request(&browser, "tabs.content", json!({ "id": tab_id }));
            if plain {
                println!("{}", result["text"].as_str().unwrap_or(""));
            } else {
                print_json(&result);
            }
        }

        ("tabs", "open") => {
            let url = args.first().map(String::as_str);
            let result = request(&browser, "tabs.open", json!({ "url": url }));
            if plain {
                println!("{}", result["id"]);
            } else {
                print_json(&result);
            }
        }

        ("tabs", "navigate") => {
            let id = args
                .first()
                .unwrap_or_else(|| die("tabs navigate needs a tab id and a url"));
            let tab_id: i64 = id
                .parse()
                .unwrap_or_else(|_| die(format!("invalid tab id: {id}")));
            let url = args
                .get(1)
                .unwrap_or_else(|| die("tabs navigate needs a url"));
            let result =
                request(&browser, "tabs.navigate", json!({ "id": tab_id, "url": url }));
            if plain {
                println!("{}", result["id"]);
            } else {
                print_json(&result);
            }
        }

        ("tabs", "activate") => {
            let id = args
                .first()
                .unwrap_or_else(|| die("tabs activate needs a tab id"));
            let tab_id: i64 = id
                .parse()
                .unwrap_or_else(|_| die(format!("invalid tab id: {id}")));
            let result = request(&browser, "tabs.activate", json!({ "id": tab_id }));
            if plain {
                println!("{}", result["id"]);
            } else {
                print_json(&result);
            }
        }

        ("tabs", "eval") => {
            let id = args
                .first()
                .unwrap_or_else(|| die("tabs eval needs a tab id and JS to run"));
            let tab_id: i64 = id
                .parse()
                .unwrap_or_else(|_| die(format!("invalid tab id: {id}")));
            let code = args
                .get(1)
                .unwrap_or_else(|| die("tabs eval needs JS to run"));
            let result =
                request(&browser, "tabs.eval", json!({ "id": tab_id, "code": code }));
            if plain {
                match &result["result"] {
                    Value::String(s) => println!("{s}"),
                    other => println!("{other}"),
                }
            } else {
                print_json(&result);
            }
        }

        ("tabs", "close") => {
            if args.is_empty() {
                die("tabs close needs at least one tab id");
            }
            let ids: Vec<i64> = args
                .iter()
                .map(|id| {
                    id.parse()
                        .unwrap_or_else(|_| die(format!("invalid tab id: {id}")))
                })
                .collect();
            let result = request(&browser, "tabs.close", json!({ "ids": ids }));
            if plain {
                for id in result["closed"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
                    println!("{id}");
                }
            } else {
                print_json(&result);
            }
        }

        ("windows", "list") => {
            let result = request(&browser, "windows.list", json!({}));
            if plain {
                for w in result.as_array().map(Vec::as_slice).unwrap_or(&[]) {
                    println!(
                        "{}\tfocused={}\ttabs={}",
                        w["id"], w["focused"], w["tabCount"]
                    );
                }
            } else {
                print_json(&result);
            }
        }

        ("tabs", "") | ("windows", "") => die(format!("'{group}' needs a verb; see --help")),
        (g, v) => die(format!("unknown command: {g} {v}; see --help")),
    }
}

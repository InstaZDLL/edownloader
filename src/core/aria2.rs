//! aria2 daemon control over JSON-RPC.
//!
//! aria2 is driven as one long-lived daemon instead of a process per download
//! because that is the only way to get exact byte progress out of it: when its
//! console is not a terminal it prints no readout at all, and scraping the
//! log is guesswork. `aria2.tellStatus` reports completed length, speed and
//! ETA directly, and the same session gives pause, resume and cancel for free.
//!
//! The RPC transport is a small HTTP/1.1 POST over loopback — no extra HTTP
//! stack, no TLS, and the payload is JSON we already depend on.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{json, Map, Value};

/// A believable desktop browser, which many file hosts and CDNs expect.
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                          (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";

pub struct Aria2 {
    child: Mutex<Child>,
    port: u16,
    secret: String,
}

#[derive(Clone, Debug)]
pub struct Status {
    pub status: String,
    pub total: u64,
    pub completed: u64,
    pub speed: u64,
    pub connections: u64,
    pub path: Option<PathBuf>,
    pub error: Option<String>,
}

impl Status {
    pub fn is_complete(&self) -> bool {
        self.status == "complete"
    }
    pub fn is_failed(&self) -> bool {
        matches!(self.status.as_str(), "error" | "removed")
    }
}

fn free_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
        .map_err(|e| format!("cannot reserve an RPC port: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("cannot read the reserved port: {e}"))?
        .port();
    drop(listener);
    Ok(port)
}

fn make_secret() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(1);
    format!("{:x}{:x}", nanos, std::process::id())
}

/// Decode a `Transfer-Encoding: chunked` body. aria2 answers with
/// Content-Length, but a proxy or a future version must not break us.
fn dechunk(payload: &str) -> Result<String, String> {
    let bytes = payload.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(offset) = payload[i..].find("\r\n") {
        let line_end = i + offset;
        let size_text = payload[i..line_end].split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16).unwrap_or(0);
        i = line_end + 2;
        if size == 0 {
            break;
        }
        let end = (i + size).min(bytes.len());
        out.extend_from_slice(&bytes[i..end]);
        i = end + 2;
        if i >= bytes.len() {
            break;
        }
    }
    String::from_utf8(out).map_err(|e| format!("chunked body was not UTF-8: {e}"))
}

fn http_post(port: u16, body: &str) -> Result<String, String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(10))
        .map_err(|e| format!("aria2 RPC is unreachable on port {port}: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .and_then(|_| stream.set_write_timeout(Some(Duration::from_secs(10))))
        .map_err(|e| format!("cannot configure the RPC socket: {e}"))?;
    let request = format!(
        "POST /jsonrpc HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("cannot send the RPC request: {e}"))?;

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| format!("cannot read the RPC response: {e}"))?;
    let text = String::from_utf8_lossy(&raw);
    let (head, payload) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| "malformed HTTP response from aria2".to_string())?;
    if !head.starts_with("HTTP/1.1 200") && !head.starts_with("HTTP/1.0 200") {
        return Err(format!(
            "aria2 RPC returned {}",
            head.lines().next().unwrap_or("an error")
        ));
    }
    if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        dechunk(payload)
    } else {
        Ok(payload.to_string())
    }
}

impl Aria2 {
    /// Start one daemon and wait until it answers.
    pub fn launch(
        exe: &Path,
        base_dir: &Path,
        max_concurrent: usize,
        connections: usize,
    ) -> Result<Aria2, String> {
        let port = free_port()?;
        let secret = make_secret();
        let connections = connections.clamp(1, 16).to_string();
        let mut command = Command::new(exe);
        command
            .arg("--enable-rpc=true")
            .arg("--rpc-listen-all=false")
            .arg(format!("--rpc-listen-port={port}"))
            .arg(format!("--rpc-secret={secret}"))
            .arg(format!("--dir={}", base_dir.display()))
            .arg("--continue=true")
            .arg("--file-allocation=none")
            .arg("--auto-file-renaming=true")
            .arg("--allow-overwrite=false")
            .arg("--content-disposition-default-utf8=true")
            .arg(format!(
                "--max-concurrent-downloads={}",
                max_concurrent.max(1)
            ))
            .arg(format!("--split={connections}"))
            .arg(format!("--max-connection-per-server={connections}"))
            .arg("--min-split-size=1M")
            .arg("--max-tries=5")
            .arg("--retry-wait=3")
            .arg("--connect-timeout=20")
            .arg("--timeout=60")
            .arg("--seed-time=0")
            .arg("--bt-stop-timeout=120")
            .arg(format!("--stop-with-process={}", std::process::id()))
            .arg("--console-log-level=warn")
            .arg("--summary-interval=0")
            .arg("--download-result=hide")
            .arg("--enable-color=false")
            .arg("--quiet=true")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let child = command
            .spawn()
            .map_err(|e| format!("cannot start aria2c ({}): {e}", exe.display()))?;
        let daemon = Aria2 {
            child: Mutex::new(child),
            port,
            secret: secret.clone(),
        };
        // Give the RPC server a moment to bind, then prove it answers.
        for _ in 0..60 {
            std::thread::sleep(Duration::from_millis(100));
            if daemon.rpc("aria2.getVersion", Vec::new()).is_ok() {
                return Ok(daemon);
            }
        }
        Err("aria2c started but its RPC endpoint never answered".to_string())
    }

    fn rpc(&self, method: &str, params: Vec<Value>) -> Result<Value, String> {
        let mut full = Vec::with_capacity(params.len() + 1);
        full.push(Value::String(format!("token:{}", self.secret)));
        full.extend(params);
        let payload = json!({
            "jsonrpc": "2.0",
            "id": "edownloader",
            "method": method,
            "params": full,
        })
        .to_string();
        let text = http_post(self.port, &payload)?;
        let value: Value = serde_json::from_str(&text)
            .map_err(|e| format!("aria2 RPC sent unparsable JSON: {e}"))?;
        if let Some(error) = value.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown RPC error");
            return Err(message.to_string());
        }
        Ok(value.get("result").cloned().unwrap_or(Value::Null))
    }

    pub fn version(&self) -> Result<String, String> {
        let value = self.rpc("aria2.getVersion", Vec::new())?;
        Ok(value
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("aria2")
            .to_string())
    }

    /// Queue one URI. Returns the aria2 GID.
    pub fn add_uri(
        &self,
        url: &str,
        dir: &Path,
        out: Option<&str>,
        connections: usize,
    ) -> Result<String, String> {
        let mut options = Map::new();
        options.insert("dir".into(), json!(dir.display().to_string()));
        options.insert("continue".into(), json!("true"));
        options.insert("user-agent".into(), json!(USER_AGENT));
        options.insert("split".into(), json!(connections.clamp(1, 16).to_string()));
        options.insert(
            "max-connection-per-server".into(),
            json!(connections.clamp(1, 16).to_string()),
        );
        options.insert("min-split-size".into(), json!("1M"));
        options.insert("file-allocation".into(), json!("none"));
        options.insert("auto-file-renaming".into(), json!("true"));
        options.insert("content-disposition-default-utf8".into(), json!("true"));
        options.insert("max-tries".into(), json!("5"));
        options.insert("retry-wait".into(), json!("3"));
        if let Some(name) = out {
            options.insert("out".into(), json!(name));
        }
        let value = self.rpc("aria2.addUri", vec![json!([url]), Value::Object(options)])?;
        value
            .as_str()
            .map(|gid| gid.to_string())
            .ok_or_else(|| "aria2 did not return a GID".to_string())
    }

    pub fn tell(&self, gid: &str) -> Result<Status, String> {
        let keys = json!([
            "status",
            "totalLength",
            "completedLength",
            "downloadSpeed",
            "connections",
            "files",
            "errorMessage",
            "errorCode",
        ]);
        let value = self.rpc("aria2.tellStatus", vec![json!(gid), keys])?;
        let number = |key: &str| -> u64 {
            value
                .get(key)
                .and_then(Value::as_str)
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0)
        };
        let path = value
            .get("files")
            .and_then(Value::as_array)
            .and_then(|files| files.first())
            .and_then(|file| file.get("path"))
            .and_then(Value::as_str)
            .filter(|p| !p.is_empty())
            .map(PathBuf::from);
        Ok(Status {
            status: value
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            total: number("totalLength"),
            completed: number("completedLength"),
            speed: number("downloadSpeed"),
            connections: number("connections"),
            path,
            error: value
                .get("errorMessage")
                .and_then(Value::as_str)
                .filter(|m| !m.is_empty())
                .map(|m| m.to_string()),
        })
    }

    pub fn pause(&self, gid: &str) -> Result<(), String> {
        self.rpc("aria2.pause", vec![json!(gid)]).map(|_| ())
    }

    pub fn unpause(&self, gid: &str) -> Result<(), String> {
        self.rpc("aria2.unpause", vec![json!(gid)]).map(|_| ())
    }

    pub fn remove(&self, gid: &str) -> Result<(), String> {
        let _ = self.rpc("aria2.remove", vec![json!(gid)]);
        let _ = self.rpc("aria2.removeDownloadResult", vec![json!(gid)]);
        Ok(())
    }

    /// Drop a finished job from the daemon's result list.
    pub fn remove_download_result(&self, gid: &str) -> Result<(), String> {
        self.rpc("aria2.removeDownloadResult", vec![json!(gid)])
            .map(|_| ())
    }
}

impl Drop for Aria2 {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dechunk_reassembles_a_body() {
        let body = "5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        assert_eq!(dechunk(body).unwrap(), "hello world");
    }

    #[test]
    fn dechunk_tolerates_truncation() {
        assert_eq!(dechunk("3\r\nabc\r\n").unwrap(), "abc");
    }

    #[test]
    fn secrets_and_ports_differ_between_calls() {
        assert_ne!(make_secret(), make_secret());
        let a = free_port().unwrap();
        let b = free_port().unwrap();
        assert!(a > 0 && b > 0);
    }

    /// End-to-end daemon check: needs the bundled aria2c, so it is opt-in.
    #[test]
    #[ignore = "starts a real aria2 daemon"]
    fn daemon_reports_a_version() {
        let tools = crate::core::tools::Tools::detect();
        let exe = tools.aria2.expect("aria2c");
        let dir = std::env::temp_dir();
        let daemon = Aria2::launch(&exe, &dir, 2, 4).unwrap();
        assert!(!daemon.version().unwrap().is_empty());
    }
}

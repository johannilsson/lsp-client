use std::io::{self, Read, Write};
use std::net::TcpStream;
#[cfg(not(unix))]
use std::process::{ChildStdin, ChildStdout};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Framed LSP message reader — owns a byte accumulator so partial reads work correctly.
pub struct MessageReader {
    inner: Box<dyn Read>,
    buf: Vec<u8>,
}

impl MessageReader {
    pub fn new(r: impl Read + 'static) -> Self {
        Self { inner: Box::new(r), buf: Vec::new() }
    }

    /// Returns Ok(true) if data was read, Ok(false) on timeout/WouldBlock, Err on real errors.
    fn fill(&mut self) -> io::Result<bool> {
        let mut tmp = [0u8; 4096];
        match self.inner.read(&mut tmp) {
            Ok(0) => Err(io::Error::new(io::ErrorKind::UnexpectedEof, "server closed connection")),
            Ok(n) => { self.buf.extend_from_slice(&tmp[..n]); Ok(true) }
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }

    pub fn read_message(&mut self) -> Result<serde_json::Value> {
        loop {
            if let Some(msg) = self.try_parse()? {
                return Ok(msg);
            }
            match self.fill() {
                Ok(true) => {}
                Ok(false) => {
                    // fill() returns Ok(false) when SO_RCVTIMEO fires — surface it as an error
                    return Err(io::Error::from(io::ErrorKind::TimedOut).into());
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Returns None if no complete message is available yet (timeout or partial data).
    pub fn try_read_message(&mut self) -> Result<Option<serde_json::Value>> {
        if let Some(msg) = self.try_parse()? {
            return Ok(Some(msg));
        }
        match self.fill() {
            Ok(false) => Ok(None), // timeout — nothing available
            Ok(true) => Ok(self.try_parse()?),
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                Ok(None)
            }
            Err(e) => Err(e.into()),
        }
    }

    fn try_parse(&mut self) -> Result<Option<serde_json::Value>> {
        let Some(header_end) = find_bytes(&self.buf, b"\r\n\r\n") else {
            return Ok(None);
        };

        let header_block = std::str::from_utf8(&self.buf[..header_end])?.to_owned();

        let content_length = header_block
            .lines()
            .find_map(|line| {
                let lower = line.to_ascii_lowercase();
                lower.strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().ok())
            })
            .flatten()
            .ok_or("missing or invalid Content-Length header")?;

        if self.buf.len() < header_end + 4 + content_length {
            return Ok(None); // body not yet complete
        }

        self.buf.drain(..header_end + 4);
        let body: Vec<u8> = self.buf.drain(..content_length).collect();
        Ok(Some(serde_json::from_slice(&body)?))
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Allows setting a read timeout on the underlying socket.
enum TimeoutCtrl {
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
}

impl TimeoutCtrl {
    fn set_read_timeout(&self, d: Option<Duration>) {
        match self {
            TimeoutCtrl::Tcp(s) => { let _ = s.set_read_timeout(d); }
            #[cfg(unix)]
            TimeoutCtrl::Unix(s) => { let _ = s.set_read_timeout(d); }
        }
    }

    fn is_tcp(&self) -> bool {
        matches!(self, TimeoutCtrl::Tcp(_))
    }
}

/// Owns the transport I/O and optionally a child process (for stdio mode).
pub struct Transport {
    pub reader: MessageReader,
    pub writer: Box<dyn Write>,
    timeout_ctrl: Option<TimeoutCtrl>,
    _child: Option<Child>,
}

impl Transport {
    /// Connect to a running server over TCP, auto-starting it if not available.
    pub fn tcp_with_autostart(
        host: &str,
        port: u16,
        server_bin: &str,
        verbose: bool,
    ) -> Result<Self> {
        match TcpStream::connect((host, port)) {
            Ok(stream) => return Ok(Self::from_tcp(stream)?),
            Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
                if verbose {
                    eprintln!("[DEBUG] Connection refused, starting {server_bin}...");
                }
                eprintln!("kotlin-lsp server not running — starting it...");
            }
            Err(e) => return Err(Box::new(e)),
        }

        Command::new(server_bin)
            .args(["--socket", &format!("{host}:{port}"), "--multi-client"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    format!(
                        "Error: '{server_bin}' not found. Install it with:\n  brew install JetBrains/utils/kotlin-lsp"
                    )
                } else {
                    e.to_string()
                }
            })?;

        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            std::thread::sleep(Duration::from_secs(2));
            match TcpStream::connect((host, port)) {
                Ok(stream) => return Ok(Self::from_tcp(stream)?),
                Err(_) if Instant::now() < deadline => continue,
                Err(_) => {
                    return Err(format!(
                        "kotlin-lsp did not become ready on {host}:{port} within 30s"
                    )
                    .into())
                }
            }
        }
    }

    fn from_tcp(stream: TcpStream) -> Result<Self> {
        stream.set_read_timeout(Some(Duration::from_secs(60)))?;
        let writer = stream.try_clone()?;
        let timeout_ctrl = stream.try_clone()?;
        Ok(Self {
            reader: MessageReader::new(stream),
            writer: Box::new(writer),
            timeout_ctrl: Some(TimeoutCtrl::Tcp(timeout_ctrl)),
            _child: None,
        })
    }

    /// Spawn an LSP server and communicate over its stdin/stdout.
    #[cfg(not(unix))]
    pub fn stdio(server_bin: &str, server_args: &[&str]) -> Result<Self> {
        let mut child = Command::new(server_bin)
            .args(server_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    format!(
                        "Error: '{server_bin}' not found. Install it with:\n  brew install JetBrains/utils/kotlin-lsp"
                    )
                } else {
                    e.to_string()
                }
            })?;

        let stdin: ChildStdin = child.stdin.take().unwrap();
        let stdout: ChildStdout = child.stdout.take().unwrap();

        Ok(Self {
            reader: MessageReader::new(stdout),
            writer: Box::new(stdin),
            timeout_ctrl: None,
            _child: Some(child),
        })
    }

    /// Connect to a running lsp-client daemon over a Unix domain socket.
    ///
    /// `supports_timeout()` returns false for Unix socket connections so that
    /// `LspSession` does not call `wait_for_idle` on the client side (the daemon
    /// handles idle detection internally).  However, an explicit `--timeout` flag
    /// will still apply via `set_read_timeout` to cap hanging requests.
    #[cfg(unix)]
    pub fn unix_socket(path: &str) -> Result<Self> {
        use std::os::unix::net::UnixStream;
        let stream = UnixStream::connect(path)
            .map_err(|e| format!("cannot connect to daemon socket {path}: {e}"))?;
        let writer = stream.try_clone()?;
        let timeout_ctrl = stream.try_clone()?;
        Ok(Self {
            reader: MessageReader::new(stream),
            writer: Box::new(writer),
            timeout_ctrl: Some(TimeoutCtrl::Unix(timeout_ctrl)),
            _child: None,
        })
    }

    pub fn set_read_timeout(&self, d: Option<Duration>) {
        if let Some(ctrl) = &self.timeout_ctrl {
            ctrl.set_read_timeout(d);
        }
    }

    /// Returns true if this transport should trigger wait_for_idle (TCP mode only).
    /// Unix socket connections skip client-side idle detection; the daemon handles it.
    pub fn supports_timeout(&self) -> bool {
        self.timeout_ctrl.as_ref().map(|c| c.is_tcp()).unwrap_or(false)
    }

    pub fn send_raw(&mut self, body: &str) -> Result<()> {
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        self.writer.write_all(header.as_bytes())?;
        self.writer.write_all(body.as_bytes())?;
        self.writer.flush()?;
        Ok(())
    }
}

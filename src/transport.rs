use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Framed LSP message reader — owns a byte accumulator so partial reads work correctly.
pub struct MessageReader {
    inner: Box<dyn Read>,
    buf: Vec<u8>,
}

impl MessageReader {
    fn new(r: impl Read + 'static) -> Self {
        Self { inner: Box::new(r), buf: Vec::new() }
    }

    fn fill(&mut self) -> io::Result<()> {
        let mut tmp = [0u8; 4096];
        let n = self.inner.read(&mut tmp)?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "server closed connection"));
        }
        self.buf.extend_from_slice(&tmp[..n]);
        Ok(())
    }

    pub fn read_message(&mut self) -> Result<serde_json::Value> {
        // Read until we have a complete header block ending with \r\n\r\n
        let header_end = loop {
            if let Some(pos) = find_bytes(&self.buf, b"\r\n\r\n") {
                break pos;
            }
            self.fill()?;
        };

        let header_block = std::str::from_utf8(&self.buf[..header_end])?.to_owned();
        self.buf.drain(..header_end + 4);

        let content_length = header_block
            .lines()
            .find_map(|line| {
                let lower = line.to_ascii_lowercase();
                lower.strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().ok())
            })
            .flatten()
            .ok_or("missing or invalid Content-Length header")?;

        while self.buf.len() < content_length {
            self.fill()?;
        }

        let body: Vec<u8> = self.buf.drain(..content_length).collect();
        Ok(serde_json::from_slice(&body)?)
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Owns the transport I/O and optionally a child process (for stdio mode).
pub struct Transport {
    pub reader: MessageReader,
    pub writer: Box<dyn Write>,
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
            Ok(stream) => {
                stream.set_read_timeout(Some(Duration::from_secs(60)))?;
                let writer = stream.try_clone()?;
                return Ok(Self {
                    reader: MessageReader::new(stream),
                    writer: Box::new(writer),
                    _child: None,
                });
            }
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
                Ok(stream) => {
                    stream.set_read_timeout(Some(Duration::from_secs(60)))?;
                    let writer = stream.try_clone()?;
                    return Ok(Self {
                        reader: MessageReader::new(stream),
                        writer: Box::new(writer),
                        _child: None,
                    });
                }
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

    /// Spawn an LSP server and communicate over its stdin/stdout.
    pub fn stdio(server_bin: &str, extra_args: &[&str]) -> Result<Self> {
        let mut child = Command::new(server_bin)
            .arg("--stdio")
            .args(extra_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
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

        let stdin: ChildStdin = child.stdin.take().unwrap();
        let stdout: ChildStdout = child.stdout.take().unwrap();

        Ok(Self {
            reader: MessageReader::new(stdout),
            writer: Box::new(stdin),
            _child: Some(child),
        })
    }

    pub fn send_raw(&mut self, body: &str) -> Result<()> {
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        self.writer.write_all(header.as_bytes())?;
        self.writer.write_all(body.as_bytes())?;
        self.writer.flush()?;
        Ok(())
    }
}

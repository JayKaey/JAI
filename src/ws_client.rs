//! A small WebSocket client (RFC 6455) for Jarvis's own Discord account: the gateway and the
//! voice gateway. TLS comes from rustls with ring (the mail client's stack), frames are never
//! compressed, and every frame Jarvis sends is masked as the RFC requires of a client. Text,
//! binary, fragmented messages, ping/pong and the closing handshake: all Discord needs.
//!
//! Under `cfg(test)` the server side of the handshake (`accept`) lets the tests run local mock
//! gateways, so no test ever talks to Discord.

use base64::Engine;
use ring::rand::SecureRandom;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::Mutex;

/// The largest message accepted (a READY for a big server can be a few megabytes).
const MAX_MESSAGE: usize = 64 << 20;
/// RFC 6455's fixed key suffix for `Sec-WebSocket-Accept`.
const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const OP_CONTINUATION: u8 = 0x0;
const OP_TEXT: u8 = 0x1;
const OP_BINARY: u8 = 0x2;
const OP_CLOSE: u8 = 0x8;
const OP_PING: u8 = 0x9;
const OP_PONG: u8 = 0xA;

pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
type Stream = BufReader<Box<dyn Io>>;

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Text(String),
    Binary(Vec<u8>),
    /// The peer closed the connection: its close code (when it gave one) and reason.
    Close(Option<u16>, String),
}

/// The sending half; clones share the connection (heartbeats go out beside the replies).
#[derive(Clone)]
pub struct Writer {
    io: Arc<Mutex<WriteHalf<Stream>>>,
    /// Clients mask what they send; a server (the tests' mock) does not.
    mask: bool,
}

/// The receiving half. Pings are answered on the way through `writer`.
pub struct Reader {
    io: ReadHalf<Stream>,
    writer: Writer,
}

/// The parts of a `ws://` or `wss://` address.
#[derive(Debug, PartialEq)]
pub struct Target {
    pub tls: bool,
    pub host: String,
    pub port: u16,
    /// Path and query, always starting with '/'.
    pub path: String,
}

pub fn parse_url(url: &str) -> Result<Target, String> {
    let (tls, rest) = if let Some(rest) = url.strip_prefix("wss://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("ws://") {
        (false, rest)
    } else {
        return Err(format!("'{url}' is not a WebSocket address"));
    };
    let (authority, path) = match rest.find(['/', '?']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let path = if path.starts_with('?') { format!("/{path}") } else { path.to_string() };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host.to_string(), port.parse::<u16>().map_err(|_| format!("bad port in '{url}'"))?),
        None => (authority.to_string(), if tls { 443 } else { 80 }),
    };
    if host.is_empty() {
        return Err(format!("no host in '{url}'"));
    }
    Ok(Target { tls, host, port, path })
}

/// `Sec-WebSocket-Accept` for a key: base64(SHA-1(key + GUID)).
pub fn accept_key(key: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, format!("{key}{GUID}").as_bytes());
    base64::engine::general_purpose::STANDARD.encode(digest.as_ref())
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    // The OS generator does not fail on Windows or Linux; a zero key would still be valid framing.
    let _ = ring::rand::SystemRandom::new().fill(&mut bytes);
    bytes
}

/// Opens `url` (`wss://` or, for the tests' mocks, `ws://`) and completes the handshake.
pub async fn connect(url: &str) -> Result<(Reader, Writer), String> {
    let target = parse_url(url)?;
    let tcp = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect((target.host.as_str(), target.port)))
        .await
        .map_err(|_| format!("{}:{} did not answer", target.host, target.port))?
        .map_err(|e| format!("cannot reach {}:{}: {e}", target.host, target.port))?;
    let _ = tcp.set_nodelay(true);
    let io: Box<dyn Io> = if target.tls { Box::new(tls(&target.host, tcp).await?) } else { Box::new(tcp) };
    let mut stream = BufReader::new(io);
    let key = base64::engine::general_purpose::STANDARD.encode(random_bytes::<16>());
    let default_port = if target.tls { 443 } else { 80 };
    let host = if target.port == default_port { target.host.clone() } else { format!("{}:{}", target.host, target.port) };
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\nUser-Agent: JARVIS\r\n\r\n",
        target.path
    );
    let inner = stream.get_mut();
    inner.write_all(request.as_bytes()).await.map_err(|e| format!("handshake: {e}"))?;
    inner.flush().await.map_err(|e| format!("handshake: {e}"))?;
    let head = tokio::time::timeout(Duration::from_secs(10), read_head(&mut stream)).await.map_err(|_| format!("{} did not finish the handshake", target.host))??;
    let status = head.lines().next().unwrap_or("").trim().to_string();
    if !status.starts_with("HTTP/1.1 101") {
        return Err(format!("{} refused the WebSocket upgrade ({status})", target.host));
    }
    if header(&head, "sec-websocket-accept").as_deref() != Some(accept_key(&key).as_str()) {
        return Err(format!("{} answered the handshake with a wrong accept key", target.host));
    }
    Ok(split(stream, true))
}

async fn tls(host: &str, tcp: TcpStream) -> Result<tokio_rustls::client::TlsStream<TcpStream>, String> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let name = rustls::pki_types::ServerName::try_from(host.to_string()).map_err(|_| format!("'{host}' is not a valid host name"))?;
    tokio::time::timeout(Duration::from_secs(10), tokio_rustls::TlsConnector::from(Arc::new(config)).connect(name, tcp))
        .await
        .map_err(|_| format!("TLS handshake with {host} timed out"))?
        .map_err(|e| format!("TLS handshake with {host} failed: {e}"))
}

/// The status line and headers, up to the blank line (whatever follows stays buffered: it is
/// the first frames).
async fn read_head(stream: &mut Stream) -> Result<String, String> {
    let mut head = String::new();
    loop {
        let mut line = String::new();
        let n = stream.read_line(&mut line).await.map_err(|e| format!("handshake: {e}"))?;
        if n == 0 {
            return Err("the connection closed during the handshake".into());
        }
        if line == "\r\n" || line == "\n" {
            return Ok(head);
        }
        head.push_str(&line);
        if head.len() > 32 * 1024 {
            return Err("handshake headers too long".into());
        }
    }
}

fn header(head: &str, name: &str) -> Option<String> {
    head.lines().skip(1).find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim().eq_ignore_ascii_case(name).then(|| value.trim().to_string())
    })
}

fn split(stream: Stream, mask: bool) -> (Reader, Writer) {
    let (read, write) = tokio::io::split(stream);
    let writer = Writer { io: Arc::new(Mutex::new(write)), mask };
    (Reader { io: read, writer: writer.clone() }, writer)
}

/// One frame on the wire. Control frames and the last frame of a message carry FIN.
fn encode_frame(fin: bool, opcode: u8, payload: &[u8], mask: Option<[u8; 4]>) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 14);
    out.push(if fin { 0x80 } else { 0 } | opcode);
    let masked = if mask.is_some() { 0x80 } else { 0 };
    match payload.len() {
        n if n < 126 => out.push(masked | n as u8),
        n if n <= 0xFFFF => {
            out.push(masked | 126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            out.push(masked | 127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    match mask {
        Some(key) => {
            out.extend_from_slice(&key);
            out.extend(payload.iter().enumerate().map(|(i, b)| b ^ key[i % 4]));
        }
        None => out.extend_from_slice(payload),
    }
    out
}

impl Writer {
    pub async fn send_text(&self, text: &str) -> Result<(), String> {
        self.frame(OP_TEXT, text.as_bytes()).await
    }

    pub async fn send_binary(&self, data: &[u8]) -> Result<(), String> {
        self.frame(OP_BINARY, data).await
    }

    /// Starts the closing handshake with `code` (1000 = normal).
    pub async fn close(&self, code: u16, reason: &str) -> Result<(), String> {
        let mut payload = code.to_be_bytes().to_vec();
        payload.extend_from_slice(reason.as_bytes());
        self.frame(OP_CLOSE, &payload).await
    }

    async fn frame(&self, opcode: u8, payload: &[u8]) -> Result<(), String> {
        let bytes = encode_frame(true, opcode, payload, self.mask.then(random_bytes::<4>));
        self.raw(&bytes).await
    }

    async fn raw(&self, bytes: &[u8]) -> Result<(), String> {
        let mut io = self.io.lock().await;
        io.write_all(bytes).await.map_err(|e| format!("connection lost: {e}"))?;
        io.flush().await.map_err(|e| format!("connection lost: {e}"))
    }
}

impl Reader {
    /// The next whole message. Pings are answered, pongs dropped, a close is echoed.
    pub async fn recv(&mut self) -> Result<Message, String> {
        let mut message: Option<(u8, Vec<u8>)> = None;
        loop {
            let (fin, opcode, payload) = self.read_frame().await?;
            match opcode {
                OP_PING => {
                    self.writer.frame(OP_PONG, &payload).await?;
                    continue;
                }
                OP_PONG => continue,
                OP_CLOSE => {
                    let code = (payload.len() >= 2).then(|| u16::from_be_bytes([payload[0], payload[1]]));
                    let reason = String::from_utf8_lossy(payload.get(2..).unwrap_or_default()).into_owned();
                    // The closing handshake: echo the code (the peer may already be gone).
                    let _ = self.writer.frame(OP_CLOSE, payload.get(..2).unwrap_or_default()).await;
                    return Ok(Message::Close(code, reason));
                }
                OP_CONTINUATION => {
                    let Some((_, data)) = message.as_mut() else {
                        return Err("a continuation frame without a message".into());
                    };
                    data.extend_from_slice(&payload);
                    if data.len() > MAX_MESSAGE {
                        return Err("message too large".into());
                    }
                }
                OP_TEXT | OP_BINARY => {
                    if message.is_some() {
                        return Err("a new message inside a fragmented one".into());
                    }
                    message = Some((opcode, payload));
                }
                other => return Err(format!("unknown frame opcode {other}")),
            }
            if fin {
                let Some((opcode, data)) = message.take() else { continue };
                return match opcode {
                    OP_TEXT => String::from_utf8(data).map(Message::Text).map_err(|_| "a text frame that is not UTF-8".to_string()),
                    _ => Ok(Message::Binary(data)),
                };
            }
        }
    }

    async fn read_frame(&mut self) -> Result<(bool, u8, Vec<u8>), String> {
        let lost = |e: std::io::Error| format!("connection lost: {e}");
        let mut head = [0u8; 2];
        self.io.read_exact(&mut head).await.map_err(lost)?;
        if head[0] & 0x70 != 0 {
            return Err("a compressed or extended frame (never negotiated)".into());
        }
        let fin = head[0] & 0x80 != 0;
        let opcode = head[0] & 0x0F;
        let len = match head[1] & 0x7F {
            126 => {
                let mut b = [0u8; 2];
                self.io.read_exact(&mut b).await.map_err(lost)?;
                u16::from_be_bytes(b) as usize
            }
            127 => {
                let mut b = [0u8; 8];
                self.io.read_exact(&mut b).await.map_err(lost)?;
                usize::try_from(u64::from_be_bytes(b)).unwrap_or(usize::MAX)
            }
            n => n as usize,
        };
        if len > MAX_MESSAGE {
            return Err("message too large".into());
        }
        let key = if head[1] & 0x80 != 0 {
            let mut k = [0u8; 4];
            self.io.read_exact(&mut k).await.map_err(lost)?;
            Some(k)
        } else {
            None
        };
        let mut payload = vec![0u8; len];
        self.io.read_exact(&mut payload).await.map_err(lost)?;
        if let Some(k) = key {
            for (i, b) in payload.iter_mut().enumerate() {
                *b ^= k[i % 4];
            }
        }
        Ok((fin, opcode, payload))
    }
}

/// The server side of the handshake, for the tests' local mock gateways: the request head
/// (to check the path and query) and the connection.
#[cfg(test)]
pub async fn accept(tcp: TcpStream) -> Result<(String, Reader, Writer), String> {
    let io: Box<dyn Io> = Box::new(tcp);
    let mut stream = BufReader::new(io);
    let head = read_head(&mut stream).await?;
    let key = header(&head, "sec-websocket-key").ok_or("no Sec-WebSocket-Key")?;
    let response = format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n", accept_key(&key));
    stream.get_mut().write_all(response.as_bytes()).await.map_err(|e| e.to_string())?;
    let (reader, writer) = split(stream, false);
    Ok((head, reader, writer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_accept_key_is_the_rfc_example() {
        assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn addresses_parse_with_and_without_port_and_path() {
        assert_eq!(parse_url("wss://gateway.discord.gg/?v=10&encoding=json").unwrap(), Target { tls: true, host: "gateway.discord.gg".into(), port: 443, path: "/?v=10&encoding=json".into() });
        assert_eq!(parse_url("wss://c-fra11.discord.media:443?v=8").unwrap(), Target { tls: true, host: "c-fra11.discord.media".into(), port: 443, path: "/?v=8".into() });
        assert_eq!(parse_url("ws://127.0.0.1:5123").unwrap(), Target { tls: false, host: "127.0.0.1".into(), port: 5123, path: "/".into() });
        assert!(parse_url("https://discord.com").is_err());
        assert!(parse_url("wss://:443/").is_err());
        assert!(parse_url("wss://host:port/").is_err());
    }

    #[test]
    fn frames_carry_their_length_and_mask() {
        assert_eq!(encode_frame(true, OP_TEXT, b"hi", None), vec![0x81, 2, b'h', b'i']);
        let masked = encode_frame(true, OP_TEXT, b"hi", Some([1, 2, 3, 4]));
        assert_eq!(masked, vec![0x81, 0x82, 1, 2, 3, 4, b'h' ^ 1, b'i' ^ 2]);
        let medium = encode_frame(false, OP_BINARY, &[7u8; 300], None);
        assert_eq!(&medium[..4], &[0x02, 126, 1, 44]);
        let large = encode_frame(true, OP_BINARY, &vec![0u8; 70_000], None);
        assert_eq!(&large[..10], &[0x82, 127, 0, 0, 0, 0, 0, 1, 0x11, 0x70]);
    }

    #[tokio::test]
    async fn a_session_with_a_local_server_handles_fragments_pings_and_close() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let (head, mut reader, writer) = accept(tcp).await.unwrap();
            assert!(head.starts_with("GET /?v=10&encoding=json HTTP/1.1"), "{head}");
            writer.send_text("{\"op\":10}").await.unwrap();
            // One binary message in two frames, a ping between them.
            writer.raw(&encode_frame(false, OP_BINARY, &[1, 2], None)).await.unwrap();
            writer.raw(&encode_frame(true, OP_PING, b"beat", None)).await.unwrap();
            writer.raw(&encode_frame(true, OP_CONTINUATION, &[3], None)).await.unwrap();
            // The client's pong and its (masked) text reach the server unmasked.
            let reply = reader.recv().await.unwrap();
            writer.close(4004, "Authentication failed.").await.unwrap();
            reply
        });
        let (mut reader, writer) = connect(&format!("ws://127.0.0.1:{port}/?v=10&encoding=json")).await.unwrap();
        assert_eq!(reader.recv().await.unwrap(), Message::Text("{\"op\":10}".into()));
        assert_eq!(reader.recv().await.unwrap(), Message::Binary(vec![1, 2, 3]));
        writer.send_text("identify").await.unwrap();
        assert_eq!(reader.recv().await.unwrap(), Message::Close(Some(4004), "Authentication failed.".into()));
        assert_eq!(server.await.unwrap(), Message::Text("identify".into()));
    }
}

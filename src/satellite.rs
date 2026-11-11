//! The mobile web satellite: `https://<pc-ip>:8443/satellite?k=<token>` on Sir's phone or iPad
//! (open it from the QR code in Quick Connections). A small Stark HUD page that streams the
//! phone's live GPS (`navigator.geolocation.watchPosition`, high accuracy) to JAI, lets Sir
//! type or speak to him, reads his answers aloud on the phone and shows what JAI "teleports"
//! there.
//!
//! The server is a minimal HTTP/1.1 listener on tokio. Browsers only grant geolocation to secure
//! pages, so it speaks HTTPS with a self-signed ECDSA P-256 certificate generated here with `ring`
//! (kept in `satellite_cert.der` / `satellite_key.p8`, renewed when the PC's address changes).
//! Every request must carry the pairing token (`?k=` or `X-JAI-Key`).
//!
//! Endpoints: `GET /satellite`, `POST /api/location`, `POST /api/chat`, `GET /api/state`,
//! `POST /api/whatsapp/incoming` (bridge webhooks), `GET /satellite/cert.der`.

use crate::app::Jai;
use crate::config::ConfigStore;
use crate::hud::{HudCommand, HudEvent};
use crate::location_engine::PhoneSample;
use serde_json::{json, Value};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

const PAGE: &str = include_str!("../assets/satellite.html");
const CERT_FILE: &str = "satellite_cert.der";
const KEY_FILE: &str = "satellite_key.p8";
const SAN_FILE: &str = "satellite_cert.json";
const MAX_HEADER: usize = 16 * 1024;
const MAX_BODY: usize = 256 * 1024;
/// Air-Drop files and scanned pages (base64 of a phone photo or a document).
const MAX_UPLOAD: usize = 48 * 1024 * 1024;

/// The PC's address on the local network (the route to the internet; no packet is sent).
/// Looked up at most once a minute (the hub shows it every frame).
pub fn lan_ip() -> IpAddr {
    static CACHE: std::sync::Mutex<Option<(IpAddr, std::time::Instant)>> = std::sync::Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((ip, at)) = *cache {
        if at.elapsed() < Duration::from_secs(60) {
            return ip;
        }
    }
    let ip = UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("8.8.8.8:80")?;
            s.local_addr()
        })
        .map(|a| a.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
    *cache = Some((ip, std::time::Instant::now()));
    ip
}

/// What the QR code encodes.
pub fn pairing_url(config: &ConfigStore) -> String {
    let s = config.read(|c| c.satellite.clone());
    format!("{}://{}:{}/satellite?token={}", if s.https { "https" } else { "http" }, lan_ip(), s.port, urlencoding::encode(&s.token))
}

/// The QR code of a URL as (side length, dark modules row by row).
pub fn qr_matrix(text: &str) -> Option<(usize, Vec<bool>)> {
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    let width = code.width();
    let modules = code.to_colors().into_iter().map(|c| c == qrcode::Color::Dark).collect();
    Some((width, modules))
}

// ------------------------------------------------------------- self-signed TLS cert ----

mod cert {
    use ring::rand::{SecureRandom, SystemRandom};
    use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};
    use std::net::IpAddr;

    fn len(n: usize) -> Vec<u8> {
        if n < 0x80 {
            vec![n as u8]
        } else {
            let bytes: Vec<u8> = n.to_be_bytes().iter().copied().skip_while(|b| *b == 0).collect();
            let mut out = vec![0x80 | bytes.len() as u8];
            out.extend(bytes);
            out
        }
    }

    fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        out.extend(len(content.len()));
        out.extend_from_slice(content);
        out
    }

    fn seq(parts: &[Vec<u8>]) -> Vec<u8> {
        tlv(0x30, &parts.concat())
    }

    const ECDSA_SHA256: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03, 0x02];
    const EC_PUBLIC_KEY: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x02, 0x01];
    const PRIME256V1: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x03, 0x01, 0x07];
    const COMMON_NAME: &[u8] = &[0x55, 0x04, 0x03];
    const ORGANIZATION: &[u8] = &[0x55, 0x04, 0x0A];
    const SUBJECT_ALT_NAME: &[u8] = &[0x55, 0x1D, 0x11];
    const BASIC_CONSTRAINTS: &[u8] = &[0x55, 0x1D, 0x13];
    const KEY_USAGE: &[u8] = &[0x55, 0x1D, 0x0F];
    const EXT_KEY_USAGE: &[u8] = &[0x55, 0x1D, 0x25];
    const SERVER_AUTH: &[u8] = &[0x2B, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x01];

    fn name() -> Vec<u8> {
        let rdn = |oid: &[u8], value: &str| tlv(0x31, &seq(&[tlv(0x06, oid), tlv(0x0C, value.as_bytes())]));
        seq(&[rdn(ORGANIZATION, "Stark Industries"), rdn(COMMON_NAME, "J.A.I. Satellite")])
    }

    fn utc_time(t: chrono::DateTime<chrono::Utc>) -> Vec<u8> {
        tlv(0x17, t.format("%y%m%d%H%M%SZ").to_string().as_bytes())
    }

    fn extension(oid: &[u8], critical: bool, value: Vec<u8>) -> Vec<u8> {
        let mut parts = vec![tlv(0x06, oid)];
        if critical {
            parts.push(vec![0x01, 0x01, 0xFF]);
        }
        parts.push(tlv(0x04, &value));
        seq(&parts)
    }

    /// (certificate DER, PKCS#8 private key) for these addresses (plus localhost).
    pub fn generate(ips: &[IpAddr], hostnames: &[String]) -> Result<(Vec<u8>, Vec<u8>), String> {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).map_err(|_| "key generation failed")?;
        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng).map_err(|_| "key parsing failed")?;
        let mut serial = [0u8; 16];
        rng.fill(&mut serial).map_err(|_| "no randomness")?;
        serial[0] = (serial[0] & 0x7F) | 0x01;
        let now = chrono::Utc::now();
        let sig_alg = seq(&[tlv(0x06, ECDSA_SHA256)]);
        let mut public = vec![0x00];
        public.extend_from_slice(key.public_key().as_ref());
        let spki = seq(&[seq(&[tlv(0x06, EC_PUBLIC_KEY), tlv(0x06, PRIME256V1)]), tlv(0x03, &public)]);
        let mut alt: Vec<Vec<u8>> = vec![tlv(0x82, b"localhost")];
        for h in hostnames.iter().filter(|h| !h.is_empty()) {
            alt.push(tlv(0x82, h.as_bytes()));
        }
        for ip in ips {
            match ip {
                IpAddr::V4(v4) => alt.push(tlv(0x87, &v4.octets())),
                IpAddr::V6(v6) => alt.push(tlv(0x87, &v6.octets())),
            }
        }
        alt.push(tlv(0x87, &[127, 0, 0, 1]));
        let extensions = tlv(
            0xA3,
            &seq(&[
                extension(SUBJECT_ALT_NAME, false, seq(&alt)),
                extension(BASIC_CONSTRAINTS, true, seq(&[])),
                extension(KEY_USAGE, true, vec![0x03, 0x02, 0x07, 0x80]),
                extension(EXT_KEY_USAGE, false, seq(&[tlv(0x06, SERVER_AUTH)])),
            ]),
        );
        let tbs = seq(&[
            vec![0xA0, 0x03, 0x02, 0x01, 0x02],
            tlv(0x02, &serial),
            sig_alg.clone(),
            name(),
            seq(&[utc_time(now - chrono::Duration::days(1)), utc_time(now + chrono::Duration::days(397))]),
            name(),
            spki,
            extensions,
        ]);
        let signature = key.sign(&rng, &tbs).map_err(|_| "signing failed")?;
        let mut sig_bits = vec![0x00];
        sig_bits.extend_from_slice(signature.as_ref());
        let cert = seq(&[tbs, sig_alg, tlv(0x03, &sig_bits)]);
        Ok((cert, pkcs8.as_ref().to_vec()))
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn certificate_is_well_formed_der() {
            let (cert, key) = super::generate(&["192.168.1.20".parse().unwrap()], &[]).unwrap();
            assert_eq!(cert[0], 0x30);
            assert!(cert.len() > 300 && key.len() > 100);
            // rustls accepts the pair.
            let config = rustls::ServerConfig::builder_with_provider(std::sync::Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(
                    vec![rustls::pki_types::CertificateDer::from(cert)],
                    rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(key)),
                );
            assert!(config.is_ok(), "{:?}", config.err());
        }
    }
}

/// The certificate and key for this PC's current addresses (renewed when they change).
fn tls_material() -> Result<(Vec<u8>, Vec<u8>), String> {
    let ip = lan_ip();
    let host = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_default().to_lowercase();
    let want = json!({ "ip": ip.to_string(), "host": host });
    let stored = std::fs::read_to_string(SAN_FILE).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok());
    if stored.as_ref() == Some(&want) {
        if let (Ok(c), Ok(k)) = (std::fs::read(CERT_FILE), std::fs::read(KEY_FILE)) {
            return Ok((c, k));
        }
    }
    let hosts = if host.is_empty() { Vec::new() } else { vec![host.clone(), format!("{host}.local")] };
    let (c, k) = cert::generate(&[ip], &hosts)?;
    let _ = std::fs::write(CERT_FILE, &c);
    let _ = std::fs::write(KEY_FILE, &k);
    let _ = std::fs::write(SAN_FILE, want.to_string());
    info!("[SATELLITE] New self-signed certificate for {ip}");
    Ok((c, k))
}

fn acceptor() -> Result<tokio_rustls::TlsAcceptor, String> {
    let (cert, key) = tls_material()?;
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(
            vec![rustls::pki_types::CertificateDer::from(cert)],
            rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(key)),
        )
        .map_err(|e| e.to_string())?;
    Ok(tokio_rustls::TlsAcceptor::from(Arc::new(config)))
}

// --------------------------------------------------------------------- the server ----

struct Request {
    method: String,
    path: String,
    query: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    fn param(&self, name: &str) -> Option<&str> {
        self.query.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

async fn read_request<S: AsyncRead + Unpin>(stream: &mut S, token: Option<&str>) -> Result<Request, String> {
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut chunk).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("connection closed".into());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            if pos > MAX_HEADER { return Err("headers too large".into()); }
            break pos;
        }
        if buf.len() > MAX_HEADER {
            return Err("headers too large".into());
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split_whitespace();
    let method = first.next().unwrap_or("").to_uppercase();
    let target = first.next().unwrap_or("/").to_string();
    if !target.starts_with('/') || target.starts_with("//") || !matches!(first.next(), Some("HTTP/1.0" | "HTTP/1.1")) {
        return Err("invalid request target or HTTP version".into());
    }
    let headers: Vec<(String, String)> = lines.filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))).collect();
    if headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("transfer-encoding")) {
        return Err("Transfer-Encoding is not supported".into());
    }
    let lengths: Vec<_> = headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case("content-length")).collect();
    if lengths.len() > 1 { return Err("duplicate Content-Length".into()); }
    let length = match lengths.first() {
        Some((_, value)) => value.parse::<usize>().map_err(|_| "invalid Content-Length")?,
        None => 0,
    };
    let upload = target.starts_with("/api/airdrop") || target.starts_with("/api/scan");
    if length > if upload { MAX_UPLOAD } else { MAX_BODY } {
        return Err("body too large".into());
    }
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q),
        None => (target.clone(), ""),
    };
    let query = query
        .split('&')
        .filter_map(|kv| kv.split_once('=').or(Some((kv, ""))).filter(|(k, _)| !k.is_empty()))
        .map(|(k, v)| (k.to_string(), urlencoding::decode(&v.replace('+', " ")).map(|s| s.to_string()).unwrap_or_default()))
        .collect();
    let mut request = Request { method, path, query, headers, body: Vec::new() };
    if let Some(token) = token {
        let given = request.header("X-JAI-Key").or_else(|| request.param("token")).or_else(|| request.param("k")).unwrap_or("");
        let public_get = request.method == "GET" && length == 0 && (request.path == "/" || (given.is_empty() && matches!(request.path.as_str(), "/satellite" | "/satellite/")));
        if !public_get && !same_token(given, token) { return Err("pairing key required".into()); }
    }
    // Authenticate before allocating or receiving a potentially large photo/file upload.
    request.body = buf[header_end + 4..].to_vec();
    while request.body.len() < length {
        let n = stream.read(&mut chunk).await.map_err(|e| e.to_string())?;
        if n == 0 { return Err("incomplete request body".into()); }
        request.body.extend_from_slice(&chunk[..n]);
    }
    request.body.truncate(length);
    Ok(request)
}

async fn respond<S: AsyncWrite + Unpin>(stream: &mut S, status: &str, content_type: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\nContent-Security-Policy: default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; media-src 'self' blob:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(body).await;
    let _ = stream.flush().await;
}

async fn respond_json<S: AsyncWrite + Unpin>(stream: &mut S, status: &str, v: &Value) {
    respond(stream, status, "application/json; charset=utf-8", v.to_string().as_bytes()).await;
}

fn same_token(a: &str, b: &str) -> bool {
    !a.is_empty() && a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Sir typed (or spoke) on the phone: one turn, the reply comes back as text.
async fn chat(app: &Jai, text: &str) -> Value {
    let mut events = app.subscribe();
    app.handle_command(HudCommand::Chat { text: text.to_string() }).await;
    let wait = async {
        loop {
            match events.recv().await {
                Ok(HudEvent::AssistantDone { text, model, interrupted, .. }) if model != "jai" || interrupted => return Some(text),
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    };
    match tokio::time::timeout(Duration::from_secs(90), wait).await {
        Ok(Some(reply)) => json!({ "reply": reply }),
        _ => json!({ "reply": "", "error": "JAI is still working on it." }),
    }
}

async fn handle<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S, app: Jai, peer: SocketAddr) {
    // Long enough for a phone photo over Wi-Fi.
    let token = app.config().read(|c| c.satellite.token.clone());
    let req = match tokio::time::timeout(Duration::from_secs(90), read_request(&mut stream, Some(&token))).await {
        Ok(Ok(r)) => r,
        Ok(Err(error)) => {
            let status = if error == "pairing key required" { "403 Forbidden" } else { "400 Bad Request" };
            respond_json(&mut stream, status, &json!({"error":error})).await;
            return;
        }
        Err(_) => return,
    };
    let given = req.header("X-JAI-Key").or_else(|| req.param("token")).or_else(|| req.param("k")).unwrap_or("");
    if req.path == "/" || req.path.is_empty() {
        respond(&mut stream, "200 OK", "text/html; charset=utf-8", b"<!doctype html><meta name=viewport content='width=device-width'><body style='background:#000;color:#5ad;font-family:sans-serif;padding:2em'><h2>J.A.I. Satellite</h2><p>Scan the QR code in the HUD (Integrations &rarr; Quick Connections) to pair this device.</p>").await;
        return;
    }
    if !same_token(given, &token) {
        if given.is_empty() && req.method == "GET" && matches!(req.path.as_str(), "/satellite" | "/satellite/") {
            // An unpaired request gets only a bootstrap, never data or an embedded secret.
            respond(&mut stream, "200 OK", "text/html; charset=utf-8", br#"<!doctype html><meta name="viewport" content="width=device-width"><title>Pair with JAI</title><p>Scan the pairing QR code in the desktop HUD to authorize this phone.</p><script>try{const k=localStorage.getItem('jai_satellite_key');if(k)location.replace('/satellite?token='+encodeURIComponent(k));}catch(e){}</script>"#).await;
            return;
        }
        warn!("[SATELLITE] Rejected {} {} from {peer} (bad or missing pairing key)", req.method, req.path);
        respond_json(&mut stream, "403 Forbidden", &json!({ "error": "pairing key required" })).await;
        return;
    }
    let tools = app.tools().clone();
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/satellite") | ("GET", "/satellite/") => {
            let key = serde_json::to_string(&token).unwrap_or_default().replace('<', "\\u003c");
            let page = PAGE.replace("\"__JAI_KEY__\"", &key);
            respond(&mut stream, "200 OK", "text/html; charset=utf-8", page.as_bytes()).await;
            tools.stark.pulse(&["hivemind", "brain"], false);
            info!("[SATELLITE] {peer} opened the satellite");
        }
        ("GET", "/satellite/cert.der") => {
            let cert = std::fs::read(CERT_FILE).unwrap_or_default();
            respond(&mut stream, "200 OK", "application/x-x509-ca-cert", &cert).await;
        }
        ("POST", "/api/location") => match serde_json::from_slice::<PhoneSample>(&req.body) {
            Ok(sample) => {
                let cfg = tools.config.read(|c| c.location.clone());
                tools.stark.location.update_from_phone(sample, &cfg, &tools.stark.graph, &tools.stark.events);
                tools.stark.pulse(&["hivemind", "gps"], false);
                respond_json(&mut stream, "200 OK", &json!({ "ok": true })).await;
            }
            Err(e) => respond_json(&mut stream, "400 Bad Request", &json!({ "error": e.to_string() })).await,
        },
        ("POST", "/api/chat") => {
            let text = req.json()["text"].as_str().unwrap_or("").trim().to_string();
            if text.is_empty() {
                respond_json(&mut stream, "400 Bad Request", &json!({ "error": "text required" })).await;
                return;
            }
            info!("[SATELLITE] Chat from {peer}: {text}");
            tools.stark.pulse(&["hivemind", "brain"], false);
            let reply = chat(&app, &text).await;
            respond_json(&mut stream, "200 OK", &reply).await;
        }
        ("GET", "/api/state") => {
            let since: u64 = req.param("since").and_then(|s| s.parse().ok()).unwrap_or(0);
            let hud = app.hud_state();
            let fix = tools.stark.location.phone_fix();
            let outbox: Vec<Value> = tools.stark.satellite_since(since).into_iter().map(|(id, text)| json!({ "id": id, "text": text })).collect();
            respond_json(
                &mut stream,
                "200 OK",
                &json!({
                    "state": hud.state,
                    "detail": hud.detail,
                    "gps": fix.as_ref().map(|f| json!({ "lat": f.lat, "lon": f.lon, "accuracy": f.accuracy_m, "speed_kmh": f.speed_kmh, "address": f.address })),
                    "whatsapp_unread": tools.stark.whatsapp.unread_count(),
                    "auto_responder": tools.stark.inbox.active().map(|(m, a)| format!("{a} · {m} min")),
                    "outbox": outbox,
                    "telemetry": app.telemetry(),
                    "guardian": crate::remote_guardian::snapshot(),
                    "tunnel": crate::worldwide_tunnel::snapshot(),
                    "workout": tools.workout.snapshot(tools.pim.calendar()).ok(),
                    "protocols": tools.protocols.list().iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
                    "wake_relay_url": tools.config.read(|c| c.satellite.wake_relay_url.clone()),
                }),
            )
            .await;
        }
        ("GET", "/api/tunnel-health") => {
            respond_json(&mut stream, "200 OK", &json!({"service":"jai-satellite"})).await;
        }
        ("POST", "/api/pc/control") => {
            match crate::remote_guardian::run_tool(&tools, "control_remote_pc", &req.json()).await {
                Ok(message) => respond_json(&mut stream, "200 OK", &json!({"ok":true,"message":message})).await,
                Err(error) => respond_json(&mut stream, "400 Bad Request", &json!({"error":error})).await,
            }
        }
        ("POST", "/api/workout") => {
            let args = req.json();
            let result = match args["action"].as_str().unwrap_or("log") {
                "log" => {
                    let counts = args["sets"].as_u64().unwrap_or(1).try_into().ok().zip(args["reps"].as_u64().and_then(|v| u32::try_from(v).ok()));
                    match counts {
                        Some((sets,reps)) => tools.workout.log_set(args["exercise"].as_str().unwrap_or(""), sets, reps, args["weight_kg"].as_f64().unwrap_or(f64::NAN), args["calendar_uid"].as_str()).and_then(|v| serde_json::to_value(v).map_err(|e| e.to_string())),
                        None => Err("Valid sets and repetitions are required".into()),
                    }
                }
                "rest" => tools.workout.start_rest(args["seconds"].as_u64().unwrap_or(90)).and_then(|v| serde_json::to_value(v).map_err(|e| e.to_string())),
                "cancel_rest" => tools.workout.cancel_rest().map(|_| json!({"cancelled":true})),
                _ => Err("Unknown workout action".into()),
            };
            match result {
                Ok(result) => respond_json(&mut stream, "200 OK", &json!({"ok":true,"result":result})).await,
                Err(error) => respond_json(&mut stream, "400 Bad Request", &json!({"error":error})).await,
            }
        }
        ("GET", "/api/weather") => {
            let body = match crate::location_engine::weather_report(&tools, 6, None).await {
                Ok(r) => json!({ "short": r.short, "temperature": r.current.temperature, "code": r.current.code, "source": r.fix.source }),
                Err(e) => json!({ "error": e }),
            };
            respond_json(&mut stream, "200 OK", &body).await;
        }
        ("POST", "/api/airdrop") => {
            info!("[SATELLITE] Air-Drop from {peer} ({} bytes)", req.body.len());
            let reply = crate::airdrop_bridge::receive(&tools, &req.json()).await;
            respond_json(&mut stream, "200 OK", &reply).await;
        }
        ("GET", "/api/airdrop/file") => {
            let shared = req.param("id").and_then(|i| i.parse().ok()).and_then(|id| tools.stark.features.airdrop.shared(id));
            match shared.and_then(|s| std::fs::read(&s.path).ok().map(|bytes| (s, bytes))) {
                Some((s, bytes)) => {
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nContent-Disposition: attachment; filename=\"{}\"\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
                        crate::airdrop_bridge::mime_of(&s.name),
                        bytes.len(),
                        s.name.replace(['"', '\r', '\n'], "_")
                    );
                    let _ = stream.write_all(head.as_bytes()).await;
                    let _ = stream.write_all(&bytes).await;
                    let _ = stream.flush().await;
                    info!("[SATELLITE] {peer} downloaded {}", s.name);
                }
                None => respond_json(&mut stream, "404 Not Found", &json!({ "error": "no such file" })).await,
            }
        }
        ("POST", "/api/scan") => {
            let reply = crate::mobile_scanner::receive(&tools, &req.json()).await;
            respond_json(&mut stream, "200 OK", &reply).await;
        }
        ("POST", "/api/whatsapp/incoming") => {
            let got = tools.stark.whatsapp.ingest(&req.json(), "webhook");
            if !got.is_empty() {
                tools.stark.whatsapp.announce(got.clone());
            }
            respond_json(&mut stream, "200 OK", &json!({ "stored": got.len() })).await;
        }
        _ => respond_json(&mut stream, "404 Not Found", &json!({ "error": "not found" })).await,
    }
}

/// The loopback listener the tunnel forwards to: on `fixed` when that port is free (0: any).
async fn bind_origin(fixed: u16) -> std::io::Result<tokio::net::TcpListener> {
    if fixed != 0 {
        match tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, fixed)).await {
            Ok(listener) => return Ok(listener),
            Err(e) => warn!("[TUNNEL] The fixed local port {fixed} is taken ({e}); a permanent address needs it free, a quick tunnel does not."),
        }
    }
    tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await
}

/// Starts the listener (HTTPS unless switched off). Errors are logged, never fatal.
pub fn start(app: Jai, shutdown: tokio_util::sync::CancellationToken) {
    let settings = app.config().read(|c| c.satellite.clone());
    if !settings.enabled {
        info!("[SATELLITE] Disabled in the settings.");
        return;
    }
    // A separate loopback-only origin avoids exposing a plain HTTP listener on the LAN.
    // It uses the same authenticated handler as the local HTTPS service. With worldwide access
    // switched off it waits until Sir asks for phone access (`worldwide_tunnel::kick`).
    {
        let (remote_app, remote_stop) = (app.clone(), shutdown.clone());
        tokio::spawn(async move {
            while !remote_app.config().read(|c| c.satellite.worldwide_enabled) {
                tokio::select! {
                    _ = remote_stop.cancelled() => return,
                    _ = crate::worldwide_tunnel::kicked() => {}
                }
            }
            // The origin listens on the fixed port a permanent address is set up with (the
            // tunnel's settings at Cloudflare name it); when that port is taken, on any free
            // one, which is all a quick tunnel needs.
            let fixed = remote_app.config().read(|c| c.satellite.tunnel_origin_port);
            let listener = match bind_origin(fixed).await {
                Ok(l) => l,
                Err(e) => { warn!("[TUNNEL] Could not bind loopback origin: {e}"); return; }
            };
            let port = match listener.local_addr() { Ok(a) => a.port(), Err(_) => return };
            let secrets = remote_app.tools().pim.config().vault().clone();
            tokio::spawn(crate::worldwide_tunnel::run(remote_app.config().clone(), secrets, port, remote_stop.clone()));
            let slots = Arc::new(tokio::sync::Semaphore::new(16));
            loop {
                let connection = tokio::select! { _ = remote_stop.cancelled() => break, result = listener.accept() => result };
                if let Ok((socket, peer)) = connection {
                    let Ok(permit) = slots.clone().try_acquire_owned() else { continue };
                    let app = remote_app.clone();
                    tokio::spawn(async move { let _permit = permit; handle(socket, app, peer).await; });
                }
            }
        });
    }
    tokio::spawn(async move {
        let addr = SocketAddr::from(([0, 0, 0, 0], settings.port));
        let listener = match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => l,
            Err(e) => {
                warn!("[SATELLITE] Cannot listen on port {}: {e}", settings.port);
                return;
            }
        };
        let tls = if settings.https {
            match acceptor() {
                Ok(a) => Some(a),
                Err(e) => {
                    warn!("[SATELLITE] TLS unavailable ({e}); local listener stopped to protect the pairing key.");
                    return;
                }
            }
        } else {
            None
        };
        info!("[SATELLITE] Mobile satellite at {}", pairing_url(app.config()).replace(&settings.token, "<key>"));
        let slots = Arc::new(tokio::sync::Semaphore::new(16));
        loop {
            let connection = tokio::select! { _ = shutdown.cancelled() => break, result = listener.accept() => result };
            let (socket, peer) = match connection {
                Ok(s) => s,
                Err(e) => {
                    debug!("[SATELLITE] accept: {e}");
                    continue;
                }
            };
            let Ok(permit) = slots.clone().try_acquire_owned() else { continue };
            let _ = socket.set_nodelay(true);
            let (app, tls) = (app.clone(), tls.clone());
            tokio::spawn(async move {
                let _permit = permit;
                match tls {
                    Some(acceptor) => match tokio::time::timeout(Duration::from_secs(10), acceptor.accept(socket)).await {
                        Ok(Ok(stream)) => handle(stream, app, peer).await,
                        Ok(Err(e)) => debug!("[SATELLITE] TLS handshake with {peer} failed: {e}"),
                        Err(_) => debug!("[SATELLITE] TLS handshake with {peer} timed out"),
                    },
                    None => handle(socket, app, peer).await,
                }
            });
        }
    });
}

pub fn is_tool(name: &str) -> bool {
    name == "show_mobile_satellite_instructions"
}

pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "show_mobile_satellite_instructions",
        "description": "How Sir uses JAI on his phone or iPad (\"how can I use you on my phone\", \"connect my phone\", \"show me the mobile link\"): explains the steps and opens the pairing window with the QR code and the link on the PC. topic \"permanent_address\": what Sir has to do so your web page has one address that never changes (\"use you as a website with a static domain\", \"a permanent link\"): the setup steps for his own domain, on screen and in short aloud.",
        "parameters": {"type": "object", "properties": {"topic": {"type": "string", "enum": ["phone", "permanent_address"]}}, "required": []}
    }})
}

/// What Sir has to do so the phone page has one address that never changes, from where his
/// settings stand: (the guide as Markdown for a block on the HUD, what JAI says). `address`:
/// the permanent address when one is configured; `online`: the tunnel serves it right now.
pub fn permanent_address_guide(settings: &crate::config::SatelliteSettings, token_stored: bool, address: Option<&str>, online: bool) -> (String, String) {
    let port = if settings.tunnel_origin_port == 0 { 8444 } else { settings.tunnel_origin_port };
    let provider = settings.tunnel_provider.to_ascii_lowercase();
    let hub = "the Integrations hub (say \"open the integrations\"), tab **Quick Connections**, section **Worldwide address**";
    // Already there: say so, and how it is used.
    if let Some(address) = address {
        let state = if online { "It is online right now." } else { "The tunnel is not online at this moment; it starts with me and I restart it when you ask for the phone link." };
        return (
            format!(
                "## Your permanent address\n\n**{address}**\n\nIt is set up and stays the same after every restart. {state}\n\n- Open it in any browser, on the phone or on another PC: it is my web page.\n- The first visit needs the pairing key: say **\"how can I use you on my phone\"** and scan the QR code once; the browser remembers it.\n- On the phone: Share, then **Add to Home Screen**. The icon keeps working, because the address never changes.\n- This PC and I must be running; the page is served from here.\n\nTo change it: {hub}."
            ),
            format!("It is already set up, Sir. My page lives at {} and that address never changes. The details are on your screen.", address.trim_start_matches("https://")),
        );
    }
    let missing = if token_stored && matches!(provider.as_str(), "auto" | "cloudflare" | "cloudflared") {
        "\n> A tunnel token is already stored, but the **Permanent address** field is empty: step 5 is all that is missing.\n"
    } else {
        ""
    };
    let guide = format!(
        "## One address that never changes\n\nRight now my page uses a quick tunnel: its address is new after every restart. For a static one you need a domain of your own. Three ways; the first is the usual one.\n{missing}\n### A. Your own domain through Cloudflare (free apart from the domain)\n\n1. Have a domain and add it to a free Cloudflare account (Cloudflare then runs its DNS). No domain yet: buy one at any registrar, or at Cloudflare itself.\n2. In the Cloudflare dashboard open **Zero Trust**, then **Networks**, then **Tunnels**, and create a tunnel of the type **Cloudflared**. Name it `jai`.\n3. Cloudflare shows an install command with a long token that starts with `eyJ`. Copy it (the whole command is fine). You do not need to install anything: I bring my own cloudflared.\n4. In that tunnel, tab **Public Hostname**, add a hostname: subdomain `jai`, your domain, service type **HTTP**, URL `127.0.0.1:{port}`.\n5. Here: {hub}. Set **Tunnel** to Cloudflare, type `https://jai.your-domain` into **Permanent address**, paste the token into **Tunnel token**, and save.\n6. Say **\"how can I use you on my phone\"**: the QR code now carries the permanent address. Scan it once and add the page to the home screen; it stays valid for good.\n\n### B. No domain of your own: Tailscale Funnel (free)\n\nInstall Tailscale on this PC, switch **Funnel** on for it in the Tailscale admin console, and set **Tunnel** to Tailscale Funnel in the same section. The address is `https://<this-pc>.<your-tailnet>.ts.net` and never changes.\n\n### C. Your own server\n\nSet **Tunnel** to Own server (SSH), give the SSH target, and put an HTTPS reverse proxy on the server in front of the forwarded port. The permanent address is the proxy's.\n\nWhatever you choose: the page is served from this PC, so it and I must be running. The token is sealed in my vault, never written to the settings file."
    );
    let spoken = if missing.is_empty() {
        format!("You need a domain of your own, Sir. Add it to a free Cloudflare account, create a tunnel there, point a hostname such as jai dot your domain at port {port} on this machine, then paste the tunnel's token and that address into my integrations hub. All six steps are on your screen, with two alternatives that need no domain.")
    } else {
        "A tunnel token is already stored, Sir. Only the permanent address is missing: enter it in my integrations hub and save. The steps are on your screen.".to_string()
    };
    (guide, spoken)
}

/// `show_mobile_satellite_instructions` with topic `permanent_address`: the guide as a block on
/// the HUD and its short form aloud ("Spoken:").
pub fn permanent_address_tool(tools: &crate::tools::ToolContext) -> Result<String, String> {
    let settings = tools.config.read(|c| c.satellite.clone());
    let token_stored = tools.pim.config().vault().has_secret(crate::worldwide_tunnel::TOKEN_SERVICE, crate::vault::TOKEN);
    let tunnel = crate::worldwide_tunnel::snapshot();
    let address = crate::worldwide_tunnel::configured_address(&settings, token_stored).or_else(|| tunnel.public_url.clone().filter(|_| tunnel.permanent));
    let (guide, spoken) = permanent_address_guide(&settings, token_stored, address.as_deref(), tunnel.permanent && tunnel.state == "online");
    if crate::gui::windows_supported() {
        let _ = tools.widgets.spawn_window(&json!({ "id": "permanent_address", "title": "Permanent address // setup", "markdown": guide, "replace_content": true, "width": 720.0, "height": 640.0 }));
        crate::gui::refresh();
    }
    Ok(format!("{guide}\nSpoken: {spoken}"))
}

/// How long the phone tool waits for a (re)starting tunnel to come online before answering.
const TUNNEL_WAIT: Duration = Duration::from_secs(8);

/// "How can I use you on my phone (without being on the same Wi-Fi)?": switches worldwide
/// access on if needed, starts or verifies the HTTPS tunnel, opens the pairing window with the
/// worldwide QR code and link, and explains the three steps (with a "Spoken:" line).
pub async fn run_tool(tools: &crate::tools::ToolContext) -> Result<String, String> {
    let s = tools.config.read(|c| c.satellite.clone());
    if !s.enabled {
        return Err("The mobile satellite is switched off; turn it on in Integrations → Quick Connections and restart me.".into());
    }
    if !s.worldwide_enabled {
        tools.config.update(|c| c.satellite.worldwide_enabled = true);
        info!("[SATELLITE] Worldwide access switched on for the phone");
    }
    // Start (or retry at once) and wait a moment for the verified public address.
    crate::worldwide_tunnel::kick();
    if crate::gui::windows_supported() {
        crate::gui::show_pairing();
    }
    tools.stark.pulse(&["brain", "hivemind", "gps"], false);
    let started = std::time::Instant::now();
    let mut worldwide = crate::worldwide_tunnel::pairing_url(&tools.config);
    while worldwide.is_none() && started.elapsed() < TUNNEL_WAIT {
        tokio::time::sleep(Duration::from_millis(250)).await;
        worldwide = crate::worldwide_tunnel::pairing_url(&tools.config);
    }
    let tunnel = crate::worldwide_tunnel::snapshot();
    let ip = lan_ip();
    let local = pairing_url(&tools.config);
    let steps = "1. Scan the Worldwide HTTPS QR code on the PC screen with your phone's camera (or open the link on your phone).\n\
         2. It runs through a secure HTTPS tunnel, so it works from anywhere in the world over 4G/5G mobile data or any Wi-Fi — no same Wi-Fi needed.\n\
         3. In Safari or Chrome tap Share → \"Add to Home Screen\", open it from there and allow Microphone and Location (GPS).";
    // Whether the address stays: a permanent one keeps the home-screen icon working for good.
    let (stays, stays_spoken) = if tunnel.permanent {
        ("This address is permanent: it is the same after every restart, so the home-screen icon keeps working.", "The address is permanent, so the icon on your phone keeps working.")
    } else {
        (
            "This is a quick-tunnel address: it changes whenever the tunnel restarts (ask me again for the new QR code). For one address that never changes, set a permanent address under Integrations → Quick Connections → Mobile satellite.",
            "This address changes when I restart; a permanent one can be set in the integrations hub.",
        )
    };
    match worldwide {
        Some(link) => Ok(format!(
            "J.A.I. on your phone, anywhere in the world:\nWorldwide HTTPS link (online and verified): {link}\n{steps}\n\
             The pairing key is remembered on the phone. {stays}\n\
             At home you can also use the local link ({ip}): {local}\n\
             This PC and JAI must stay on.\n\
             Spoken: The worldwide QR code is on your screen, Sir. Scan it with your phone, or open the link. It runs through a secure tunnel, so it works anywhere over 4G or 5G, no shared Wi-Fi required. {stays_spoken} Then tap Add to Home Screen and allow the microphone and location."
        )),
        None => Ok(format!(
            "J.A.I. on your phone, anywhere in the world:\nThe worldwide HTTPS tunnel is {} — {}. The pairing window shows its QR code the moment it is online.\n{steps}\n\
             Meanwhile, on this Wi-Fi ({ip}): {local}\n\
             Spoken: I'm bringing the secure worldwide tunnel online, Sir; its QR code appears on your screen in a moment. Scan it, and it works anywhere over 4G or 5G, no shared Wi-Fi required. Then tap Add to Home Screen and allow the microphone and location.",
            tunnel.state, tunnel.detail
        )),
    }
}

/// Whether the certificate files exist (for the hub's "install certificate" hint).
pub fn has_certificate() -> bool {
    Path::new(CERT_FILE).is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "What do I have to do so that I can use you as a website with a static domain?": the
    /// steps with his own port and the hub's own labels; what is missing when he is half way;
    /// the address itself once it is set up.
    #[test]
    fn the_permanent_address_guide_starts_from_where_the_settings_stand() {
        let mut settings = crate::config::SatelliteSettings::default();
        let (guide, spoken) = permanent_address_guide(&settings, false, None, false);
        for needle in ["Zero Trust", "Cloudflared", "eyJ", "Public Hostname", "127.0.0.1:8444", "Quick Connections", "Worldwide address", "**Permanent address**", "**Tunnel token**", "Tailscale Funnel", "Own server (SSH)", "how can I use you on my phone"] {
            assert!(guide.contains(needle), "the guide lacks '{needle}'");
        }
        assert!(spoken.starts_with("You need a domain of your own, Sir.") && spoken.contains("port 8444"), "{spoken}");
        assert!(!spoken.contains('*') && !spoken.contains('`') && !spoken.contains("https://"), "nothing a voice cannot say: {spoken}");
        settings.tunnel_origin_port = 9000;
        assert!(permanent_address_guide(&settings, false, None, false).0.contains("127.0.0.1:9000"));
        // Half way: the token is there, the address is not.
        let (guide, spoken) = permanent_address_guide(&settings, true, None, false);
        assert!(guide.contains("step 5 is all that is missing") && spoken.contains("Only the permanent address is missing"));
        // Done: the address, and how it is used.
        let (guide, spoken) = permanent_address_guide(&settings, true, Some("https://jai.example.com"), true);
        assert!(guide.contains("**https://jai.example.com**") && guide.contains("It is online right now.") && guide.contains("Add to Home Screen"));
        assert_eq!(spoken, "It is already set up, Sir. My page lives at jai.example.com and that address never changes. The details are on your screen.");
        assert!(permanent_address_guide(&settings, true, Some("https://jai.example.com"), false).0.contains("not online at this moment"));
    }

    #[tokio::test]
    async fn requests_parse_with_query_and_body() {
        let raw = b"POST /api/chat?k=abc%20d HTTP/1.1\r\nHost: x\r\nContent-Length: 14\r\nX-JAI-Key: k1\r\n\r\n{\"text\":\"hi!\"}x";
        let mut stream: &[u8] = raw;
        let req = read_request(&mut stream, None).await.unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/api/chat");
        assert_eq!(req.param("k"), Some("abc d"));
        assert_eq!(req.header("x-jai-key"), Some("k1"));
        assert_eq!(req.json()["text"], "hi!");
        assert!(same_token("abc", "abc") && !same_token("abc", "abd") && !same_token("ab", "abc"));
        let (side, modules) = qr_matrix("https://192.168.1.20:8443/satellite?k=0123456789abcdef01234567").unwrap();
        assert_eq!(modules.len(), side * side);
        assert!(PAGE.contains("__JAI_KEY__") && PAGE.contains("watchPosition"));
    }

    #[tokio::test]
    async fn unauthenticated_upload_is_rejected_before_body_read() {
        let raw = b"POST /api/airdrop HTTP/1.1\r\nHost: x\r\nContent-Length: 1000000\r\n\r\n";
        let mut stream: &[u8] = raw;
        let error = read_request(&mut stream, Some("secret")).await.err().unwrap();
        assert_eq!(error, "pairing key required");
        let mut conflicting: &[u8] = b"POST /api/chat HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nx";
        assert!(read_request(&mut conflicting, None).await.is_err());
    }
}

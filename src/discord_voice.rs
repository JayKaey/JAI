//! JAI's voice in a Discord call as his own account (the bot, `discord_bot`): Discord's voice
//! gateway (v8), RTP over UDP sealed with the transport encryption the server offers
//! (AES-256-GCM, else XChaCha20-Poly1305, both "rtpsize"), Opus at 48 kHz, and DAVE, the
//! end-to-end encryption every voice call has required since 1 March 2026, through `davey`
//! (OpenMLS).
//!
//! * Speaking: the clauses the TTS renders while JAI may talk in the call arrive here as
//!   48 kHz mono (`Audio::Samples`) and go out as 20 ms Opus frames: DAVE-encrypted, sealed,
//!   paced by the clock. A pause ends with five silence frames and "not speaking".
//! * Listening: every participant's packets are opened (transport, then DAVE), decoded and cut
//!   into sentences by the call VAD (`discord_bridge::CallVad`, 16 kHz) for Whisper. While JAI
//!   talks, what comes back is dropped (a friend's open microphone would echo him).
//! * Nothing here is written to disk.

use crate::crash_guard::lock;
use crate::discord_bridge::{AntiLoopbackGate, CallVad};
use crate::dsp::StreamResampler;
use crate::ws_client::{self, Message, Writer};
use aes_gcm::aead::{AeadInPlace, KeyInit};
use audiopus::coder::{Decoder, Encoder};
use audiopus::packet::Packet;
use audiopus::{Application, Bitrate, Channels, MutSignals, SampleRate};
use davey::{DaveSession, MediaType, ProposalsOperationType};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::num::NonZeroU16;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot};
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// 20 ms at 48 kHz: one Opus frame.
pub const FRAME: usize = 960;
/// Opus's RTP payload type on Discord.
const OPUS_PT: u8 = 0x78;
const SILENCE: [u8; 3] = davey::OPUS_SILENCE_PACKET;
/// The transport encryptions JAI speaks, preferred first (Discord's own order).
pub const MODES: [&str; 2] = ["aead_aes256_gcm_rtpsize", "aead_xchacha20_poly1305_rtpsize"];
const TAG: usize = 16;
const NONCE: usize = 4;
/// Quiet ticks (20 ms) between clauses before a stretch of speech is closed.
const PAUSE_TICKS: u32 = 15;

/// The voice gateway's opcodes (v8, with DAVE's).
mod op {
    pub const IDENTIFY: u64 = 0;
    pub const SELECT_PROTOCOL: u64 = 1;
    pub const READY: u64 = 2;
    pub const HEARTBEAT: u64 = 3;
    pub const SESSION_DESCRIPTION: u64 = 4;
    pub const SPEAKING: u64 = 5;
    pub const HEARTBEAT_ACK: u64 = 6;
    pub const HELLO: u64 = 8;
    pub const CLIENTS_CONNECT: u64 = 11;
    pub const CLIENT_DISCONNECT: u64 = 13;
    pub const DAVE_PREPARE_TRANSITION: u64 = 21;
    pub const DAVE_EXECUTE_TRANSITION: u64 = 22;
    pub const DAVE_TRANSITION_READY: u64 = 23;
    pub const DAVE_PREPARE_EPOCH: u64 = 24;
    pub const DAVE_EXTERNAL_SENDER: u8 = 25;
    pub const DAVE_KEY_PACKAGE: u8 = 26;
    pub const DAVE_PROPOSALS: u8 = 27;
    pub const DAVE_COMMIT_WELCOME: u8 = 28;
    pub const DAVE_ANNOUNCE_COMMIT: u8 = 29;
    pub const DAVE_WELCOME: u8 = 30;
    pub const DAVE_INVALID_COMMIT_WELCOME: u64 = 31;
}

/// Where and as whom to connect: from the main gateway's VOICE_STATE_UPDATE (session id) and
/// VOICE_SERVER_UPDATE (endpoint, token).
#[derive(Debug, Clone)]
pub struct VoiceParams {
    pub endpoint: String,
    pub guild_id: u64,
    pub channel_id: u64,
    pub user_id: u64,
    pub session_id: String,
    pub token: String,
}

#[derive(Debug)]
pub enum VoiceEvent {
    /// Connected: the transport encryption and the DAVE version (0: transport only).
    Ready { mode: String, dave: u16 },
    /// End-to-end encryption is (re)established; the privacy code people can compare.
    Encrypted { privacy_code: String },
    /// One participant finished a sentence (16 kHz mono).
    Utterance { user_id: u64, pcm: Vec<f32>, voiced_ms: u32 },
    Closed { code: Option<u16>, reason: String },
}

/// What the sender gets: his voice (48 kHz mono), or "stop at once" (Sir cut him off).
pub enum Audio {
    Samples(Vec<f32>),
    Flush,
}

/// A live call. Dropping it leaves.
pub struct VoiceHandle {
    audio: mpsc::UnboundedSender<Audio>,
    stop: CancellationToken,
    speaking: Arc<AtomicBool>,
}

impl VoiceHandle {
    /// Where his voice goes in (the TTS tap clones this).
    pub fn audio(&self) -> mpsc::UnboundedSender<Audio> {
        self.audio.clone()
    }

    /// His voice is going out right now.
    pub fn speaking(&self) -> bool {
        self.speaking.load(Ordering::Acquire)
    }

    pub fn is_closed(&self) -> bool {
        self.stop.is_cancelled()
    }
}

impl Drop for VoiceHandle {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

// --------------------------------------------------------------------------- transport ----

/// The transport encryption of every RTP packet ("rtpsize": the header and the extension's
/// 4-byte head are authenticated, the extension body is sealed with the payload; the 32-bit
/// nonce counter rides at the end of the packet).
pub enum Transport {
    Aes(Box<aes_gcm::Aes256Gcm>),
    XChaCha(Box<chacha20poly1305::XChaCha20Poly1305>),
}

impl Transport {
    pub fn new(mode: &str, key: &[u8]) -> Result<Self, String> {
        let bad_key = |_| format!("the voice key is {} bytes, not 32", key.len());
        match mode {
            "aead_aes256_gcm_rtpsize" => aes_gcm::Aes256Gcm::new_from_slice(key).map(|c| Self::Aes(Box::new(c))).map_err(bad_key),
            "aead_xchacha20_poly1305_rtpsize" => chacha20poly1305::XChaCha20Poly1305::new_from_slice(key).map(|c| Self::XChaCha(Box::new(c))).map_err(bad_key),
            other => Err(format!("unknown voice encryption '{other}'")),
        }
    }

    /// header ‖ sealed payload ‖ tag ‖ counter.
    pub fn seal(&self, header: &[u8], payload: &[u8], counter: u32) -> Option<Vec<u8>> {
        let c = counter.to_be_bytes();
        let mut body = payload.to_vec();
        let mut tag = [0u8; TAG];
        match self {
            Self::Aes(cipher) => {
                let mut n = [0u8; 12];
                n[..4].copy_from_slice(&c);
                tag.copy_from_slice(cipher.encrypt_in_place_detached(aes_gcm::Nonce::from_slice(&n), header, &mut body).ok()?.as_slice());
            }
            Self::XChaCha(cipher) => {
                let mut n = [0u8; 24];
                n[..4].copy_from_slice(&c);
                tag.copy_from_slice(cipher.encrypt_in_place_detached(chacha20poly1305::XNonce::from_slice(&n), header, &mut body).ok()?.as_slice());
            }
        }
        let mut out = Vec::with_capacity(header.len() + body.len() + TAG + NONCE);
        out.extend_from_slice(header);
        out.extend_from_slice(&body);
        out.extend_from_slice(&tag);
        out.extend_from_slice(&c);
        Some(out)
    }

    /// The payload of a sealed RTP packet, extension body and padding removed (None: not ours,
    /// damaged or forged).
    pub fn open(&self, packet: &[u8]) -> Option<Vec<u8>> {
        let aad = rtp_header_len(packet)?;
        if packet.len() < aad + TAG + NONCE {
            return None;
        }
        let ext_words = if packet[0] & 0x10 != 0 { u16::from_be_bytes([packet[aad - 2], packet[aad - 1]]) as usize } else { 0 };
        let end = packet.len() - TAG - NONCE;
        let counter = &packet[end + TAG..];
        let tag = &packet[end..end + TAG];
        let mut body = packet[aad..end].to_vec();
        match self {
            Self::Aes(cipher) => {
                let mut n = [0u8; 12];
                n[..4].copy_from_slice(counter);
                cipher.decrypt_in_place_detached(aes_gcm::Nonce::from_slice(&n), &packet[..aad], &mut body, aes_gcm::Tag::from_slice(tag)).ok()?;
            }
            Self::XChaCha(cipher) => {
                let mut n = [0u8; 24];
                n[..4].copy_from_slice(counter);
                cipher.decrypt_in_place_detached(chacha20poly1305::XNonce::from_slice(&n), &packet[..aad], &mut body, chacha20poly1305::Tag::from_slice(tag)).ok()?;
            }
        }
        if ext_words * 4 > body.len() {
            return None;
        }
        let mut payload = body.split_off(ext_words * 4);
        if packet[0] & 0x20 != 0 {
            let pad = *payload.last()? as usize;
            if pad == 0 || pad > payload.len() {
                return None;
            }
            payload.truncate(payload.len() - pad);
        }
        Some(payload)
    }
}

/// The authenticated part of an RTP header: 12 bytes, the CSRCs and the extension's head.
fn rtp_header_len(packet: &[u8]) -> Option<usize> {
    if packet.len() < 12 || packet[0] >> 6 != 2 {
        return None;
    }
    let mut len = 12 + 4 * (packet[0] & 0x0F) as usize;
    if packet[0] & 0x10 != 0 {
        len += 4;
    }
    (packet.len() >= len).then_some(len)
}

pub fn rtp_header(sequence: u16, timestamp: u32, ssrc: u32) -> [u8; 12] {
    let mut h = [0u8; 12];
    h[0] = 0x80;
    h[1] = OPUS_PT;
    h[2..4].copy_from_slice(&sequence.to_be_bytes());
    h[4..8].copy_from_slice(&timestamp.to_be_bytes());
    h[8..12].copy_from_slice(&ssrc.to_be_bytes());
    h
}

/// IP discovery: "who am I from outside?" (type 1, length 70, the SSRC, 66 zero bytes).
pub fn discovery_request(ssrc: u32) -> [u8; 74] {
    let mut p = [0u8; 74];
    p[0..2].copy_from_slice(&1u16.to_be_bytes());
    p[2..4].copy_from_slice(&70u16.to_be_bytes());
    p[4..8].copy_from_slice(&ssrc.to_be_bytes());
    p
}

/// The answer: the address (NUL-terminated) and port the voice server sees.
pub fn parse_discovery(p: &[u8]) -> Option<(String, u16)> {
    if p.len() < 74 || u16::from_be_bytes([p[0], p[1]]) != 2 {
        return None;
    }
    let raw = &p[8..72];
    let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
    let ip = std::str::from_utf8(&raw[..end]).ok()?.trim().to_string();
    (!ip.is_empty()).then(|| (ip, u16::from_be_bytes([p[72], p[73]])))
}

// -------------------------------------------------------------------------------- DAVE ----

/// The call's end-to-end encryption: the MLS session and the protocol version media follows.
struct Dave {
    session: Option<DaveSession>,
    /// The version media is encrypted with now (0: transport encryption only).
    version: u16,
    /// Prepared transitions: id → protocol version.
    pending: HashMap<u16, u16>,
    /// The voice server's external sender package (it may come before the session exists).
    external: Option<Vec<u8>>,
    user_id: u64,
    channel_id: u64,
    privacy_code: String,
}

impl Dave {
    fn new(user_id: u64, channel_id: u64) -> Self {
        Self { session: None, version: 0, pending: HashMap::new(), external: None, user_id, channel_id, privacy_code: String::new() }
    }

    /// The protocol the session speaks (for a recovery).
    fn protocol(&self) -> u16 {
        self.session.as_ref().map(|s| s.protocol_version().get()).unwrap_or(self.version)
    }

    /// (Re)starts the MLS session for `version`; the key package to send (op 26), if any.
    fn reinit(&mut self, version: u16) -> Result<Option<Vec<u8>>, String> {
        let Some(v) = NonZeroU16::new(version) else {
            // Transport encryption only: whatever still arrives encrypted is let through.
            if let Some(s) = self.session.as_mut() {
                let _ = s.reset();
                s.set_passthrough_mode(true, Some(10));
            }
            return Ok(None);
        };
        match self.session.as_mut() {
            Some(s) => s.reinit(v, self.user_id, self.channel_id, None).map_err(|e| format!("DAVE reinit: {e}"))?,
            None => {
                let mut s = DaveSession::new(v, self.user_id, self.channel_id, None).map_err(|e| format!("DAVE: {e}"))?;
                if let Some(external) = &self.external {
                    s.set_external_sender(external).map_err(|e| format!("DAVE external sender: {e}"))?;
                }
                self.session = Some(s);
            }
        }
        let session = self.session.as_mut().ok_or("no DAVE session")?;
        session.create_key_package().map(Some).map_err(|e| format!("DAVE key package: {e}"))
    }

    fn external_sender(&mut self, package: &[u8]) -> Result<(), String> {
        self.external = Some(package.to_vec());
        match self.session.as_mut() {
            Some(s) => s.set_external_sender(package).map_err(|e| e.to_string()),
            None => Ok(()),
        }
    }

    fn prepare(&mut self, transition: u16, version: u16) -> Result<(), String> {
        let s = self.session.as_mut().ok_or("no DAVE session")?;
        s.prepare_transition(transition, version).map_err(|e| e.to_string())?;
        if transition == 0 {
            self.version = version;
        } else {
            self.pending.insert(transition, version);
        }
        Ok(())
    }

    fn execute(&mut self, transition: u16) {
        if let Some(s) = self.session.as_mut() {
            if let Err(e) = s.execute_transition(transition) {
                warn!("[DISCORD] DAVE transition {transition}: {e}");
            }
        }
        if let Some(v) = self.pending.remove(&transition) {
            self.version = v;
        }
    }

    /// The commit (and welcome) to send back for the server's proposals.
    fn proposals(&mut self, payload: &[u8]) -> Result<Option<Vec<u8>>, String> {
        let (kind, rest) = payload.split_first().ok_or("empty proposals")?;
        let kind = if *kind == 1 { ProposalsOperationType::REVOKE } else { ProposalsOperationType::APPEND };
        let s = self.session.as_mut().ok_or("no DAVE session")?;
        let answer = s.process_proposals(kind, rest, None).map_err(|e| e.to_string())?;
        Ok(answer.map(|cw| {
            let mut out = cw.commit;
            if let Some(welcome) = cw.welcome {
                out.extend_from_slice(&welcome);
            }
            out
        }))
    }

    /// An announced commit (29) or a welcome (30). Ok(Some(id)): prepared, report ready;
    /// Ok(None): not for us; Err(id): invalid, ask for a new one.
    fn commit_or_welcome(&mut self, welcome: bool, payload: &[u8]) -> Result<Option<u16>, u16> {
        if payload.len() < 2 {
            return Err(0);
        }
        let id = u16::from_be_bytes([payload[0], payload[1]]);
        let Some(s) = self.session.as_mut() else { return Err(id) };
        let result = if welcome { s.process_welcome(&payload[2..], None).map(|_| true).map_err(|e| e.to_string()) } else { s.process_commit(&payload[2..]).map_err(|e| e.to_string()) };
        match result {
            Ok(true) => {
                let version = s.protocol_version().get();
                self.prepare(id, version).map_err(|e| {
                    warn!("[DISCORD] DAVE transition {id}: {e}");
                    id
                })?;
                Ok(Some(id))
            }
            Ok(false) => Ok(None),
            Err(e) => {
                warn!("[DISCORD] DAVE {} {id} rejected: {e}", if welcome { "welcome" } else { "commit" });
                Err(id)
            }
        }
    }

    /// A new privacy code after a transition.
    fn fresh_code(&mut self) -> Option<String> {
        let code = self.session.as_ref()?.voice_privacy_code()?.to_string();
        (code != self.privacy_code).then(|| {
            self.privacy_code = code.clone();
            code
        })
    }

    /// His frame for the wire (None while the group's keys are not ready: dropped, never sent
    /// in the clear into an encrypted call).
    fn encrypt(&mut self, opus: &[u8]) -> Option<Vec<u8>> {
        if self.version == 0 {
            return Some(opus.to_vec());
        }
        self.session.as_mut()?.encrypt_opus(opus).ok().map(|c| c.into_owned())
    }

    fn decrypt(&mut self, user_id: u64, frame: &[u8]) -> Option<Vec<u8>> {
        if frame == SILENCE {
            return None;
        }
        match self.session.as_mut() {
            Some(s) if self.version > 0 => s.decrypt(user_id, MediaType::AUDIO, frame).ok(),
            _ => Some(frame.to_vec()),
        }
    }
}

// ------------------------------------------------------------------------- media paths ----

/// One participant: Opus → 48 kHz mono → 16 kHz → sentences.
struct Listener {
    decoder: Decoder,
    resampler: StreamResampler,
    vad: CallVad,
    pending: Vec<f32>,
    last: Instant,
    active: bool,
}

impl Listener {
    fn new() -> Option<Self> {
        Some(Self { decoder: Decoder::new(SampleRate::Hz48000, Channels::Stereo).ok()?, resampler: StreamResampler::new(48_000, 16_000), vad: CallVad::new(16_000), pending: Vec::new(), last: Instant::now(), active: false })
    }

    fn push(&mut self, opus: &[u8]) -> Option<(Vec<f32>, u32)> {
        self.last = Instant::now();
        self.active = true;
        let mut pcm = vec![0f32; 5760 * 2];
        let packet = Packet::try_from(opus).ok()?;
        let signals = MutSignals::try_from(&mut pcm[..]).ok()?;
        let n = self.decoder.decode_float(Some(packet), signals, false).ok()?;
        let mono: Vec<f32> = pcm[..n * 2].chunks_exact(2).map(|c| 0.5 * (c[0] + c[1])).collect();
        self.feed(&mono)
    }

    fn feed(&mut self, mono48: &[f32]) -> Option<(Vec<f32>, u32)> {
        self.resampler.process(mono48, &mut self.pending);
        let frame = self.vad.frame_len();
        let mut done = None;
        while self.pending.len() >= frame {
            let chunk: Vec<f32> = self.pending.drain(..frame).collect();
            if let Some(utterance) = self.vad.push_frame(&chunk) {
                done = Some(utterance);
            }
        }
        done
    }

    /// Discord sends nothing between someone's sentences: their silence closes the sentence.
    fn flush(&mut self) -> Option<(Vec<f32>, u32)> {
        if !self.active || self.last.elapsed() < Duration::from_millis(300) {
            return None;
        }
        self.active = false;
        let quiet = vec![0f32; FRAME];
        (0..45).find_map(|_| self.feed(&quiet))
    }
}

/// His side of the RTP stream.
struct Sender {
    encoder: Encoder,
    sequence: u16,
    timestamp: u32,
    counter: u32,
    ssrc: u32,
}

impl Sender {
    fn new(ssrc: u32) -> Result<Self, String> {
        let mut encoder = Encoder::new(SampleRate::Hz48000, Channels::Stereo, Application::Audio).map_err(|e| format!("Opus: {e}"))?;
        encoder.set_bitrate(Bitrate::BitsPerSecond(64_000)).map_err(|e| format!("Opus: {e}"))?;
        let seed = rand_u32();
        Ok(Self { encoder, sequence: seed as u16, timestamp: seed.rotate_left(13), counter: 0, ssrc })
    }

    /// 960 mono samples → one Opus frame (stereo, both sides the same).
    fn encode(&self, mono: &[f32]) -> Option<Vec<u8>> {
        let stereo: Vec<f32> = mono.iter().flat_map(|s| [*s, *s]).collect();
        let mut out = vec![0u8; 4000];
        let n = self.encoder.encode_float(&stereo, &mut out).ok()?;
        out.truncate(n);
        Some(out)
    }

    fn packet(&mut self, transport: &Transport, frame: &[u8]) -> Option<Vec<u8>> {
        let sealed = transport.seal(&rtp_header(self.sequence, self.timestamp, self.ssrc), frame, self.counter)?;
        self.sequence = self.sequence.wrapping_add(1);
        self.counter = self.counter.wrapping_add(1);
        Some(sealed)
    }
}

fn rand_u32() -> u32 {
    use ring::rand::SecureRandom;
    let mut b = [0u8; 4];
    let _ = ring::rand::SystemRandom::new().fill(&mut b);
    u32::from_le_bytes(b)
}

/// Windows wakes timers on a 15.6 ms tick unless someone asks for 1 ms; 20 ms Opus frames need
/// it while JAI is in a call.
struct FineTimer;

impl FineTimer {
    fn start() -> Self {
        #[cfg(windows)]
        // SAFETY: a plain request, undone in Drop.
        unsafe {
            windows::Win32::Media::timeBeginPeriod(1);
        }
        Self
    }
}

impl Drop for FineTimer {
    fn drop(&mut self) {
        #[cfg(windows)]
        // SAFETY: matches the request in `start`.
        unsafe {
            windows::Win32::Media::timeEndPeriod(1);
        }
    }
}

// ----------------------------------------------------------------------------- session ----

struct Shared {
    dave: Mutex<Dave>,
    /// SSRC → user id (from the voice gateway's speaking events).
    users: Mutex<HashMap<u32, u64>>,
    speaking: Arc<AtomicBool>,
    seq: AtomicI64,
}

/// Connects to the call; returns once audio can flow (or why it cannot).
pub async fn connect(params: VoiceParams, events: mpsc::UnboundedSender<VoiceEvent>) -> Result<VoiceHandle, String> {
    let stop = CancellationToken::new();
    let (ready_tx, ready_rx) = oneshot::channel::<Result<(), String>>();
    let (audio_tx, audio_rx) = mpsc::unbounded_channel();
    let speaking = Arc::new(AtomicBool::new(false));
    let shared = Arc::new(Shared { dave: Mutex::new(Dave::new(params.user_id, params.channel_id)), users: Mutex::new(HashMap::new()), speaking: speaking.clone(), seq: AtomicI64::new(-1) });
    let task_stop = stop.clone();
    tokio::spawn(async move {
        let mut ready = Some(ready_tx);
        let end = session(params, &events, shared, audio_rx, &task_stop, &mut ready).await;
        task_stop.cancel();
        let (code, reason) = end.unwrap_or_else(|e| (None, e));
        if let Some(tx) = ready.take() {
            let _ = tx.send(Err(describe_close(code, &reason)));
        }
        info!("[DISCORD] Voice connection closed ({}{reason})", code.map(|c| format!("{c}: ")).unwrap_or_default());
        let _ = events.send(VoiceEvent::Closed { code, reason });
    });
    match tokio::time::timeout(Duration::from_secs(20), ready_rx).await {
        Ok(Ok(Ok(()))) => Ok(VoiceHandle { audio: audio_tx, stop, speaking }),
        Ok(Ok(Err(e))) => {
            stop.cancel();
            Err(e)
        }
        Ok(Err(_)) => {
            stop.cancel();
            Err("the voice connection ended while connecting".into())
        }
        Err(_) => {
            stop.cancel();
            Err("Discord's voice server did not finish connecting within 20 s".into())
        }
    }
}

/// Sir-facing words for a voice close code.
pub fn describe_close(code: Option<u16>, reason: &str) -> String {
    match code {
        Some(4014) => "I was disconnected from the call (moved, kicked, or the channel is gone).".into(),
        Some(4006) | Some(4009) => "Discord ended my voice session; ask me to join again.".into(),
        Some(4016) => "Discord's voice server and I share no encryption mode.".into(),
        Some(4017) => "the call requires end-to-end encryption I could not set up.".into(),
        Some(c) => format!("the voice connection closed ({c}: {reason})"),
        None => reason.to_string(),
    }
}

fn voice_url(endpoint: &str) -> String {
    if endpoint.starts_with("ws://") || endpoint.starts_with("wss://") {
        return endpoint.to_string();
    }
    let host = endpoint.split(':').next().unwrap_or(endpoint);
    format!("wss://{host}/?v=8")
}

fn snowflake(v: &Value) -> Option<u64> {
    match v {
        Value::String(s) => s.parse().ok(),
        Value::Number(n) => n.as_u64(),
        _ => None,
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

async fn send(writer: &Writer, op: u64, d: Value) -> Result<(), String> {
    writer.send_text(&json!({ "op": op, "d": d }).to_string()).await
}

async fn send_binary(writer: &Writer, op: u8, payload: &[u8]) -> Result<(), String> {
    let mut bytes = Vec::with_capacity(payload.len() + 1);
    bytes.push(op);
    bytes.extend_from_slice(payload);
    writer.send_binary(&bytes).await
}

async fn tick(interval: &mut Option<tokio::time::Interval>) {
    match interval {
        Some(i) => {
            i.tick().await;
        }
        None => std::future::pending::<()>().await,
    }
}

async fn discover(socket: &UdpSocket, ssrc: u32) -> Result<(String, u16), String> {
    let mut buf = [0u8; 128];
    for _ in 0..3 {
        socket.send(&discovery_request(ssrc)).await.map_err(|e| format!("UDP: {e}"))?;
        if let Ok(Ok(n)) = tokio::time::timeout(Duration::from_secs(2), socket.recv(&mut buf)).await {
            if let Some(found) = parse_discovery(&buf[..n]) {
                return Ok(found);
            }
        }
    }
    Err("Discord's voice server did not answer over UDP (a firewall?)".into())
}

async fn session(
    p: VoiceParams,
    events: &mpsc::UnboundedSender<VoiceEvent>,
    shared: Arc<Shared>,
    audio_rx: mpsc::UnboundedReceiver<Audio>,
    stop: &CancellationToken,
    ready: &mut Option<oneshot::Sender<Result<(), String>>>,
) -> Result<(Option<u16>, String), String> {
    let (mut reader, writer) = ws_client::connect(&voice_url(&p.endpoint)).await?;
    send(&writer, op::IDENTIFY, json!({
        "server_id": p.guild_id.to_string(),
        "user_id": p.user_id.to_string(),
        "session_id": p.session_id,
        "token": p.token,
        "max_dave_protocol_version": davey::DAVE_PROTOCOL_VERSION,
    }))
    .await?;
    let _timer = FineTimer::start();
    let mut heartbeat: Option<tokio::time::Interval> = None;
    let mut acked = true;
    let mut audio_rx = Some(audio_rx);
    let mut ssrc = 0u32;
    let mut udp: Option<Arc<UdpSocket>> = None;
    loop {
        let message = tokio::select! {
            _ = stop.cancelled() => {
                let _ = writer.close(1000, "leaving").await;
                return Ok((None, "left the call".into()));
            }
            _ = tick(&mut heartbeat) => {
                if !acked {
                    return Ok((None, "the voice server stopped answering".into()));
                }
                acked = false;
                send(&writer, op::HEARTBEAT, json!({ "t": now_ms(), "seq_ack": shared.seq.load(Ordering::Acquire) })).await?;
                continue;
            }
            m = reader.recv() => m?,
        };
        match message {
            Message::Close(code, reason) => return Ok((code, reason)),
            Message::Binary(bytes) => {
                if bytes.len() < 3 {
                    continue;
                }
                shared.seq.store(u16::from_be_bytes([bytes[0], bytes[1]]) as i64, Ordering::Release);
                on_dave_binary(&writer, &shared, events, bytes[2], &bytes[3..]).await?;
            }
            Message::Text(text) => {
                let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
                if let Some(s) = v["seq"].as_i64() {
                    shared.seq.store(s, Ordering::Release);
                }
                let d = &v["d"];
                match v["op"].as_u64().unwrap_or(u64::MAX) {
                    op::HELLO => {
                        let ms = d["heartbeat_interval"].as_f64().unwrap_or(13_750.0).max(1_000.0) as u64;
                        let mut interval = tokio::time::interval(Duration::from_millis(ms));
                        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
                        heartbeat = Some(interval);
                    }
                    op::HEARTBEAT_ACK => acked = true,
                    op::READY => {
                        ssrc = d["ssrc"].as_u64().unwrap_or(0) as u32;
                        let ip = d["ip"].as_str().unwrap_or("").to_string();
                        let port = d["port"].as_u64().unwrap_or(0) as u16;
                        let offered: Vec<&str> = d["modes"].as_array().map(|m| m.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
                        let mode = MODES.iter().find(|m| offered.contains(m)).ok_or_else(|| format!("no voice encryption I know ({})", offered.join(", ")))?;
                        let socket = UdpSocket::bind("0.0.0.0:0").await.map_err(|e| format!("UDP: {e}"))?;
                        socket.connect((ip.as_str(), port)).await.map_err(|e| format!("UDP to {ip}:{port}: {e}"))?;
                        let (address, port) = discover(&socket, ssrc).await?;
                        udp = Some(Arc::new(socket));
                        send(&writer, op::SELECT_PROTOCOL, json!({ "protocol": "udp", "data": { "address": address, "port": port, "mode": mode } })).await?;
                    }
                    op::SESSION_DESCRIPTION => {
                        let mode = d["mode"].as_str().unwrap_or("");
                        let key: Vec<u8> = d["secret_key"].as_array().map(|k| k.iter().filter_map(|b| b.as_u64().map(|b| b as u8)).collect()).unwrap_or_default();
                        let transport = Arc::new(Transport::new(mode, &key)?);
                        let version = d["dave_protocol_version"].as_u64().unwrap_or(0) as u16;
                        let package = {
                            let mut dave = lock(&shared.dave);
                            dave.version = version;
                            dave.reinit(version)
                        }?;
                        if let Some(kp) = package {
                            send_binary(&writer, op::DAVE_KEY_PACKAGE, &kp).await?;
                        }
                        let socket = udp.clone().ok_or("the voice server described a session before it was ready")?;
                        if let Some(rx) = audio_rx.take() {
                            tokio::spawn(send_audio(socket.clone(), transport.clone(), shared.clone(), rx, writer.clone(), ssrc, stop.clone()));
                            tokio::spawn(receive_audio(socket, transport, shared.clone(), events.clone(), stop.clone()));
                        }
                        info!("[DISCORD] In the call: {mode}, DAVE v{version}");
                        let _ = events.send(VoiceEvent::Ready { mode: mode.to_string(), dave: version });
                        if let Some(tx) = ready.take() {
                            let _ = tx.send(Ok(()));
                        }
                    }
                    op::SPEAKING => {
                        if let (Some(user), Some(their_ssrc)) = (snowflake(&d["user_id"]), d["ssrc"].as_u64()) {
                            lock(&shared.users).insert(their_ssrc as u32, user);
                        }
                    }
                    // Who is in the call comes from the main gateway's voice states.
                    op::CLIENTS_CONNECT => {}
                    op::CLIENT_DISCONNECT => {
                        if let Some(user) = snowflake(&d["user_id"]) {
                            lock(&shared.users).retain(|_, u| *u != user);
                        }
                    }
                    op::DAVE_PREPARE_TRANSITION => {
                        let transition = d["transition_id"].as_u64().unwrap_or(0) as u16;
                        let version = d["protocol_version"].as_u64().unwrap_or(0) as u16;
                        let prepared = lock(&shared.dave).prepare(transition, version);
                        match prepared {
                            Ok(()) if transition != 0 => send(&writer, op::DAVE_TRANSITION_READY, json!({ "transition_id": transition })).await?,
                            Ok(()) => {}
                            Err(e) => warn!("[DISCORD] DAVE prepare {transition}: {e}"),
                        }
                    }
                    op::DAVE_EXECUTE_TRANSITION => {
                        let transition = d["transition_id"].as_u64().unwrap_or(0) as u16;
                        let code = {
                            let mut dave = lock(&shared.dave);
                            dave.execute(transition);
                            dave.fresh_code()
                        };
                        if let Some(privacy_code) = code {
                            let _ = events.send(VoiceEvent::Encrypted { privacy_code });
                        }
                    }
                    op::DAVE_PREPARE_EPOCH => {
                        let version = d["protocol_version"].as_u64().unwrap_or(0) as u16;
                        if d["epoch"].as_u64() == Some(1) {
                            let package = lock(&shared.dave).reinit(version)?;
                            if let Some(kp) = package {
                                send_binary(&writer, op::DAVE_KEY_PACKAGE, &kp).await?;
                            }
                        }
                    }
                    other => debug!("[DISCORD] voice op {other}"),
                }
            }
        }
    }
}

async fn on_dave_binary(writer: &Writer, shared: &Shared, events: &mpsc::UnboundedSender<VoiceEvent>, opcode: u8, payload: &[u8]) -> Result<(), String> {
    match opcode {
        op::DAVE_EXTERNAL_SENDER => {
            let set = lock(&shared.dave).external_sender(payload);
            if let Err(e) = set {
                warn!("[DISCORD] DAVE external sender: {e}");
            }
        }
        op::DAVE_PROPOSALS => {
            let commit = lock(&shared.dave).proposals(payload);
            match commit {
                Ok(Some(c)) => send_binary(writer, op::DAVE_COMMIT_WELCOME, &c).await?,
                Ok(None) => {}
                Err(e) => warn!("[DISCORD] DAVE proposals: {e}"),
            }
        }
        op::DAVE_ANNOUNCE_COMMIT | op::DAVE_WELCOME => {
            let result = lock(&shared.dave).commit_or_welcome(opcode == op::DAVE_WELCOME, payload);
            match result {
                Ok(Some(transition)) if transition != 0 => send(writer, op::DAVE_TRANSITION_READY, json!({ "transition_id": transition })).await?,
                Ok(_) => {}
                Err(transition) => {
                    send(writer, op::DAVE_INVALID_COMMIT_WELCOME, json!({ "transition_id": transition })).await?;
                    let package = {
                        let mut dave = lock(&shared.dave);
                        let version = dave.protocol();
                        dave.reinit(version)
                    };
                    if let Ok(Some(kp)) = package {
                        send_binary(writer, op::DAVE_KEY_PACKAGE, &kp).await?;
                    }
                }
            }
            let code = lock(&shared.dave).fresh_code();
            if let Some(privacy_code) = code {
                let _ = events.send(VoiceEvent::Encrypted { privacy_code });
            }
        }
        other => debug!("[DISCORD] voice binary op {other}"),
    }
    Ok(())
}

/// His voice: Opus frames on the clock while there is something to say.
async fn send_audio(udp: Arc<UdpSocket>, transport: Arc<Transport>, shared: Arc<Shared>, mut audio: mpsc::UnboundedReceiver<Audio>, writer: Writer, ssrc: u32, stop: CancellationToken) {
    let mut sender = match Sender::new(ssrc) {
        Ok(s) => s,
        Err(e) => {
            warn!("[DISCORD] No voice: {e}");
            return;
        }
    };
    let mut queue: VecDeque<f32> = VecDeque::new();
    let mut speaking = false;
    let mut idle_ticks = 0u32;
    let mut silence_left = 0u32;
    let mut next = tokio::time::Instant::now();
    loop {
        if !speaking {
            let first = tokio::select! {
                _ = stop.cancelled() => return,
                a = audio.recv() => a,
            };
            match first {
                None => return,
                Some(Audio::Flush) => continue,
                Some(Audio::Samples(s)) => queue.extend(s),
            }
            if send(&writer, op::SPEAKING, json!({ "speaking": 1, "delay": 0, "ssrc": ssrc })).await.is_err() {
                return;
            }
            speaking = true;
            shared.speaking.store(true, Ordering::Release);
            idle_ticks = 0;
            silence_left = 0;
            next = tokio::time::Instant::now();
        }
        while let Ok(a) = audio.try_recv() {
            match a {
                Audio::Samples(s) => {
                    queue.extend(s);
                    silence_left = 0;
                }
                Audio::Flush => queue.clear(),
            }
        }
        let frame = if !queue.is_empty() {
            idle_ticks = 0;
            let mut pcm: Vec<f32> = queue.drain(..queue.len().min(FRAME)).collect();
            pcm.resize(FRAME, 0.0);
            sender.encode(&pcm).and_then(|opus| lock(&shared.dave).encrypt(&opus))
        } else {
            idle_ticks += 1;
            if idle_ticks == PAUSE_TICKS {
                silence_left = 5;
            }
            if silence_left > 0 {
                silence_left -= 1;
                Some(SILENCE.to_vec())
            } else {
                None
            }
        };
        if let Some(packet) = frame.and_then(|f| sender.packet(&transport, &f)) {
            let _ = udp.send(&packet).await;
        }
        sender.timestamp = sender.timestamp.wrapping_add(FRAME as u32);
        if queue.is_empty() && idle_ticks > PAUSE_TICKS && silence_left == 0 {
            speaking = false;
            shared.speaking.store(false, Ordering::Release);
            let _ = send(&writer, op::SPEAKING, json!({ "speaking": 0, "delay": 0, "ssrc": ssrc })).await;
            continue;
        }
        next += Duration::from_millis(20);
        let now = tokio::time::Instant::now();
        if next + Duration::from_millis(200) < now {
            // Far behind (the PC stalled): carry on from now instead of bursting.
            next = now;
        }
        tokio::select! {
            _ = stop.cancelled() => return,
            _ = tokio::time::sleep_until(next) => {}
        }
    }
}

/// Everyone else: packets to sentences, plus the 5 s keep-alive that holds the NAT open.
async fn receive_audio(udp: Arc<UdpSocket>, transport: Arc<Transport>, shared: Arc<Shared>, events: mpsc::UnboundedSender<VoiceEvent>, stop: CancellationToken) {
    let mut buf = vec![0u8; 4096];
    let mut listeners: HashMap<u32, Listener> = HashMap::new();
    let mut gate = AntiLoopbackGate::default();
    let mut keepalive = tokio::time::interval(Duration::from_secs(5));
    let mut flush = tokio::time::interval(Duration::from_millis(100));
    let mut alive = 0u32;
    loop {
        tokio::select! {
            _ = stop.cancelled() => return,
            _ = keepalive.tick() => {
                let mut packet = [0u8; 8];
                packet[..4].copy_from_slice(&alive.to_le_bytes());
                alive = alive.wrapping_add(1);
                let _ = udp.send(&packet).await;
            }
            _ = flush.tick() => {
                for (ssrc, listener) in listeners.iter_mut() {
                    if let Some((pcm, voiced_ms)) = listener.flush() {
                        let user = lock(&shared.users).get(ssrc).copied();
                        if let Some(user_id) = user {
                            let _ = events.send(VoiceEvent::Utterance { user_id, pcm, voiced_ms });
                        }
                    }
                }
            }
            received = udp.recv(&mut buf) => {
                let Ok(n) = received else { continue };
                let packet = &buf[..n];
                // RTP version 2 carrying Opus (RTCP's packet types never match).
                if n < 12 || packet[0] >> 6 != 2 || packet[1] & 0x7F != OPUS_PT {
                    continue;
                }
                let ssrc = u32::from_be_bytes([packet[8], packet[9], packet[10], packet[11]]);
                let Some(user_id) = lock(&shared.users).get(&ssrc).copied() else { continue };
                let Some(payload) = transport.open(packet) else { continue };
                let Some(opus) = lock(&shared.dave).decrypt(user_id, &payload) else { continue };
                if !gate.admit(shared.speaking.load(Ordering::Acquire), Instant::now()) {
                    continue;
                }
                let listener = match listeners.entry(ssrc) {
                    std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                    std::collections::hash_map::Entry::Vacant(v) => match Listener::new() {
                        Some(l) => v.insert(l),
                        None => continue,
                    },
                };
                if let Some((pcm, voiced_ms)) = listener.push(&opus) {
                    let _ = events.send(VoiceEvent::Utterance { user_id, pcm, voiced_ms });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(seconds: f32, amplitude: f32) -> Vec<f32> {
        (0..(48_000.0 * seconds) as usize).map(|i| amplitude * (i as f32 * 2.0 * std::f32::consts::PI * 300.0 / 48_000.0).sin()).collect()
    }

    #[test]
    fn transport_seals_and_opens_both_modes_with_an_extension() {
        for mode in MODES {
            let t = Transport::new(mode, &[7u8; 32]).unwrap();
            let sealed = t.seal(&rtp_header(5, 960, 42), b"opus frame", 9).unwrap();
            assert_eq!(&sealed[sealed.len() - 4..], &9u32.to_be_bytes());
            assert_eq!(t.open(&sealed).unwrap(), b"opus frame");
            let mut forged = sealed.clone();
            forged[13] ^= 1;
            assert!(t.open(&forged).is_none(), "{mode}: a changed byte must not open");
            // With a one-word header extension (authenticated head, sealed body), as Discord's
            // clients send it.
            let mut header = rtp_header(6, 1920, 42).to_vec();
            header[0] |= 0x10;
            header.extend_from_slice(&[0xBE, 0xDE, 0, 1]);
            let sealed = t.seal(&header, &[1, 2, 3, 4, b'h', b'i'], 10).unwrap();
            assert_eq!(t.open(&sealed).unwrap(), b"hi");
        }
        assert!(Transport::new("xsalsa20_poly1305", &[0; 32]).is_err());
        assert!(Transport::new(MODES[0], &[0; 16]).is_err());
    }

    #[test]
    fn address_discovery_parses_the_answer() {
        let request = discovery_request(0x0102_0304);
        assert_eq!(&request[..8], &[0, 1, 0, 70, 1, 2, 3, 4]);
        let mut answer = [0u8; 74];
        answer[1] = 2;
        answer[8..19].copy_from_slice(b"203.0.113.9");
        answer[72..74].copy_from_slice(&50_004u16.to_be_bytes());
        assert_eq!(parse_discovery(&answer), Some(("203.0.113.9".into(), 50_004)));
        assert_eq!(parse_discovery(&request), None);
    }

    #[test]
    fn opus_round_trip_is_fast_and_keeps_the_sound() {
        let sender = Sender::new(1).unwrap();
        let mut listener = Listener::new().unwrap();
        let audio = tone(1.0, 0.3);
        let started = Instant::now();
        let frames: Vec<Vec<u8>> = audio.chunks(FRAME).map(|c| sender.encode(c).unwrap()).collect();
        let encode_ms = started.elapsed().as_millis();
        assert_eq!(frames.len(), 50);
        assert!(encode_ms < 400, "one second of voice took {encode_ms} ms to encode");
        // Decoded in order, as a listener hears the stream (each frame's energy is predicted from
        // the one before, so a decoder that joins mid-stream needs a moment).
        let mut decoder = Decoder::new(SampleRate::Hz48000, Channels::Stereo).unwrap();
        let mut pcm = vec![0f32; 5760 * 2];
        let mut levels = Vec::new();
        for f in &frames {
            let n = decoder.decode_float(Some(Packet::try_from(&f[..]).unwrap()), MutSignals::try_from(&mut pcm[..]).unwrap(), false).unwrap();
            assert_eq!(n, FRAME);
            levels.push((pcm[..n * 2].iter().map(|s| s * s).sum::<f32>() / (n * 2) as f32).sqrt());
        }
        // The tone is 0.3 peak (0.21 RMS): it comes back at about that level.
        for rms in &levels[10..] {
            assert!((0.15..0.3).contains(rms), "decoded level {rms} ({levels:?})");
        }
        // A spoken second, then their silence (no packets): one sentence comes out.
        for f in &frames {
            assert!(listener.push(f).is_none());
        }
        listener.last = Instant::now() - Duration::from_millis(400);
        let (sentence, voiced) = listener.flush().expect("the pause closes the sentence");
        assert!(voiced >= 800, "voiced {voiced} ms");
        assert!((16_000..32_000).contains(&sentence.len()), "{} samples", sentence.len());
    }

    #[test]
    fn dave_off_passes_frames_and_never_decrypts_silence() {
        let mut dave = Dave::new(1, 2);
        assert_eq!(dave.reinit(0).unwrap(), None);
        assert_eq!(dave.encrypt(b"frame").as_deref(), Some(&b"frame"[..]));
        assert_eq!(dave.decrypt(9, b"frame").as_deref(), Some(&b"frame"[..]));
        assert_eq!(dave.decrypt(9, &SILENCE), None);
        // DAVE on: a key package to send, and nothing leaves in the clear before the group's keys.
        let package = dave.reinit(1).unwrap().expect("a key package");
        assert!(package.len() > 100);
        dave.version = 1;
        assert_eq!(dave.encrypt(b"frame"), None);
        assert_eq!(dave.decrypt(9, b"frame"), None);
        // No group yet: a commit is not for him, a welcome without the server's external sender
        // is invalid (reported back with its transition id).
        assert_eq!(dave.commit_or_welcome(false, &[0, 7, 1, 2, 3]), Ok(None));
        assert_eq!(dave.commit_or_welcome(true, &[0, 7, 1, 2, 3]), Err(7));
        assert!(dave.proposals(&[]).is_err());
    }

    /// A local mock of Discord's voice server: handshake, address discovery, session
    /// description (transport encryption only), then his voice arrives over UDP and a friend's
    /// voice reaches him as one sentence.
    #[tokio::test]
    async fn a_call_with_a_mock_voice_server_carries_voice_both_ways() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_port = listener.local_addr().unwrap().port();
        let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let udp_port = udp.local_addr().unwrap().port();
        let key = [11u8; 32];
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let (head, mut reader, writer) = ws_client::accept(tcp).await.unwrap();
            assert!(head.starts_with("GET /?v=8 "), "{head}");
            let identify: Value = match reader.recv().await.unwrap() {
                Message::Text(t) => serde_json::from_str(&t).unwrap(),
                other => panic!("{other:?}"),
            };
            assert_eq!(identify["op"], 0);
            assert_eq!(identify["d"]["server_id"], "500");
            assert_eq!(identify["d"]["max_dave_protocol_version"], 1);
            writer.send_text(&json!({"op": 8, "d": {"heartbeat_interval": 60_000.0}}).to_string()).await.unwrap();
            writer.send_text(&json!({"op": 2, "d": {"ssrc": 77, "ip": "127.0.0.1", "port": udp_port, "modes": ["aead_xchacha20_poly1305_rtpsize", "aead_aes256_gcm_rtpsize"]}}).to_string()).await.unwrap();
            let mut buf = [0u8; 2048];
            let (n, jai) = udp.recv_from(&mut buf).await.unwrap();
            assert_eq!(n, 74);
            let mut answer = [0u8; 74];
            answer[1] = 2;
            answer[8..17].copy_from_slice(b"127.0.0.1");
            answer[72..74].copy_from_slice(&jai.port().to_be_bytes());
            udp.send_to(&answer, jai).await.unwrap();
            // Select protocol (heartbeats may come first).
            let select = loop {
                if let Message::Text(t) = reader.recv().await.unwrap() {
                    let v: Value = serde_json::from_str(&t).unwrap();
                    if v["op"] == 1 {
                        break v;
                    }
                }
            };
            assert_eq!(select["d"]["data"]["mode"], "aead_aes256_gcm_rtpsize");
            writer.send_text(&json!({"op": 4, "d": {"mode": "aead_aes256_gcm_rtpsize", "secret_key": key.to_vec(), "dave_protocol_version": 0}}).to_string()).await.unwrap();
            writer.send_text(&json!({"op": 5, "d": {"user_id": "4242", "ssrc": 99, "speaking": 1}}).to_string()).await.unwrap();
            // A friend talks for a second (ssrc 99), then stops.
            let transport = Transport::new(MODES[0], &key).unwrap();
            let mut friend = Sender::new(99).unwrap();
            for (i, chunk) in tone(1.0, 0.3).chunks(FRAME).enumerate() {
                let frame = friend.encode(chunk).unwrap();
                friend.timestamp = (i * FRAME) as u32;
                udp.send_to(&friend.packet(&transport, &frame).unwrap(), jai).await.unwrap();
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            // His voice: decode what arrives from ssrc 77 until a loud frame shows up.
            let mut decoder = Decoder::new(SampleRate::Hz48000, Channels::Stereo).unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                let Ok(Ok((n, _))) = tokio::time::timeout(Duration::from_secs(2), udp.recv_from(&mut buf)).await else { continue };
                let packet = &buf[..n];
                if n < 12 || packet[1] != OPUS_PT || u32::from_be_bytes([packet[8], packet[9], packet[10], packet[11]]) != 77 {
                    continue;
                }
                let opus = transport.open(packet).expect("his packets open with the session key");
                let mut pcm = vec![0f32; 5760 * 2];
                let n = decoder.decode_float(Some(Packet::try_from(&opus[..]).unwrap()), MutSignals::try_from(&mut pcm[..]).unwrap(), false).unwrap();
                let rms = (pcm[..n * 2].iter().map(|s| s * s).sum::<f32>() / (n * 2).max(1) as f32).sqrt();
                if rms > 0.1 {
                    return writer;
                }
            }
            panic!("his voice never arrived");
        });
        let (events_tx, mut events) = mpsc::unbounded_channel();
        let handle = connect(VoiceParams { endpoint: format!("ws://127.0.0.1:{ws_port}/?v=8"), guild_id: 500, channel_id: 600, user_id: 700, session_id: "s".into(), token: "t".into() }, events_tx).await.unwrap();
        // The friend's sentence comes out as one utterance from user 4242.
        let mut heard = None;
        let deadline = Instant::now() + Duration::from_secs(10);
        while heard.is_none() && Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(1), events.recv()).await {
                Ok(Some(VoiceEvent::Utterance { user_id, pcm, voiced_ms })) => heard = Some((user_id, pcm.len(), voiced_ms)),
                _ => continue,
            }
        }
        let (user, samples, voiced) = heard.expect("the friend's sentence");
        assert_eq!(user, 4242);
        assert!(voiced >= 700 && (14_000..34_000).contains(&samples), "{voiced} ms, {samples} samples");
        handle.audio().send(Audio::Samples(tone(0.6, 0.3))).unwrap();
        let writer = tokio::time::timeout(Duration::from_secs(15), server).await.unwrap().unwrap();
        drop(handle);
        let _ = writer.close(1000, "").await;
    }
}

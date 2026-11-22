//! Google Gemini Live over a WebSocket (`BidiGenerateContentConstrained`, authenticated by the one-use
//! ephemeral token the server minted with the whole session configuration locked in).
//!
//! Wire facts (verified against the live API on 2026-11-22 unless noted):
//! - Frames are JSON, sent by Google as *binary* WebSocket frames. The first client message is `{"setup":{}}`
//!   (the real setup is locked in the token); the server answers `setupComplete`.
//! - Mic: `realtimeInput.audio` base64 16-bit LE PCM, 16 kHz (`audio/pcm;rate=16000`). Bot audio comes as
//!   `serverContent.modelTurn.parts[].inlineData` PCM at 24 kHz, in bursts faster than real time.
//! - `serverContent.interrupted` = barge-in. `usageMetadata` arrives once per generation (with
//!   `turnComplete`); each carries the whole prompt again, so the values are per generation, not cumulative.
//! - Web search is Google's own `googleSearch` tool, enabled in the locked token setup and run inside the Live
//!   session: nothing to answer on our side. `serverContent.groundingMetadata` reports the queries (billed
//!   per query). Providers are compartmentalized: this module knows nothing about OpenAI's search tool.
//!
//! The supervisor speaks OpenAI events, so this session translates: `usageMetadata` becomes a `ResponseDone`
//! event, `interrupted` becomes `SpeechStarted`, `groundingMetadata` becomes `WebSearches`, and the
//! `response.create` client event (the greeting trigger) is mapped back to a Gemini text turn.

use crate::codec::FRAME_SAMPLES;
use crate::jitter::ring::{pcm_ring, PcmProducer};
use crate::openai::events::{ResponseSummary, ServerEvent, TokenDetails, Usage};
use crate::openai::Secret;
use crate::rtc::{mic_ring, rms, smooth, Handler, Levels, MicProducer, RtcState};
use base64::Engine as _;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tungstenite::client::IntoClientRequest;
use tungstenite::Message;

pub use crate::jitter::ring::PcmConsumer;

const TICK: Duration = Duration::from_millis(20);
/// Mic audio goes out in 40 ms messages (640 samples at 16 kHz).
const MIC_CHUNK_16K: usize = 640;
/// Start playing a response once this much bot audio is queued (200 ms), or after `PLAY_START_MAX_WAIT`.
/// With 60 ms the first word ("Привет") stuttered: Google's first chunks are small and the next one can be
/// late, so the playout ran dry mid-word. The cushion costs 200 ms at the start of each response only.
const PLAY_START_FRAMES: usize = 10;
/// How many frames (60 ms) the playout ring is kept filled with once a response is playing.
const RING_TARGET_FRAMES: usize = 3;
const PLAY_START_MAX_WAIT: Duration = Duration::from_millis(400);
const KICKOFF: &str = "[The call has just started. Greet the user as instructed.]";

/// Where to connect. `Endpoint::google()` is the real service; tests point at a local plain-text server.
#[derive(Debug, Clone)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub path: String,
}

impl Endpoint {
    pub fn google() -> Self {
        Self {
            host: "generativelanguage.googleapis.com".into(),
            port: 443,
            tls: true,
            path: "/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContentConstrained".into(),
        }
    }
}

#[derive(Debug)]
pub struct GeminiError(pub String);

impl std::fmt::Display for GeminiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for GeminiError {}

fn err<T: std::fmt::Display>(ctx: &'static str) -> impl FnOnce(T) -> GeminiError {
    move |e| GeminiError(format!("{ctx}: {e}"))
}

#[derive(Debug)]
enum Cmd {
    /// A client event in OpenAI's vocabulary, translated by the session thread.
    Event(String),
    Close,
}

#[derive(Debug)]
pub struct GeminiSession {
    tx: Sender<Cmd>,
    join: Option<JoinHandle<()>>,
    muted: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    pub levels: Levels,
}

impl GeminiSession {
    /// Connect (blocking: TCP, TLS, WebSocket handshake) and start the session thread.
    pub fn start(
        endpoint: &Endpoint,
        token: &Secret,
        handler: impl Handler,
    ) -> Result<(Self, MicProducer, PcmConsumer), GeminiError> {
        let ws = connect(endpoint, token)?;
        let (mic_p, mic_c) = mic_ring();
        let (pcm_p, pcm_c) = pcm_ring(16);
        let (tx, rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let muted = Arc::new(AtomicBool::new(false));
        let levels = Levels::default();
        let worker = Worker {
            ws,
            rx,
            handler: Box::new(handler),
            mic: mic_c,
            pcm: pcm_p,
            muted: Arc::clone(&muted),
            stop: Arc::clone(&stop),
            levels: levels.clone(),
            mic16: Vec::with_capacity(MIC_CHUNK_16K * 2),
            was_muted: false,
            bot: VecDeque::new(),
            playing: false,
            first_audio: None,
            last_audio: Instant::now(),
            prev24: 0,
            skip_next_create: false,
        };
        let join = std::thread::Builder::new()
            .name("lexi-gemini".into())
            .spawn(move || worker.run())
            .map_err(err("spawn"))?;
        Ok((
            Self {
                tx,
                join: Some(join),
                muted,
                stop,
                levels,
            },
            mic_p,
            pcm_c,
        ))
    }

    /// A client event in OpenAI's format (`response.create` as the greeting trigger, a typed user message);
    /// translated to the Gemini equivalent.
    pub fn send_event(&self, json: String) {
        let _ = self.tx.send(Cmd::Event(json));
    }

    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    pub fn close(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.tx.send(Cmd::Close);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for GeminiSession {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ----- transport -------------------------------------------------------------------------------

enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Stream {
    fn tcp(&self) -> &TcpStream {
        match self {
            Stream::Plain(s) => s,
            Stream::Tls(s) => &s.sock,
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

type Ws = tungstenite::WebSocket<Stream>;

fn tls_config() -> Result<Arc<rustls::ClientConfig>, GeminiError> {
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let provider = Arc::new(crate::tls::provider());
    let cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(err("tls versions"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(cfg))
}

fn connect(ep: &Endpoint, token: &Secret) -> Result<Ws, GeminiError> {
    let addr = (ep.host.as_str(), ep.port)
        .to_socket_addrs()
        .map_err(err("resolve"))?
        .next()
        .ok_or_else(|| GeminiError("resolve: no address".into()))?;
    let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(8)).map_err(err("connect"))?;
    let _ = tcp.set_nodelay(true);
    tcp.set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(err("timeout"))?;
    tcp.set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(err("timeout"))?;
    let stream = if ep.tls {
        let name =
            rustls::pki_types::ServerName::try_from(ep.host.clone()).map_err(err("server name"))?;
        let conn = rustls::ClientConnection::new(tls_config()?, name).map_err(err("tls"))?;
        Stream::Tls(Box::new(rustls::StreamOwned::new(conn, tcp)))
    } else {
        Stream::Plain(tcp)
    };
    // The token is a secret: it only ever lives in this URL, which is never logged.
    let url = format!(
        "{}://{}:{}{}?access_token={}",
        if ep.tls { "wss" } else { "ws" },
        ep.host,
        ep.port,
        ep.path,
        token.expose()
    );
    let request = url.into_client_request().map_err(err("request"))?;
    let (ws, _resp) = tungstenite::client(request, stream)
        .map_err(|e| GeminiError(format!("websocket handshake: {}", handshake_reason(&e))))?;
    let _ = ws
        .get_ref()
        .tcp()
        .set_read_timeout(Some(Duration::from_millis(10)));
    Ok(ws)
}

/// Handshake errors can embed the request (and so the token); keep only the kind.
fn handshake_reason<R: tungstenite::handshake::HandshakeRole>(
    e: &tungstenite::HandshakeError<R>,
) -> String {
    match e {
        tungstenite::HandshakeError::Failure(tungstenite::Error::Http(r)) => {
            format!("HTTP {}", r.status())
        }
        tungstenite::HandshakeError::Failure(tungstenite::Error::Io(e)) => {
            format!("io: {}", e.kind())
        }
        tungstenite::HandshakeError::Failure(_) => "failed".into(),
        tungstenite::HandshakeError::Interrupted(_) => "interrupted".into(),
    }
}

// ----- the session thread ----------------------------------------------------------------------

struct Worker {
    ws: Ws,
    rx: Receiver<Cmd>,
    handler: Box<dyn Handler>,
    mic: rtrb::Consumer<i16>,
    pcm: PcmProducer,
    muted: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    levels: Levels,
    /// Mic audio already downsampled to 16 kHz, waiting for a full chunk.
    mic16: Vec<i16>,
    was_muted: bool,
    /// Bot audio at 48 kHz waiting to be paced out in 20 ms frames.
    bot: VecDeque<i16>,
    playing: bool,
    first_audio: Option<Instant>,
    last_audio: Instant,
    /// Last 24 kHz sample, for interpolation across chunk boundaries.
    prev24: i16,
    /// A typed user message is answered by Gemini at once: the `response.create` that follows is a no-op.
    skip_next_create: bool,
}

impl Worker {
    fn run(mut self) {
        let reason = self.run_loop();
        let _ = self.ws.close(None);
        self.handler.on_state(RtcState::Closed(reason));
    }

    fn run_loop(&mut self) -> String {
        if let Err(e) = self.send_json(&json!({ "setup": {} })) {
            return e;
        }
        let mut next_tick = Instant::now() + TICK;
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return "closed".into();
            }
            loop {
                match self.rx.try_recv() {
                    Ok(Cmd::Close) => return "closed".into(),
                    Ok(Cmd::Event(j)) => {
                        if let Err(e) = self.client_event(&j) {
                            return e;
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return "closed".into(),
                }
            }
            // Drain what has arrived (each read waits at most the 10 ms socket timeout).
            for _ in 0..256 {
                match self.ws.read() {
                    Ok(Message::Text(t)) => self.server_message(t.as_bytes()),
                    Ok(Message::Binary(b)) => self.server_message(&b),
                    Ok(Message::Close(c)) => {
                        let why = c.map(|f| f.reason.to_string()).unwrap_or_default();
                        // Reasons are Google's text ("quota exceeded", "credits depleted"...): useful and not secret.
                        log::info!("gemini closed by server: {why}");
                        return "remote".into();
                    }
                    Ok(_) => {}
                    Err(tungstenite::Error::Io(e))
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        break
                    }
                    Err(
                        tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed,
                    ) => return "remote".into(),
                    Err(e) => {
                        log::warn!("gemini websocket error: {}", error_kind(&e));
                        return "ice disconnected".into(); // lets the supervisor try to reconnect
                    }
                }
            }
            let now = Instant::now();
            if now >= next_tick {
                next_tick = (next_tick + TICK).max(now);
                if let Err(e) = self.mic_tick() {
                    return e;
                }
                self.play_tick();
            }
        }
    }

    fn send_json(&mut self, v: &Value) -> Result<(), String> {
        match self.ws.send(Message::text(v.to_string())) {
            Ok(()) => Ok(()),
            // The frame is queued inside tungstenite and goes out with the next write or flush.
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                Ok(())
            }
            Err(e) => {
                log::warn!("gemini send failed: {}", error_kind(&e));
                Err("ice disconnected".into())
            }
        }
    }

    /// Translate a client event from the supervisor (OpenAI vocabulary) to Gemini.
    fn client_event(&mut self, json: &str) -> Result<(), String> {
        let Ok(v) = serde_json::from_str::<Value>(json) else {
            return Ok(());
        };
        match v["type"].as_str() {
            Some("response.create") => {
                if std::mem::take(&mut self.skip_next_create) {
                    return Ok(());
                }
                self.send_json(&json!({ "realtimeInput": { "text": KICKOFF } }))
            }
            // A typed user message (the desktop harness's --prompt): Gemini answers it as soon as it arrives,
            // so the `response.create` that follows must not start a second turn.
            Some("conversation.item.create") if v["item"]["type"] == "message" => {
                let text = v["item"]["content"][0]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                self.skip_next_create = true;
                self.send_json(&json!({ "realtimeInput": { "text": text } }))
            }
            _ => Ok(()),
        }
    }

    fn server_message(&mut self, bytes: &[u8]) {
        let Ok(v) = serde_json::from_slice::<Value>(bytes) else {
            return;
        };
        if v.get("setupComplete").is_some() {
            self.handler.on_state(RtcState::Connected);
            self.handler.on_state(RtcState::ChannelOpen);
        }
        if let Some(sc) = v.get("serverContent") {
            if sc["interrupted"] == true {
                self.bot.clear();
                self.playing = false;
                self.first_audio = None; // the next response gets its own cushion
                self.pcm.flush();
                self.handler.on_event(ServerEvent::SpeechStarted {
                    audio_start_ms: None,
                });
            }
            // Google's built-in search ran: it is billed per query, so tell the supervisor how many.
            if let Some(g) = sc.get("groundingMetadata") {
                let n = g["webSearchQueries"]
                    .as_array()
                    .map_or(1, |q| q.len().max(1));
                self.handler
                    .on_event(ServerEvent::WebSearches { count: n as u32 });
            }
            for part in sc["modelTurn"]["parts"].as_array().into_iter().flatten() {
                if let Some(b64) = part["inlineData"]["data"].as_str() {
                    self.push_bot_audio(b64);
                }
            }
        }
        if let Some(u) = v.get("usageMetadata") {
            self.handler.on_event(ServerEvent::ResponseDone {
                response: ResponseSummary {
                    status: Some("completed".into()),
                    usage: Some(usage_from_gemini(u)),
                    ..ResponseSummary::default()
                },
            });
        }
    }

    fn push_bot_audio(&mut self, b64: &str) {
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64) else {
            return;
        };
        let pcm24: Vec<i16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b))
            .collect();
        upsample_2x(&mut self.prev24, &pcm24, &mut self.bot);
        self.last_audio = Instant::now();
        self.first_audio.get_or_insert(self.last_audio);
    }

    /// Pace bot audio into the playout ring one 20 ms frame per tick.
    fn play_tick(&mut self) {
        if !self.playing && !self.bot.is_empty() {
            let waited = self
                .first_audio
                .is_some_and(|t| t.elapsed() >= PLAY_START_MAX_WAIT);
            if self.bot.len() >= PLAY_START_FRAMES * FRAME_SAMPLES || waited {
                self.playing = true;
                log::info!(
                    "gemini playout starts with {} ms queued",
                    self.bot.len() * 1000 / crate::codec::SAMPLE_RATE as usize
                );
            }
        }
        if self.playing {
            let ended = self.last_audio.elapsed() > Duration::from_millis(60);
            // Keep the playout ring itself topped up to RING_TARGET_FRAMES. The audio callback pulls on its own
            // 20 ms clock, so a ring that holds 0-1 frames underruns at the slightest jitter (audible as
            // stutter); a 60 ms ring absorbs it. (The consumer drops backlog beyond 6 frames, so stay below.)
            let mut pushed = false;
            for _ in 0..RING_TARGET_FRAMES {
                if self.pcm.buffered() >= RING_TARGET_FRAMES * FRAME_SAMPLES {
                    break;
                }
                if self.bot.len() < FRAME_SAMPLES && !(ended && !self.bot.is_empty()) {
                    break;
                }
                let mut frame = [0i16; FRAME_SAMPLES];
                for (i, s) in self.bot.iter().take(FRAME_SAMPLES).enumerate() {
                    frame[i] = *s;
                }
                let n = self.pcm.push(&frame).min(self.bot.len());
                self.bot.drain(..n);
                smooth(&self.levels.bot, rms(&frame));
                pushed = true;
            }
            if pushed {
                return;
            }
            if self.bot.is_empty() {
                self.playing = false;
                self.first_audio = None;
            }
        }
        smooth(&self.levels.bot, 0.0);
    }

    /// Move captured audio to Google: 48 kHz -> 16 kHz, 40 ms per message. Muted audio is dropped, and
    /// `audioStreamEnd` flushes whatever the server buffered when the mute starts.
    fn mic_tick(&mut self) -> Result<(), String> {
        let muted = self.muted.load(Ordering::Relaxed);
        for _ in 0..4 {
            if self.mic.slots() < FRAME_SAMPLES {
                break;
            }
            let mut frame = [0i16; FRAME_SAMPLES];
            if let Ok(chunk) = self.mic.read_chunk(FRAME_SAMPLES) {
                let (a, b) = chunk.as_slices();
                frame[..a.len()].copy_from_slice(a);
                frame[a.len()..a.len() + b.len()].copy_from_slice(b);
                chunk.commit_all();
            }
            if muted {
                frame.fill(0);
            }
            smooth(&self.levels.mic, rms(&frame));
            if !muted {
                downsample_3x(&frame, &mut self.mic16);
            }
        }
        if muted {
            self.mic16.clear();
            if !self.was_muted {
                self.was_muted = true;
                return self.send_json(&json!({ "realtimeInput": { "audioStreamEnd": true } }));
            }
            return Ok(());
        }
        self.was_muted = false;
        while self.mic16.len() >= MIC_CHUNK_16K {
            let chunk: Vec<i16> = self.mic16.drain(..MIC_CHUNK_16K).collect();
            let bytes: Vec<u8> = chunk.iter().flat_map(|s| s.to_le_bytes()).collect();
            let data = base64::engine::general_purpose::STANDARD.encode(bytes);
            self.send_json(&json!({ "realtimeInput": { "audio": {
                "data": data, "mimeType": "audio/pcm;rate=16000"
            } } }))?;
        }
        Ok(())
    }
}

/// Error kinds without payloads (a URL or header could be embedded in the text form).
fn error_kind(e: &tungstenite::Error) -> &'static str {
    match e {
        tungstenite::Error::Io(_) => "io",
        tungstenite::Error::Protocol(_) => "protocol",
        tungstenite::Error::Capacity(_) => "capacity",
        tungstenite::Error::Utf8(_) => "utf8",
        _ => "other",
    }
}

// ----- audio and usage helpers -----------------------------------------------------------------

/// 48 kHz -> 16 kHz by averaging each group of three samples (a crude low-pass; fine for speech).
pub fn downsample_3x(src: &[i16], out: &mut Vec<i16>) {
    out.extend(
        src.as_chunks::<3>()
            .0
            .iter()
            .map(|t| ((i32::from(t[0]) + i32::from(t[1]) + i32::from(t[2])) / 3) as i16),
    );
}

/// 24 kHz -> 48 kHz by linear interpolation; `prev` carries the last input sample across chunks.
pub fn upsample_2x(prev: &mut i16, src: &[i16], out: &mut VecDeque<i16>) {
    for &s in src {
        out.push_back(((i32::from(*prev) + i32::from(s)) / 2) as i16);
        out.push_back(s);
        *prev = s;
    }
}

/// Map Gemini's `usageMetadata` (one generation) onto the OpenAI-shaped [`Usage`] the supervisor prices.
/// Thinking tokens are billed as output, so they count as output text; tool-use prompt tokens as input text.
pub fn usage_from_gemini(u: &Value) -> Usage {
    let n = |v: &Value| v.as_u64().unwrap_or(0);
    let by_modality = |details: &Value, m: &str| -> u64 {
        details
            .as_array()
            .into_iter()
            .flatten()
            .filter(|d| d["modality"] == m)
            .map(|d| n(&d["tokenCount"]))
            .sum()
    };
    let prompt = n(&u["promptTokenCount"]);
    let tool_use = n(&u["toolUsePromptTokenCount"]);
    let (p_audio, p_image) = (
        by_modality(&u["promptTokensDetails"], "AUDIO"),
        by_modality(&u["promptTokensDetails"], "IMAGE")
            + by_modality(&u["promptTokensDetails"], "VIDEO"),
    );
    // Without a breakdown everything counts as text; with one, text is what is left of the prompt.
    let p_text = prompt.saturating_sub(p_audio + p_image) + tool_use;
    let response = n(&u["responseTokenCount"]);
    let o_audio = by_modality(&u["responseTokensDetails"], "AUDIO");
    let o_text = response.saturating_sub(o_audio) + n(&u["thoughtsTokenCount"]);
    Usage {
        total_tokens: n(&u["totalTokenCount"]),
        input_tokens: prompt + tool_use,
        output_tokens: response + n(&u["thoughtsTokenCount"]),
        input_token_details: TokenDetails {
            text_tokens: p_text,
            audio_tokens: p_audio,
            image_tokens: p_image,
            ..TokenDetails::default()
        },
        output_token_details: TokenDetails {
            text_tokens: o_text,
            audio_tokens: o_audio,
            ..TokenDetails::default()
        },
    }
}

#[cfg(test)]
mod tests;

//! str0m session, UDP loop, SDP and data channel (SPEC §4.1, §4.3).
//!
//! One **net thread** owns the `str0m::Rtc` and all UDP sockets' send side. Per-socket reader
//! threads only forward datagrams over a channel. The net thread:
//!
//! * runs the sans-I/O loop (`poll_output` → transmit / event / timeout, then ONE mutation),
//! * every 20 ms pulls mic PCM from a lock-free ring, Opus-encodes it and writes RTP,
//! * feeds received RTP into the [`Playout`] jitter buffer and pushes decoded 20 ms frames into
//!   a lock-free PCM ring for the audio output callback (see [`crate::jitter::ring`]),
//! * flushes playout on `input_audio_buffer.speech_started` (barge-in).
//!
//! API shape vs PLAN: instead of `send_pcm_frame`/`on_pcm` callbacks, audio crosses threads
//! through the two rings returned by [`RtcSession::start`] (a [`MicProducer`] to push captured
//! PCM and a [`PcmConsumer`] to pull playout PCM) so that audio callbacks never lock, allocate
//! or call into other code.

use crate::codec::{Encoded, OpusEncoder, FRAME_SAMPLES, SAMPLE_RATE};
use crate::jitter::ring::{pcm_ring, PcmConsumer, PcmProducer};
use crate::jitter::{FrameKind, Playout};
use crate::openai::{self, parse_event, Secret, ServerEvent, DATA_CHANNEL_LABEL};
use rtrb::{Consumer, Producer, RingBuffer};
use std::collections::VecDeque;
use std::fmt;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use str0m::change::SdpAnswer;
use str0m::channel::ChannelId;
use str0m::media::{Direction, Frequency, MediaKind, MediaTime, Mid};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc};

const TICK: Duration = Duration::from_millis(20);
const MAX_SOCKETS: usize = 8;
/// Mic ring capacity: 200 ms.
const MIC_RING_FRAMES: usize = 10;

#[derive(Debug)]
pub struct RtcError(pub String);

impl fmt::Display for RtcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "rtc: {}", self.0)
    }
}
impl std::error::Error for RtcError {}

fn e<T: fmt::Display>(ctx: &'static str) -> impl FnOnce(T) -> RtcError {
    move |x| RtcError(format!("{ctx}: {x}"))
}

#[derive(Debug, Clone)]
pub struct RtcConfig {
    /// Base URL for `/v1/realtime/calls` (override for tests only).
    pub openai_base: String,
    /// Also use loopback addresses as host candidates (tests / local peers).
    pub include_loopback: bool,
}

impl Default for RtcConfig {
    fn default() -> Self {
        Self {
            openai_base: openai::OPENAI_BASE.into(),
            include_loopback: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RtcState {
    /// ICE/DTLS connected; media can flow.
    Connected,
    /// The `oai-events` channel is open.
    ChannelOpen,
    /// Session ended (reason is for display only).
    Closed(String),
}

/// Callbacks run on the net thread: keep them short and non-blocking.
pub trait Handler: Send + 'static {
    fn on_state(&mut self, state: RtcState);
    fn on_event(&mut self, event: ServerEvent);
}

/// Producer side of the mic ring: push captured 48 kHz mono PCM here (audio-callback safe).
#[derive(Debug)]
pub struct MicProducer(Producer<i16>);

impl MicProducer {
    /// Returns the number of samples accepted (the rest are dropped if the ring is full).
    pub fn push(&mut self, samples: &[i16]) -> usize {
        let n = samples.len().min(self.0.slots());
        if n == 0 {
            return 0;
        }
        match self.0.write_chunk_uninit(n) {
            Ok(c) => {
                c.fill_from_iter(samples[..n].iter().copied());
                n
            }
            Err(_) => 0,
        }
    }
}

/// Create the mic ring (200 ms): the producer goes to the capture callback, the consumer to the net thread.
pub fn mic_ring() -> (MicProducer, Consumer<i16>) {
    let (p, c) = RingBuffer::new(MIC_RING_FRAMES * FRAME_SAMPLES);
    (MicProducer(p), c)
}

/// Smoothed RMS levels (0..1) published by the net thread, as `f32` bits.
#[derive(Debug, Clone, Default)]
pub struct Levels {
    mic: Arc<AtomicU32>,
    bot: Arc<AtomicU32>,
}

impl Levels {
    pub fn mic(&self) -> f32 {
        f32::from_bits(self.mic.load(Ordering::Relaxed))
    }
    pub fn bot(&self) -> f32 {
        f32::from_bits(self.bot.load(Ordering::Relaxed))
    }
}

fn rms(pcm: &[i16]) -> f32 {
    if pcm.is_empty() {
        return 0.0;
    }
    let sum: f64 = pcm.iter().map(|&s| f64::from(s).powi(2)).sum();
    ((sum / pcm.len() as f64).sqrt() / 32768.0) as f32
}

fn smooth(atom: &AtomicU32, new: f32) {
    let old = f32::from_bits(atom.load(Ordering::Relaxed));
    // fast attack, slower release
    let k = if new > old { 0.5 } else { 0.15 };
    atom.store((old + (new - old) * k).to_bits(), Ordering::Relaxed);
}

enum Msg {
    Packet {
        sock: usize,
        source: SocketAddr,
        data: Vec<u8>,
        at: Instant,
    },
    SendEvent(String),
    Close,
}

#[derive(Debug)]
pub struct RtcSession {
    tx: Sender<Msg>,
    join: Option<JoinHandle<()>>,
    muted: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    pub levels: Levels,
}

type Signaling<'a> = &'a mut dyn FnMut(&str) -> Result<String, RtcError>;

impl RtcSession {
    /// Build an offer, exchange it with OpenAI (`POST /v1/realtime/calls`, SDP in / SDP out) using
    /// the ephemeral `token`, and start the net thread. Blocks for the HTTP round trip.
    pub fn start(
        cfg: RtcConfig,
        token: &Secret,
        handler: impl Handler,
    ) -> Result<(Self, MicProducer, PcmConsumer), RtcError> {
        let base = cfg.openai_base.clone();
        let mut sig =
            |offer: &str| openai::exchange_sdp(&base, token, offer).map_err(e("sdp exchange"));
        Self::start_with_signaling(cfg, &mut sig, handler)
    }

    /// Like [`start`](Self::start) but with caller-provided offer→answer signaling (tests).
    pub fn start_with_signaling(
        cfg: RtcConfig,
        signaling: Signaling<'_>,
        handler: impl Handler,
    ) -> Result<(Self, MicProducer, PcmConsumer), RtcError> {
        let sockets = bind_sockets(cfg.include_loopback)?;
        let mut rtc = Rtc::builder()
            .clear_codecs()
            .enable_opus(true, false)
            .set_stats_interval(None)
            .build(Instant::now());
        for s in &sockets {
            let addr = s.local_addr().map_err(e("local_addr"))?;
            let c = Candidate::host(addr, "udp").map_err(e("candidate"))?;
            rtc.add_local_candidate(c);
        }

        let mut api = rtc.sdp_api();
        let mid = api.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
        let _cid = api.add_channel(DATA_CHANNEL_LABEL.into());
        let (offer, pending) = api
            .apply()
            .ok_or_else(|| RtcError("no offer produced".into()))?;
        let answer_sdp = signaling(&offer.to_sdp_string())?;
        let answer = SdpAnswer::from_sdp_string(&answer_sdp).map_err(e("parse answer"))?;
        rtc.sdp_api()
            .accept_answer(pending, answer)
            .map_err(e("accept answer"))?;

        let (mic_p, mic_c) = mic_ring();
        let (pcm_p, pcm_c) = pcm_ring(16);
        let (tx, rx) = mpsc::channel::<Msg>();
        let stop = Arc::new(AtomicBool::new(false));
        let muted = Arc::new(AtomicBool::new(false));
        let levels = Levels::default();

        let mut readers = Vec::new();
        for (i, s) in sockets.iter().enumerate() {
            let rs = s.try_clone().map_err(e("clone socket"))?;
            rs.set_read_timeout(Some(Duration::from_millis(100)))
                .map_err(e("read timeout"))?;
            let (tx, stop) = (tx.clone(), Arc::clone(&stop));
            readers.push(std::thread::spawn(move || reader_loop(i, rs, tx, stop)));
        }

        let net = Net {
            rtc,
            mid,
            sockets,
            rx,
            handler: Box::new(handler),
            enc: OpusEncoder::new().map_err(e("encoder"))?,
            playout: Playout::new().map_err(e("playout"))?,
            mic: mic_c,
            pcm: pcm_p,
            muted: Arc::clone(&muted),
            levels: levels.clone(),
            channel: None,
            pending_events: VecDeque::new(),
            connected: false,
            readers,
            stop: Arc::clone(&stop),
        };
        let join = std::thread::Builder::new()
            .name("lexi-net".into())
            .spawn(move || net.run())
            .map_err(e("spawn"))?;
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

    /// Send a client event (JSON text) on the `oai-events` channel; queued until it opens.
    pub fn send_event(&self, json: String) {
        let _ = self.tx.send(Msg::SendEvent(json));
    }

    /// Muted: the mic ring is drained but silence is encoded (DTX makes that free).
    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    /// Close the session and join the net thread.
    pub fn close(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        let _ = self.tx.send(Msg::Close);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for RtcSession {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn local_ips(include_loopback: bool) -> Vec<IpAddr> {
    let mut ips: Vec<IpAddr> = if_addrs::get_if_addrs()
        .map(|v| v.into_iter().map(|i| i.ip()).collect())
        .unwrap_or_default();
    ips.retain(|ip| match ip {
        IpAddr::V4(v4) => {
            (include_loopback || !v4.is_loopback()) && !v4.is_link_local() && !v4.is_unspecified()
        }
        IpAddr::V6(v6) => {
            let link_local = (v6.segments()[0] & 0xffc0) == 0xfe80;
            (include_loopback || !v6.is_loopback()) && !link_local && !v6.is_unspecified()
        }
    });
    ips.sort();
    ips.dedup();
    ips.truncate(MAX_SOCKETS);
    ips
}

fn bind_sockets(include_loopback: bool) -> Result<Vec<UdpSocket>, RtcError> {
    let mut out = Vec::new();
    for ip in local_ips(include_loopback) {
        if let Ok(s) = UdpSocket::bind(SocketAddr::new(ip, 0)) {
            out.push(s);
        }
    }
    if out.is_empty() {
        return Err(RtcError("no usable network interface".into()));
    }
    Ok(out)
}

fn reader_loop(idx: usize, sock: UdpSocket, tx: Sender<Msg>, stop: Arc<AtomicBool>) {
    let mut buf = vec![0u8; 2048];
    while !stop.load(Ordering::Relaxed) {
        match sock.recv_from(&mut buf) {
            Ok((n, source)) => {
                let m = Msg::Packet {
                    sock: idx,
                    source,
                    data: buf[..n].to_vec(),
                    at: Instant::now(),
                };
                if tx.send(m).is_err() {
                    return;
                }
            }
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            // e.g. ICMP "connection refused" surfacing on UDP: keep going.
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
}

struct Net {
    rtc: Rtc,
    mid: Mid,
    sockets: Vec<UdpSocket>,
    rx: mpsc::Receiver<Msg>,
    handler: Box<dyn Handler>,
    enc: OpusEncoder,
    playout: Playout,
    mic: Consumer<i16>,
    pcm: PcmProducer,
    muted: Arc<AtomicBool>,
    levels: Levels,
    channel: Option<ChannelId>,
    pending_events: VecDeque<String>,
    connected: bool,
    readers: Vec<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
}

impl Net {
    fn run(mut self) {
        let reason = self.run_loop();
        // Best-effort: let str0m emit its close packets (DTLS close_notify) before dropping sockets.
        self.rtc.disconnect();
        let _ = self.drain();
        self.stop.store(true, Ordering::Relaxed);
        for r in self.readers.drain(..) {
            let _ = r.join();
        }
        self.handler.on_state(RtcState::Closed(reason));
    }

    fn socket_for(&self, source: SocketAddr, dest: SocketAddr) -> Option<&UdpSocket> {
        self.sockets
            .iter()
            .find(|s| s.local_addr().map(|a| a == source).unwrap_or(false))
            .or_else(|| {
                self.sockets.iter().find(|s| {
                    s.local_addr()
                        .map(|a| a.is_ipv4() == dest.is_ipv4())
                        .unwrap_or(false)
                })
            })
    }

    /// Drain `poll_output` to the next timeout (the single-mutation invariant).
    fn drain(&mut self) -> Result<Instant, String> {
        loop {
            match self.rtc.poll_output().map_err(|x| x.to_string())? {
                Output::Timeout(t) => return Ok(t),
                Output::Transmit(t) => {
                    if let Some(s) = self.socket_for(t.source, t.destination) {
                        // UDP send errors are transient; ICE/RTP tolerate loss.
                        let _ = s.send_to(&t.contents, t.destination);
                    }
                }
                Output::Event(ev) => {
                    if let Some(reason) = self.on_rtc_event(ev) {
                        return Err(reason);
                    }
                }
            }
        }
    }

    /// Returns `Some(reason)` if the session is over.
    fn on_rtc_event(&mut self, ev: Event) -> Option<String> {
        match ev {
            Event::Connected => {
                self.connected = true;
                self.handler.on_state(RtcState::Connected);
            }
            Event::IceConnectionStateChange(IceConnectionState::Disconnected) => {
                return Some("ice disconnected".into());
            }
            Event::ChannelOpen(id, label) if label == DATA_CHANNEL_LABEL => {
                self.channel = Some(id);
                self.handler.on_state(RtcState::ChannelOpen);
                while let Some(m) = self.pending_events.pop_front() {
                    self.write_channel(&m);
                }
            }
            Event::ChannelData(d) if !d.binary => {
                if let Some(ev) = std::str::from_utf8(&d.data).ok().and_then(parse_event) {
                    if matches!(ev, ServerEvent::SpeechStarted { .. }) {
                        // Barge-in: drop everything not yet played.
                        self.playout.flush();
                        self.pcm.flush();
                    }
                    self.handler.on_event(ev);
                }
            }
            Event::ChannelClose(id) if Some(id) == self.channel => {
                return Some("data channel closed".into());
            }
            Event::MediaData(m) => {
                self.playout.push(**m.seq_range.start(), m.data.to_vec());
            }
            _ => {}
        }
        None
    }

    fn write_channel(&mut self, text: &str) {
        if let Some(mut ch) = self.channel.and_then(|id| self.rtc.channel(id)) {
            let _ = ch.write(false, text.as_bytes());
        }
    }

    /// Encode and send one mic frame if a full frame is buffered. Returns true if a frame was consumed.
    fn mic_frame(&mut self) -> bool {
        if self.mic.slots() < FRAME_SAMPLES {
            return false;
        }
        let mut frame = [0i16; FRAME_SAMPLES];
        if let Ok(chunk) = self.mic.read_chunk(FRAME_SAMPLES) {
            let (a, b) = chunk.as_slices();
            frame[..a.len()].copy_from_slice(a);
            frame[a.len()..a.len() + b.len()].copy_from_slice(b);
            chunk.commit_all();
        }
        if self.muted.load(Ordering::Relaxed) {
            frame.fill(0);
        }
        smooth(&self.levels.mic, rms(&frame));
        if !self.connected {
            return true; // discard stale audio until media can flow
        }
        let Ok(Encoded::Packet { ts, data }) = self.enc.encode(&frame) else {
            return true; // DTX or encode error: nothing to send
        };
        if let Some(w) = self.rtc.writer(self.mid) {
            let pt = w.payload_params().next().map(|p| p.pt());
            if let Some(pt) = pt {
                let _ = w.write(
                    pt,
                    Instant::now(),
                    MediaTime::new(ts, Frequency::FORTY_EIGHT_KHZ),
                    data,
                );
            }
        }
        true
    }

    fn playout_tick(&mut self) {
        let mut frame = [0i16; FRAME_SAMPLES];
        match self.playout.tick(&mut frame) {
            Ok(FrameKind::Silence) | Err(_) => smooth(&self.levels.bot, 0.0),
            Ok(_) => {
                smooth(&self.levels.bot, rms(&frame));
                self.pcm.push(&frame);
            }
        }
    }

    fn run_loop(&mut self) -> String {
        let mut next_tick = Instant::now() + TICK;
        loop {
            let deadline = match self.drain() {
                Ok(t) => t,
                Err(reason) => return reason,
            };
            if !self.rtc.is_alive() {
                return "rtc closed".into();
            }
            // One mutation per iteration: a mic frame, a network packet, a timeout, or a command.
            if self.mic_frame() {
                continue;
            }
            let now = Instant::now();
            if now >= next_tick {
                self.playout_tick();
                next_tick += TICK;
                if now > next_tick + TICK * 5 {
                    next_tick = now + TICK; // we stalled; do not burst
                }
            }
            let wake = deadline.min(next_tick);
            let wait = wake
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(1));
            match self.rx.recv_timeout(wait) {
                Ok(Msg::Packet {
                    sock,
                    source,
                    data,
                    at,
                }) => {
                    let Some(dest) = self.sockets.get(sock).and_then(|s| s.local_addr().ok())
                    else {
                        continue;
                    };
                    let Ok(contents) = data.as_slice().try_into() else {
                        continue;
                    };
                    let input = Input::Receive(
                        at,
                        Receive {
                            proto: Protocol::Udp,
                            source,
                            destination: dest,
                            contents,
                        },
                    );
                    if let Err(x) = self.rtc.handle_input(input) {
                        return format!("rtc input: {x}");
                    }
                }
                Ok(Msg::SendEvent(s)) => {
                    if self.channel.is_some() {
                        self.write_channel(&s);
                    } else {
                        self.pending_events.push_back(s);
                    }
                }
                Ok(Msg::Close) => return "closed by caller".into(),
                Err(RecvTimeoutError::Timeout) => {
                    let now = Instant::now();
                    if now >= deadline {
                        if let Err(x) = self.rtc.handle_input(Input::Timeout(now)) {
                            return format!("rtc timeout: {x}");
                        }
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return "command channel closed".into(),
            }
        }
    }
}

/// Sample rate expected by [`MicProducer`] and produced by the playout ring.
pub const PCM_SAMPLE_RATE: u32 = SAMPLE_RATE;

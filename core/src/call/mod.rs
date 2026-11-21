//! Call supervisor (SPEC §2.4, §4.1): minter → [`rtc`](crate::rtc) session → audio backend, plus
//! mute, reconnect with backoff, the max-session guard and level publishing.
//!
//! Platform independent: the session factory ([`Connector`]) and the audio side ([`AudioBackend`])
//! are traits, so the state machine is unit-tested on the desktop with mocks and used unchanged on
//! Android (where the backend is AAudio). One supervisor thread owns everything; the network is
//! only touched on short-lived connect workers so a hang-up is never blocked by an HTTP call.

use crate::jitter::ring::PcmConsumer;
use crate::openai::{self, attest, OpenAiError, Secret, ServerEvent};
use crate::pricing;
use crate::rtc::{Handler, MicProducer, RtcConfig, RtcSession, RtcState};
use crate::state::{CallState, Shared};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Why a connect attempt failed; [`code`](Self::code) is what the UI receives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectError {
    Network,
    Auth,
    RateLimited,
    Service,
    Other,
}

impl ConnectError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::Auth => "auth",
            Self::RateLimited => "rate",
            Self::Service => "service",
            Self::Other => "error",
        }
    }
}

/// Events from a live session (delivered on the net thread; keep consumers non-blocking).
#[derive(Debug, Clone)]
pub enum SessionEvent {
    /// ICE/DTLS up: media flows.
    Connected,
    ChannelOpen,
    Closed(String),
    Server(ServerEvent),
}

pub type EventSink = Arc<dyn Fn(SessionEvent) + Send + Sync>;

pub trait Session: Send {
    /// Send a client event (JSON) on the `oai-events` channel (queued until it opens).
    fn send_event(&self, json: String);
    fn set_muted(&self, muted: bool);
    /// Smoothed (mic, bot) RMS levels, 0..1.
    fn levels(&self) -> (f32, f32);
    fn close(self: Box<Self>);
}

pub struct Connected {
    /// The model the minter put in the token; it decides which price table applies (SPEC §2.6).
    pub model: String,
    pub session: Box<dyn Session>,
    pub mic: MicProducer,
    pub pcm: PcmConsumer,
}

impl std::fmt::Debug for Connected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Connected")
    }
}

/// Mints a token and opens a session. Blocking; called on a worker thread.
pub trait Connector: Send + Sync + 'static {
    fn connect(&self, events: EventSink) -> Result<Connected, ConnectError>;
}

/// Capture/playback. `attach` is called for every (re)connect with fresh rings.
pub trait AudioBackend: Send + 'static {
    fn attach(&mut self, mic: MicProducer, pcm: PcmConsumer) -> Result<(), String>;
    /// Re-open the streams on the current device (route change, disconnect).
    fn restart(&mut self) -> Result<(), String>;
    /// Set by a stream error callback; polled by the supervisor.
    fn needs_restart(&self) -> bool {
        false
    }
    fn detach(&mut self);
}

pub type StateFn = Box<dyn Fn(CallState, Option<&str>) + Send>;
/// Called with the running cost of the call in micro-USD, after each priced `response.done`.
pub type CostFn = Box<dyn Fn(u64) + Send>;

#[derive(Debug, Clone)]
pub struct CallConfig {
    pub max_session: Duration,
    /// Waits after the 1st, 2nd, 3rd failed reconnect attempt (SPEC §2.4: 0.5 s / 1 s / 2 s).
    pub backoff: [Duration; 3],
    pub max_reconnect_attempts: usize,
    /// Housekeeping period (mute sync, levels, timers).
    pub tick: Duration,
}

impl Default for CallConfig {
    fn default() -> Self {
        Self {
            max_session: Duration::from_secs(30 * 60),
            backoff: [
                Duration::from_millis(500),
                Duration::from_secs(1),
                Duration::from_secs(2),
            ],
            max_reconnect_attempts: 3,
            tick: Duration::from_millis(50),
        }
    }
}

enum Cmd {
    Hangup,
    NetworkChanged,
    RouteChanged,
}

enum Msg {
    Cmd(Cmd),
    Event {
        gen: u64,
        ev: SessionEvent,
    },
    Connected {
        gen: u64,
        result: Result<Connected, ConnectError>,
    },
}

#[derive(Debug)]
pub struct CallHandle {
    tx: Sender<Msg>,
    join: Option<JoinHandle<()>>,
}

impl CallHandle {
    pub fn start(
        cfg: CallConfig,
        connector: Arc<dyn Connector>,
        audio: Box<dyn AudioBackend>,
        shared: Arc<Shared>,
        on_state: StateFn,
        on_cost: CostFn,
    ) -> std::io::Result<Self> {
        let (tx, rx) = channel();
        let sup = Supervisor {
            cfg,
            connector,
            audio,
            shared,
            on_state,
            on_cost,
            model: None,
            cost_nano: 0,
            tx: tx.clone(),
            rx,
            gen: 0,
            session: None,
            media_up: false,
            channel_open: false,
            greeted: false,
            state: CallState::Idle,
            attempt: 0,
            max_attempts: 1,
            retry_at: None,
            audio_failures: 0,
            last_audio_restart: None,
            started: Instant::now(),
        };
        let join = std::thread::Builder::new()
            .name("lexi-call".into())
            .spawn(move || sup.run())?;
        Ok(Self {
            tx,
            join: Some(join),
        })
    }

    pub fn hangup(&self) {
        let _ = self.tx.send(Msg::Cmd(Cmd::Hangup));
    }
    pub fn network_changed(&self) {
        let _ = self.tx.send(Msg::Cmd(Cmd::NetworkChanged));
    }
    pub fn route_changed(&self) {
        let _ = self.tx.send(Msg::Cmd(Cmd::RouteChanged));
    }

    /// Wait for the supervisor to finish (after `hangup`). Tests and orderly shutdown.
    pub fn join(mut self) {
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for CallHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Cmd(Cmd::Hangup));
    }
}

struct Supervisor {
    cfg: CallConfig,
    connector: Arc<dyn Connector>,
    audio: Box<dyn AudioBackend>,
    shared: Arc<Shared>,
    on_state: StateFn,
    on_cost: CostFn,
    /// Model of the current session, for pricing.
    model: Option<String>,
    /// Total cost of this call so far, nano-USD, kept across reconnects (billing continues too).
    cost_nano: u64,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    /// Generation of the current connect attempt/session; stale events and results are ignored.
    gen: u64,
    session: Option<Box<dyn Session>>,
    media_up: bool,
    /// The `oai-events` channel of the current attempt is open.
    channel_open: bool,
    /// The opening `response.create` was sent: once per call, never again after a reconnect.
    greeted: bool,
    state: CallState,
    attempt: usize,
    max_attempts: usize,
    retry_at: Option<Instant>,
    audio_failures: u32,
    last_audio_restart: Option<Instant>,
    started: Instant,
}

impl Supervisor {
    fn set_state(&mut self, s: CallState, detail: Option<&str>) {
        self.state = s;
        self.shared.set_call_state(s);
        (self.on_state)(s, detail);
    }

    fn run(mut self) {
        self.started = Instant::now();
        self.set_state(CallState::Connecting, None);
        self.max_attempts = 1;
        self.attempt = 0;
        self.spawn_connect();
        loop {
            match self.rx.recv_timeout(self.cfg.tick) {
                Ok(Msg::Cmd(Cmd::Hangup)) => return self.finish(CallState::Ended, "hangup"),
                Ok(Msg::Cmd(Cmd::NetworkChanged)) => self.reconnect("network changed"),
                Ok(Msg::Cmd(Cmd::RouteChanged)) => {
                    if self.session.is_some() {
                        self.restart_audio();
                    }
                }
                Ok(Msg::Event { gen, ev }) if gen == self.gen => {
                    if let Some((s, d)) = self.on_event(ev) {
                        return self.finish(s, d);
                    }
                }
                Ok(Msg::Event { .. }) => {}
                Ok(Msg::Connected { gen, result }) => {
                    if let Some((s, d)) = self.on_connected(gen, result) {
                        return self.finish(s, d);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return self.finish(CallState::Ended, "hangup")
                }
            }
            if let Some((s, d)) = self.housekeeping() {
                return self.finish(s, d);
            }
        }
    }

    fn spawn_connect(&mut self) {
        self.gen += 1;
        self.media_up = false;
        self.channel_open = false;
        self.retry_at = None;
        let gen = self.gen;
        let (connector, tx) = (Arc::clone(&self.connector), self.tx.clone());
        let ev_tx = self.tx.clone();
        let sink: EventSink = Arc::new(move |ev| {
            let _ = ev_tx.send(Msg::Event { gen, ev });
        });
        let spawned = std::thread::Builder::new()
            .name("lexi-connect".into())
            .spawn(move || {
                let result = connector.connect(sink);
                let _ = tx.send(Msg::Connected { gen, result });
            });
        if let Err(e) = spawned {
            log::error!("cannot spawn connect worker: {e}");
            let _ = self.tx.send(Msg::Connected {
                gen,
                result: Err(ConnectError::Other),
            });
        }
    }

    fn drop_session(&mut self) {
        if let Some(s) = self.session.take() {
            s.close();
        }
        self.audio.detach();
    }

    fn reconnect(&mut self, why: &str) {
        log::info!("reconnecting: {why}");
        self.drop_session();
        self.gen += 1; // invalidate the old session's events and any in-flight attempt
        self.set_state(CallState::Reconnecting, None);
        self.attempt = 0;
        self.max_attempts = self.cfg.max_reconnect_attempts.max(1);
        self.spawn_connect();
    }

    fn on_event(&mut self, ev: SessionEvent) -> Option<(CallState, &'static str)> {
        match ev {
            SessionEvent::Connected => {
                self.media_up = true;
                self.maybe_live();
            }
            SessionEvent::ChannelOpen => {
                self.channel_open = true;
                self.maybe_greet();
            }
            SessionEvent::Closed(reason) => {
                log::info!("session closed: {reason}");
                if reason == "ice disconnected" {
                    self.reconnect("ice disconnected");
                } else {
                    return Some((CallState::Ended, "remote"));
                }
            }
            SessionEvent::Server(ServerEvent::ResponseDone { response }) => {
                if let Some(u) = response.usage {
                    log::info!(
                        "usage: total={} in={} out={}",
                        u.total_tokens,
                        u.input_tokens,
                        u.output_tokens
                    );
                    // Unknown model: no price, so no readout (better than a wrong one).
                    let priced = self
                        .model
                        .as_deref()
                        .and_then(|m| pricing::cost_nano_usd(m, &u));
                    if let Some(nano) = priced {
                        self.cost_nano += nano;
                        let total = pricing::to_micro_usd(self.cost_nano);
                        log::info!("cost: this response {nano} nUSD, call total {total} uUSD");
                        (self.on_cost)(total);
                    }
                }
            }
            SessionEvent::Server(ServerEvent::Error { error }) => {
                // Never log the message body; type/code are enough to diagnose.
                log::warn!(
                    "server error: type={} code={}",
                    error.kind.as_deref().unwrap_or("-"),
                    error.code.as_deref().unwrap_or("-")
                );
            }
            SessionEvent::Server(_) => {}
        }
        None
    }

    fn on_connected(
        &mut self,
        gen: u64,
        result: Result<Connected, ConnectError>,
    ) -> Option<(CallState, &'static str)> {
        if gen != self.gen {
            // Superseded (hang-up/reconnect while connecting): discard the stale session.
            if let Ok(c) = result {
                c.session.close();
            }
            return None;
        }
        match result {
            Ok(c) => {
                self.attempt = 0;
                self.audio_failures = 0;
                if let Err(e) = self.audio.attach(c.mic, c.pcm) {
                    log::error!("audio attach failed: {e}");
                    c.session.close();
                    return Some((CallState::Error, "audio"));
                }
                self.model = Some(c.model);
                self.session = Some(c.session);
                self.maybe_live();
                self.maybe_greet();
                None
            }
            Err(e) => {
                self.attempt += 1;
                log::warn!(
                    "connect failed ({}), attempt {}/{}",
                    e.code(),
                    self.attempt,
                    self.max_attempts
                );
                let retryable = matches!(
                    e,
                    ConnectError::Network | ConnectError::Service | ConnectError::Other
                );
                if retryable && self.attempt < self.max_attempts {
                    let wait = self.cfg.backoff[(self.attempt - 1).min(2)];
                    self.retry_at = Some(Instant::now() + wait);
                    None
                } else {
                    Some((CallState::Error, e.code()))
                }
            }
        }
    }

    /// Make the model speak first. The prompt tells it to greet; it only does so when asked to respond,
    /// because nothing the user says has arrived yet. Costs a few audio tokens per call.
    fn maybe_greet(&mut self) {
        if self.greeted || !self.channel_open {
            return;
        }
        if let Some(s) = &self.session {
            s.send_event(r#"{"type":"response.create"}"#.to_owned());
            self.greeted = true;
        }
    }

    fn maybe_live(&mut self) {
        if self.session.is_some() && self.media_up && self.state != CallState::Live {
            log::info!("live after {} ms", self.started.elapsed().as_millis());
            self.set_state(CallState::Live, None);
        }
    }

    fn restart_audio(&mut self) {
        self.last_audio_restart = Some(Instant::now());
        match self.audio.restart() {
            Ok(()) => self.audio_failures = 0,
            Err(e) => {
                self.audio_failures += 1;
                log::warn!("audio restart failed ({}): {e}", self.audio_failures);
            }
        }
    }

    fn housekeeping(&mut self) -> Option<(CallState, &'static str)> {
        let now = Instant::now();
        if self.retry_at.is_some_and(|t| now >= t) {
            self.spawn_connect();
        }
        if let Some(s) = &self.session {
            s.set_muted(self.shared.muted());
            let (mic, bot) = s.levels();
            self.shared.set_mic_level(mic);
            self.shared.set_bot_level(bot);
            let cooled = self
                .last_audio_restart
                .is_none_or(|t| now.duration_since(t) > Duration::from_millis(500));
            if cooled && self.audio.needs_restart() {
                self.restart_audio();
            }
            if self.audio_failures >= 3 {
                return Some((CallState::Error, "audio"));
            }
        }
        if now.duration_since(self.started) >= self.cfg.max_session {
            return Some((CallState::Ended, "max-session"));
        }
        None
    }

    fn finish(mut self, state: CallState, detail: &str) {
        self.gen += 1; // late connect results close themselves
        self.drop_session();
        self.shared.set_mic_level(0.0);
        self.shared.set_bot_level(0.0);
        self.set_state(state, Some(detail));
    }
}

// ----- real implementations -------------------------------------------------------------------

struct RealSession(RtcSession);

impl Session for RealSession {
    fn send_event(&self, json: String) {
        self.0.send_event(json);
    }
    fn set_muted(&self, muted: bool) {
        self.0.set_muted(muted);
    }
    fn levels(&self) -> (f32, f32) {
        (self.0.levels.mic(), self.0.levels.bot())
    }
    fn close(self: Box<Self>) {
        self.0.close();
    }
}

struct Forward(EventSink);

impl Handler for Forward {
    fn on_state(&mut self, state: RtcState) {
        (self.0)(match state {
            RtcState::Connected => SessionEvent::Connected,
            RtcState::ChannelOpen => SessionEvent::ChannelOpen,
            RtcState::Closed(r) => SessionEvent::Closed(r),
        });
    }
    fn on_event(&mut self, event: ServerEvent) {
        (self.0)(SessionEvent::Server(event));
    }
}

/// Mint from our minter (with the attestation header when a key is configured), then open the
/// Realtime WebRTC session. A fresh token and timestamp are used for every attempt.
#[derive(Debug)]
pub struct RealConnector {
    pub minter_url: String,
    pub install_hash: String,
    pub attest_key: Option<Secret>,
    /// Requested voice (`None` = the minter's default).
    pub voice: Option<String>,
    pub rtc: RtcConfig,
}

fn classify(e: &OpenAiError) -> ConnectError {
    match e {
        OpenAiError::Network(_) => ConnectError::Network,
        OpenAiError::Status(401 | 403) => ConnectError::Auth,
        OpenAiError::Status(429) => ConnectError::RateLimited,
        OpenAiError::Status(s) if *s >= 500 => ConnectError::Service,
        OpenAiError::Status(_) => ConnectError::Other,
        OpenAiError::BadResponse(_) => ConnectError::Service,
    }
}

impl Connector for RealConnector {
    fn connect(&self, events: EventSink) -> Result<Connected, ConnectError> {
        let t0 = Instant::now();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let attest = self
            .attest_key
            .as_ref()
            .and_then(|k| attest::header(k, now));
        let minted = openai::mint_from_minter(
            &self.minter_url,
            &self.install_hash,
            attest.as_deref(),
            self.voice.as_deref(),
        )
        .map_err(|e| {
            log::warn!("token request failed: {e}");
            classify(&e)
        })?;
        let t_token = t0.elapsed();
        let (session, mic, pcm) =
            RtcSession::start(self.rtc.clone(), &minted.token, Forward(events)).map_err(|e| {
                log::warn!("session start failed: {e}");
                ConnectError::Network
            })?;
        log::info!(
            "connect timing: token {} ms, offer + sdp exchange {} ms",
            t_token.as_millis(),
            (t0.elapsed() - t_token).as_millis()
        );
        Ok(Connected {
            model: minted.model,
            session: Box::new(RealSession(session)),
            mic,
            pcm,
        })
    }
}

#[cfg(test)]
mod tests;

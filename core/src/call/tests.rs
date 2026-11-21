use super::*;
use crate::jitter::ring::pcm_ring;
use crate::rtc::mic_ring;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

type States = Arc<Mutex<Vec<(CallState, Option<String>)>>>;

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let t = Instant::now();
    while !cond() {
        assert!(
            t.elapsed() < Duration::from_secs(3),
            "timed out waiting for: {what}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[derive(Default)]
struct MockSession {
    muted: Arc<AtomicBool>,
    closed: Arc<AtomicUsize>,
    sent: Arc<Mutex<Vec<String>>>,
}

impl Session for MockSession {
    fn send_event(&self, json: String) {
        self.sent.lock().unwrap().push(json);
    }
    fn set_muted(&self, m: bool) {
        self.muted.store(m, Ordering::SeqCst);
    }
    fn levels(&self) -> (f32, f32) {
        (0.25, 0.5)
    }
    fn close(self: Box<Self>) {
        self.closed.fetch_add(1, Ordering::SeqCst);
    }
}

/// Scripted connector: each call pops the next outcome; `Ok` emits `Connected` like a real session.
struct MockConnector {
    script: Mutex<Vec<Result<(), ConnectError>>>,
    calls: AtomicUsize,
    delay: Duration,
    muted: Arc<AtomicBool>,
    closed: Arc<AtomicUsize>,
    sent: Arc<Mutex<Vec<String>>>,
    model: String,
    last_sink: Mutex<Option<EventSink>>,
}

impl MockConnector {
    fn new(script: Vec<Result<(), ConnectError>>) -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(script),
            calls: AtomicUsize::new(0),
            delay: Duration::ZERO,
            muted: Arc::default(),
            closed: Arc::default(),
            sent: Arc::default(),
            model: "gpt-realtime-2.1-mini".into(),
            last_sink: Mutex::new(None),
        })
    }
    fn with_delay(script: Vec<Result<(), ConnectError>>, delay: Duration) -> Arc<Self> {
        let mut c = Self::new(script);
        if let Some(m) = Arc::get_mut(&mut c) {
            m.delay = delay;
        }
        c
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
    fn inject(&self, ev: SessionEvent) {
        if let Some(s) = self.last_sink.lock().unwrap().as_ref() {
            s(ev);
        }
    }
}

impl Connector for MockConnector {
    fn connect(&self, events: EventSink) -> Result<Connected, ConnectError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(self.delay);
        let next = {
            let mut s = self.script.lock().unwrap();
            if s.is_empty() {
                Ok(())
            } else {
                s.remove(0)
            }
        };
        next?;
        let (mic, _mic_c) = mic_ring();
        let (_pcm_p, pcm) = pcm_ring(16);
        *self.last_sink.lock().unwrap() = Some(Arc::clone(&events));
        events(SessionEvent::Connected);
        events(SessionEvent::ChannelOpen);
        Ok(Connected {
            model: self.model.clone(),
            session: Box::new(MockSession {
                muted: Arc::clone(&self.muted),
                closed: Arc::clone(&self.closed),
                sent: Arc::clone(&self.sent),
            }),
            mic,
            pcm,
        })
    }
}

#[derive(Default, Clone)]
struct AudioCounts {
    attach: Arc<AtomicUsize>,
    detach: Arc<AtomicUsize>,
    restart: Arc<AtomicUsize>,
    flag: Arc<AtomicBool>,
}

struct MockAudio(AudioCounts);

impl AudioBackend for MockAudio {
    fn attach(&mut self, _m: MicProducer, _p: PcmConsumer) -> Result<(), String> {
        self.0.attach.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn restart(&mut self) -> Result<(), String> {
        self.0.restart.fetch_add(1, Ordering::SeqCst);
        self.0.flag.store(false, Ordering::SeqCst);
        Ok(())
    }
    fn needs_restart(&self) -> bool {
        self.0.flag.load(Ordering::SeqCst)
    }
    fn detach(&mut self) {
        self.0.detach.fetch_add(1, Ordering::SeqCst);
    }
}

fn fast_cfg() -> CallConfig {
    CallConfig {
        max_session: Duration::from_secs(60),
        backoff: [Duration::from_millis(10); 3],
        max_reconnect_attempts: 3,
        tick: Duration::from_millis(5),
    }
}

struct Rig {
    costs: Arc<Mutex<Vec<u64>>>,
    handle: CallHandle,
    states: States,
    shared: Arc<Shared>,
    audio: AudioCounts,
}

fn start(connector: Arc<MockConnector>, cfg: CallConfig, shared: Arc<Shared>) -> Rig {
    let states: States = Arc::default();
    let s2 = Arc::clone(&states);
    let costs: Arc<Mutex<Vec<u64>>> = Arc::default();
    let c2 = Arc::clone(&costs);
    let audio = AudioCounts::default();
    let handle = CallHandle::start(
        cfg,
        connector,
        Box::new(MockAudio(audio.clone())),
        Arc::clone(&shared),
        Box::new(move |s, d| s2.lock().unwrap().push((s, d.map(str::to_owned)))),
        Box::new(move |micro| c2.lock().unwrap().push(micro)),
    )
    .unwrap();
    Rig {
        costs,
        handle,
        states,
        shared,
        audio,
    }
}

impl Rig {
    fn last(&self) -> Option<(CallState, Option<String>)> {
        self.states.lock().unwrap().last().cloned()
    }
    fn seq(&self) -> Vec<CallState> {
        self.states
            .lock()
            .unwrap()
            .iter()
            .map(|(s, _)| *s)
            .collect()
    }
    fn wait_state(&self, s: CallState) {
        wait_until(&format!("{s:?}"), || {
            self.last().is_some_and(|(l, _)| l == s)
        });
    }
}

#[test]
fn connects_goes_live_and_hangs_up_cleanly() {
    let c = MockConnector::new(vec![]);
    let rig = start(Arc::clone(&c), fast_cfg(), Arc::default());
    rig.wait_state(CallState::Live);
    assert_eq!(rig.seq(), vec![CallState::Connecting, CallState::Live]);
    wait_until("levels published", || rig.shared.bot_level() > 0.4);
    assert!((rig.shared.mic_level() - 0.25).abs() < 1e-6);
    rig.handle.hangup();
    rig.wait_state(CallState::Ended);
    assert_eq!(rig.last().unwrap().1.as_deref(), Some("hangup"));
    assert_eq!(c.closed.load(Ordering::SeqCst), 1);
    assert_eq!(rig.audio.attach.load(Ordering::SeqCst), 1);
    assert!(rig.audio.detach.load(Ordering::SeqCst) >= 1);
    assert_eq!(rig.shared.bot_level(), 0.0);
}

#[test]
fn mute_set_before_the_call_applies_as_soon_as_the_session_exists() {
    let shared = Arc::new(Shared::default());
    shared.set_muted(true);
    let c = MockConnector::new(vec![]);
    let rig = start(Arc::clone(&c), fast_cfg(), shared);
    wait_until("muted before any audio flows", || {
        c.muted.load(Ordering::SeqCst)
    });
    // and it follows later toggles
    rig.shared.set_muted(false);
    wait_until("unmuted", || !c.muted.load(Ordering::SeqCst));
    rig.handle.hangup();
    rig.wait_state(CallState::Ended);
}

#[test]
fn network_change_reconnects_with_a_new_session() {
    let c = MockConnector::new(vec![]);
    let rig = start(Arc::clone(&c), fast_cfg(), Arc::default());
    rig.wait_state(CallState::Live);
    rig.handle.network_changed();
    wait_until("second connect", || c.calls() == 2);
    wait_until("live again", || {
        rig.seq()
            == vec![
                CallState::Connecting,
                CallState::Live,
                CallState::Reconnecting,
                CallState::Live,
            ]
    });
    assert_eq!(c.closed.load(Ordering::SeqCst), 1, "old session closed");
    assert_eq!(rig.audio.attach.load(Ordering::SeqCst), 2);
    rig.handle.hangup();
    rig.wait_state(CallState::Ended);
}

#[test]
fn reconnect_retries_then_succeeds() {
    let c = MockConnector::new(vec![
        Ok(()),
        Err(ConnectError::Network),
        Err(ConnectError::Network),
    ]);
    let rig = start(Arc::clone(&c), fast_cfg(), Arc::default());
    rig.wait_state(CallState::Live);
    rig.handle.network_changed();
    wait_until("third reconnect attempt", || c.calls() == 4);
    wait_until("live again", || {
        rig.last().is_some_and(|(s, _)| s == CallState::Live)
            && rig.seq().contains(&CallState::Reconnecting)
    });
    rig.handle.hangup();
    rig.wait_state(CallState::Ended);
}

#[test]
fn reconnect_gives_up_after_three_attempts() {
    let c = MockConnector::new(vec![
        Ok(()),
        Err(ConnectError::Network),
        Err(ConnectError::Network),
        Err(ConnectError::Network),
    ]);
    let rig = start(Arc::clone(&c), fast_cfg(), Arc::default());
    rig.wait_state(CallState::Live);
    rig.handle.network_changed();
    rig.wait_state(CallState::Error);
    assert_eq!(rig.last().unwrap().1.as_deref(), Some("network"));
    assert_eq!(c.calls(), 4, "1 initial + exactly 3 reconnect attempts");
}

#[test]
fn initial_connect_failure_is_a_single_attempt_with_a_code() {
    let c = MockConnector::new(vec![Err(ConnectError::Auth)]);
    let rig = start(Arc::clone(&c), fast_cfg(), Arc::default());
    rig.wait_state(CallState::Error);
    assert_eq!(rig.last().unwrap().1.as_deref(), Some("auth"));
    assert_eq!(c.calls(), 1);
    assert_eq!(rig.audio.attach.load(Ordering::SeqCst), 0);
}

#[test]
fn rate_limit_is_not_retried_on_reconnect() {
    let c = MockConnector::new(vec![Ok(()), Err(ConnectError::RateLimited)]);
    let rig = start(Arc::clone(&c), fast_cfg(), Arc::default());
    rig.wait_state(CallState::Live);
    rig.handle.network_changed();
    rig.wait_state(CallState::Error);
    assert_eq!(rig.last().unwrap().1.as_deref(), Some("rate"));
    assert_eq!(c.calls(), 2);
}

#[test]
fn max_session_guard_ends_the_call() {
    let cfg = CallConfig {
        max_session: Duration::from_millis(150),
        ..fast_cfg()
    };
    let c = MockConnector::new(vec![]);
    let rig = start(Arc::clone(&c), cfg, Arc::default());
    rig.wait_state(CallState::Ended);
    assert_eq!(rig.last().unwrap().1.as_deref(), Some("max-session"));
    assert_eq!(c.closed.load(Ordering::SeqCst), 1);
}

#[test]
fn ice_disconnect_reconnects_but_other_closes_end_the_call() {
    let c = MockConnector::new(vec![]);
    let rig = start(Arc::clone(&c), fast_cfg(), Arc::default());
    rig.wait_state(CallState::Live);
    c.inject(SessionEvent::Closed("ice disconnected".into()));
    wait_until("reconnected", || {
        c.calls() == 2
            && rig.last().is_some_and(|(s, _)| s == CallState::Live)
            && rig.seq().len() == 4
    });
    c.inject(SessionEvent::Closed("data channel closed".into()));
    rig.wait_state(CallState::Ended);
    assert_eq!(rig.last().unwrap().1.as_deref(), Some("remote"));
}

#[test]
fn events_from_a_superseded_session_are_ignored() {
    let c = MockConnector::new(vec![]);
    let rig = start(Arc::clone(&c), fast_cfg(), Arc::default());
    rig.wait_state(CallState::Live);
    let old = c.last_sink.lock().unwrap().clone().unwrap();
    rig.handle.network_changed();
    wait_until("reconnected", || rig.seq().len() == 4);
    old(SessionEvent::Closed("data channel closed".into())); // late event from the closed session
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(rig.last().unwrap().0, CallState::Live);
    rig.handle.hangup();
    rig.wait_state(CallState::Ended);
}

#[test]
fn audio_error_flag_and_route_changes_restart_the_streams() {
    let c = MockConnector::new(vec![]);
    let rig = start(Arc::clone(&c), fast_cfg(), Arc::default());
    rig.wait_state(CallState::Live);
    rig.audio.flag.store(true, Ordering::SeqCst); // what an AAudio error callback does
    wait_until("restart from error flag", || {
        rig.audio.restart.load(Ordering::SeqCst) == 1
    });
    rig.handle.route_changed();
    wait_until("restart from route change", || {
        rig.audio.restart.load(Ordering::SeqCst) == 2
    });
    rig.handle.hangup();
    rig.wait_state(CallState::Ended);
}

#[test]
fn hangup_while_connecting_discards_the_late_session() {
    let c = MockConnector::with_delay(vec![], Duration::from_millis(150));
    let rig = start(Arc::clone(&c), fast_cfg(), Arc::default());
    wait_until("connect started", || c.calls() == 1);
    rig.handle.hangup();
    rig.wait_state(CallState::Ended);
    assert_eq!(rig.seq(), vec![CallState::Connecting, CallState::Ended]);
    assert_eq!(
        rig.audio.attach.load(Ordering::SeqCst),
        0,
        "never attached audio"
    );
}

#[test]
fn network_change_during_a_slow_connect_closes_the_superseded_session() {
    let c = MockConnector::with_delay(vec![], Duration::from_millis(120));
    let rig = start(Arc::clone(&c), fast_cfg(), Arc::default());
    wait_until("first connect started", || c.calls() == 1);
    rig.handle.network_changed(); // supersedes the in-flight attempt
    wait_until("second connect", || c.calls() == 2);
    wait_until("live", || {
        rig.last().is_some_and(|(s, _)| s == CallState::Live)
    });
    // the first attempt finished late: its session must have been closed, not attached
    wait_until("stale session closed", || {
        c.closed.load(Ordering::SeqCst) == 1
    });
    assert_eq!(rig.audio.attach.load(Ordering::SeqCst), 1);
    rig.handle.hangup();
    rig.wait_state(CallState::Ended);
    assert_eq!(c.closed.load(Ordering::SeqCst), 2);
}

#[test]
fn greets_once_when_the_channel_opens_and_not_again_after_a_reconnect() {
    let c = MockConnector::new(vec![]);
    let rig = start(Arc::clone(&c), fast_cfg(), Arc::default());
    rig.wait_state(CallState::Live);
    wait_until("greeting sent", || !c.sent.lock().unwrap().is_empty());
    assert_eq!(
        c.sent.lock().unwrap().as_slice(),
        [r#"{"type":"response.create"}"#]
    );
    rig.handle.network_changed();
    wait_until("reconnected", || c.calls() == 2 && rig.seq().len() == 4);
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(
        c.sent.lock().unwrap().len(),
        1,
        "no second greeting after a reconnect"
    );
    rig.handle.hangup();
    rig.wait_state(CallState::Ended);
}

fn response_done(usage: &str) -> SessionEvent {
    SessionEvent::Server(
        crate::openai::parse_event(&format!(
            r#"{{"type":"response.done","response":{{"status":"completed","usage":{usage}}}}}"#
        ))
        .unwrap(),
    )
}

/// OpenAI's documented example: 2058.84 micro-USD, shown rounded as 2059.
const DOCUMENTED_USAGE: &str = r#"{"total_tokens":253,"input_tokens":132,"output_tokens":121,
  "input_token_details":{"text_tokens":119,"audio_tokens":13,"image_tokens":0,"cached_tokens":64,
  "cached_tokens_details":{"text_tokens":64,"audio_tokens":0,"image_tokens":0}},
  "output_token_details":{"text_tokens":30,"audio_tokens":91}}"#;

#[test]
fn cost_accumulates_per_response_and_survives_a_reconnect() {
    let c = MockConnector::new(vec![]);
    let rig = start(Arc::clone(&c), fast_cfg(), Arc::default());
    rig.wait_state(CallState::Live);
    assert!(
        rig.costs.lock().unwrap().is_empty(),
        "nothing is shown before the first response"
    );
    c.inject(response_done(DOCUMENTED_USAGE));
    wait_until("first cost", || rig.costs.lock().unwrap().len() == 1);
    assert_eq!(rig.costs.lock().unwrap()[0], 2059);

    rig.handle.network_changed();
    wait_until("reconnected", || c.calls() == 2 && rig.seq().len() == 4);
    c.inject(response_done(DOCUMENTED_USAGE));
    wait_until("second cost", || rig.costs.lock().unwrap().len() == 2);
    // 2 x 2058.84 = 4117.68 micro-USD: the total keeps growing across the reconnect, rounded once.
    assert_eq!(rig.costs.lock().unwrap()[1], 4118);
    rig.handle.hangup();
    rig.wait_state(CallState::Ended);
}

#[test]
fn unknown_model_and_missing_usage_report_no_cost() {
    let mut c = MockConnector::new(vec![]);
    if let Some(m) = Arc::get_mut(&mut c) {
        m.model = "some-future-model".into();
    }
    let rig = start(Arc::clone(&c), fast_cfg(), Arc::default());
    rig.wait_state(CallState::Live);
    c.inject(response_done(DOCUMENTED_USAGE));
    c.inject(SessionEvent::Server(
        crate::openai::parse_event(r#"{"type":"response.done","response":{"status":"cancelled"}}"#)
            .unwrap(),
    ));
    std::thread::sleep(Duration::from_millis(80));
    assert!(rig.costs.lock().unwrap().is_empty());
    rig.handle.hangup();
    rig.wait_state(CallState::Ended);
}

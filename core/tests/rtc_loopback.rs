//! Full ICE + DTLS + RTP + data-channel round trip against an in-process str0m peer
//! (standing in for OpenAI), over loopback. No network or API key needed.
#![allow(clippy::unwrap_used)]

use lexi_core::codec::{Encoded, OpusEncoder, FRAME_SAMPLES};
use lexi_core::openai::ServerEvent;
use lexi_core::rtc::{Handler, RtcConfig, RtcSession, RtcState};
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use str0m::change::SdpOffer;
use str0m::media::{Frequency, MediaTime};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc};

#[derive(Default, Debug)]
struct PeerLog {
    rx_audio_ts: Vec<u64>,
    client_events: Vec<String>,
    connected: bool,
    channel_label: Option<String>,
}

#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<String>>>);

struct TestHandler(Log);
impl Handler for TestHandler {
    fn on_state(&mut self, s: RtcState) {
        self.0 .0.lock().unwrap().push(format!("state:{s:?}"));
    }
    fn on_event(&mut self, ev: ServerEvent) {
        self.0 .0.lock().unwrap().push(format!("event:{ev:?}"));
    }
}

fn tone_frame(f: usize) -> Vec<i16> {
    (0..FRAME_SAMPLES)
        .map(|i| {
            let t = (f * FRAME_SAMPLES + i) as f32 / 48000.0;
            (8000.0 * (2.0 * std::f32::consts::PI * 440.0 * t).sin()) as i16
        })
        .collect()
}

/// The fake "OpenAI": answers the offer, echoes Opus audio of its own, and sends events.
fn spawn_peer(
    offer_sdp: &str,
    log: Arc<Mutex<PeerLog>>,
    stop: Arc<AtomicBool>,
) -> (String, std::thread::JoinHandle<()>) {
    let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = sock.local_addr().unwrap();
    let mut rtc = Rtc::builder()
        .clear_codecs()
        .enable_opus(true, false)
        .build(Instant::now());
    rtc.add_local_candidate(Candidate::host(addr, "udp").unwrap());
    let answer = rtc
        .sdp_api()
        .accept_offer(SdpOffer::from_sdp_string(offer_sdp).unwrap())
        .unwrap()
        .to_sdp_string();
    let h = std::thread::spawn(move || {
        let mut enc = OpusEncoder::new().unwrap();
        let mut mid = None;
        let mut channel = None;
        let mut frames_sent = 0usize;
        let mut next_tick = Instant::now();
        let mut sent_speech_started = false;
        let mut buf = vec![0u8; 2048];
        while !stop.load(Ordering::Relaxed) {
            let deadline = loop {
                match rtc.poll_output().unwrap() {
                    Output::Timeout(t) => break t,
                    Output::Transmit(t) => {
                        let _ = sock.send_to(&t.contents, t.destination);
                    }
                    Output::Event(ev) => match ev {
                        Event::Connected => log.lock().unwrap().connected = true,
                        Event::MediaAdded(m) => mid = Some(m.mid),
                        Event::MediaData(d) => log.lock().unwrap().rx_audio_ts.push(d.time.numer()),
                        Event::ChannelOpen(id, label) => {
                            log.lock().unwrap().channel_label = Some(label);
                            channel = Some(id);
                        }
                        Event::ChannelData(d) => {
                            log.lock()
                                .unwrap()
                                .client_events
                                .push(String::from_utf8_lossy(&d.data).into_owned());
                        }
                        Event::IceConnectionStateChange(IceConnectionState::Disconnected) => return,
                        _ => {}
                    },
                }
            };
            let now = Instant::now();
            // Mutation 1 (at most one per iteration): greet, send audio, or announce speech.
            let connected = log.lock().unwrap().connected;
            if connected && now >= next_tick {
                if let (Some(mid), Some(ch)) = (mid, channel) {
                    if frames_sent == 0 {
                        rtc.channel(ch)
                            .unwrap()
                            .write(false, br#"{"type":"session.created","session":{}}"#)
                            .unwrap();
                        frames_sent += 1;
                        continue;
                    }
                    if frames_sent <= 60 {
                        if let Ok(Encoded::Packet { data, .. }) =
                            enc.encode(&tone_frame(frames_sent))
                        {
                            let w = rtc.writer(mid).unwrap();
                            let pt = w.payload_params().next().unwrap().pt();
                            let ts = frames_sent as u64 * 960;
                            w.write(
                                pt,
                                now,
                                MediaTime::new(ts, Frequency::FORTY_EIGHT_KHZ),
                                data,
                            )
                            .unwrap();
                        }
                        frames_sent += 1;
                        next_tick += Duration::from_millis(20);
                        continue;
                    }
                    if !sent_speech_started {
                        sent_speech_started = true;
                        rtc.channel(ch)
                            .unwrap()
                            .write(false, br#"{"type":"input_audio_buffer.speech_started","audio_start_ms":5}"#)
                            .unwrap();
                        continue;
                    }
                }
            }
            sock.set_read_timeout(Some(
                deadline
                    .min(next_tick)
                    .saturating_duration_since(Instant::now())
                    .max(Duration::from_millis(1)),
            ))
            .unwrap();
            let input = match sock.recv_from(&mut buf) {
                Ok((n, source)) => Input::Receive(
                    Instant::now(),
                    Receive {
                        proto: Protocol::Udp,
                        source,
                        destination: addr,
                        contents: buf[..n].try_into().unwrap(),
                    },
                ),
                Err(_) => {
                    if Instant::now() < deadline {
                        continue;
                    }
                    Input::Timeout(Instant::now())
                }
            };
            if rtc.handle_input(input).is_err() {
                return;
            }
        }
    });
    (answer, h)
}

#[test]
fn full_session_over_loopback() {
    let peer_log = Arc::new(Mutex::new(PeerLog::default()));
    let stop = Arc::new(AtomicBool::new(false));
    let mut peer = None;
    let log = Log::default();

    let mut sig = |offer: &str| {
        assert!(offer.contains("opus/48000"), "offer must carry Opus");
        assert!(!offer.to_lowercase().contains("pcmu"), "Opus only");
        assert!(
            offer.contains("m=application"),
            "offer must carry a data channel"
        );
        let (answer, h) = spawn_peer(offer, Arc::clone(&peer_log), Arc::clone(&stop));
        peer = Some(h);
        Ok(answer)
    };
    let cfg = RtcConfig {
        include_loopback: true,
        ..RtcConfig::default()
    };
    let (session, mut mic, mut pcm) =
        RtcSession::start_with_signaling(cfg, &mut sig, TestHandler(log.clone())).unwrap();
    session.send_event(r#"{"type":"response.create"}"#.into()); // queued until the channel opens

    // Simulated audio callbacks: mic every 20 ms, speaker pull every 10 ms.
    let start = Instant::now();
    let mut f = 0usize;
    let mut loud_samples = 0usize;
    let mut out = [0i16; 480];
    while start.elapsed() < Duration::from_millis(2600) {
        if start.elapsed() >= Duration::from_millis(20 * f as u64) {
            mic.push(&tone_frame(f));
            f += 1;
        }
        let n = pcm.pull(&mut out);
        loud_samples += out[..n].iter().filter(|s| s.abs() > 500).count();
        std::thread::sleep(Duration::from_millis(5));
    }
    let bot_level = session.levels.bot();
    let mic_level = session.levels.mic();
    session.close();
    stop.store(true, Ordering::Relaxed);
    peer.unwrap().join().unwrap();

    let events = log.0.lock().unwrap().clone();
    let p = peer_log.lock().unwrap();
    assert_eq!(p.channel_label.as_deref(), Some("oai-events"));
    assert!(p.connected, "peer never connected; handler log: {events:?}");
    assert!(
        events.contains(&"state:Connected".to_string()),
        "{events:?}"
    );
    assert!(
        events.contains(&"state:ChannelOpen".to_string()),
        "{events:?}"
    );
    assert!(
        events.iter().any(|e| e.contains("SessionCreated")),
        "{events:?}"
    );
    assert!(
        events.iter().any(|e| e.contains("SpeechStarted")),
        "{events:?}"
    );
    assert!(
        events.last().unwrap().starts_with("state:Closed"),
        "{events:?}"
    );

    // Client → peer: queued data-channel message delivered, RTP timestamps +960 per frame.
    assert!(
        p.client_events
            .iter()
            .any(|e| e.contains("response.create")),
        "{:?}",
        p.client_events
    );
    assert!(
        p.rx_audio_ts.len() >= 40,
        "peer got only {} audio packets",
        p.rx_audio_ts.len()
    );
    for w in p.rx_audio_ts.windows(2) {
        assert_eq!(
            w[1] - w[0],
            960,
            "RTP timestamps must advance by 960: {:?}",
            &p.rx_audio_ts[..10]
        );
    }

    // Peer → client: decoded audio reached the playout ring.
    assert!(
        loud_samples > 20 * FRAME_SAMPLES / 2,
        "only {loud_samples} loud samples played"
    );
    assert!(
        bot_level > 0.0 && mic_level > 0.0,
        "levels bot={bot_level} mic={mic_level}"
    );
}

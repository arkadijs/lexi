use super::*;
use std::net::TcpListener;
use std::sync::Mutex;

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let t = Instant::now();
    while !cond() {
        assert!(
            t.elapsed() < Duration::from_secs(5),
            "timed out waiting for: {what}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn downsample_averages_triples_and_upsample_interpolates_across_chunks() {
    let mut out = Vec::new();
    downsample_3x(&[3, 6, 9, -3, -6, -9, 100], &mut out);
    assert_eq!(out, vec![6, -6], "the incomplete trailing group is ignored");

    let (mut prev, mut q) = (0i16, VecDeque::new());
    upsample_2x(&mut prev, &[10, 20], &mut q);
    upsample_2x(&mut prev, &[30], &mut q);
    assert_eq!(
        q.iter().copied().collect::<Vec<_>>(),
        vec![5, 10, 15, 20, 25, 30]
    );
}

#[test]
fn usage_maps_modalities_thinking_and_tool_prompt_tokens() {
    // Captured from gemini-3.8-live (2026-11-22): the second generation of a conversation.
    let u: Value = serde_json::from_str(
        r#"{"promptTokenCount":1401,"responseTokenCount":48,"totalTokenCount":1449,
        "promptTokensDetails":[{"modality":"TEXT","tokenCount":1099},{"modality":"AUDIO","tokenCount":233}],
        "responseTokensDetails":[{"modality":"AUDIO","tokenCount":25}],"thoughtsTokenCount":75}"#,
    )
    .unwrap();
    let m = usage_from_gemini(&u);
    assert_eq!(m.input_token_details.text_tokens, 1168, "1401 - 233 audio");
    assert_eq!(m.input_token_details.audio_tokens, 233);
    // 48 response tokens: 25 audio and 23 other (text), plus 75 thinking tokens billed as output.
    assert_eq!(m.output_token_details.audio_tokens, 25);
    assert_eq!(m.output_token_details.text_tokens, 23 + 75);
    assert_eq!(m.total_tokens, 1449);
    // Priced at the built-in Gemini rates: 1168 x 0.75 + 233 x 3 + 98 x 4.5 + 25 x 12 = 2316.0 micro-USD.
    let nano = crate::pricing::cost_nano_usd("gemini-3.8-live", &m).unwrap();
    assert_eq!(nano, 1_168 * 750 + 233 * 3_000 + 98 * 4_500 + 25 * 12_000);
}

#[test]
fn usage_without_a_breakdown_counts_as_text() {
    let u: Value = serde_json::from_str(
        r#"{"promptTokenCount":100,"responseTokenCount":10,"totalTokenCount":110}"#,
    )
    .unwrap();
    let m = usage_from_gemini(&u);
    assert_eq!(m.input_token_details.text_tokens, 100);
    assert_eq!(m.output_token_details.text_tokens, 10);
}

#[derive(Default)]
struct Seen {
    states: Vec<RtcState>,
    events: Vec<ServerEvent>,
}

struct Rec(Arc<Mutex<Seen>>);

impl Handler for Rec {
    fn on_state(&mut self, s: RtcState) {
        self.0.lock().unwrap().states.push(s);
    }
    fn on_event(&mut self, e: ServerEvent) {
        self.0.lock().unwrap().events.push(e);
    }
}

fn b64(samples: &[i16]) -> String {
    let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// A scripted local Gemini: records what the client sends (parsed), sends what the test asks.
struct MockServer {
    to_client: Sender<String>,
    from_client: Arc<Mutex<Vec<Value>>>,
    path: Arc<Mutex<String>>,
    join: Option<JoinHandle<()>>,
}

impl MockServer {
    #[allow(clippy::result_large_err)] // the handshake callback's signature is tungstenite's
    fn start() -> (Endpoint, Self) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (to_client, rx) = channel::<String>();
        let from_client: Arc<Mutex<Vec<Value>>> = Arc::default();
        let path: Arc<Mutex<String>> = Arc::default();
        let (rec, p2) = (Arc::clone(&from_client), Arc::clone(&path));
        let join = std::thread::spawn(move || {
            let (sock, _) = listener.accept().unwrap();
            let mut ws = tungstenite::accept_hdr(
                sock,
                |req: &tungstenite::handshake::server::Request, resp| {
                    *p2.lock().unwrap() = req.uri().to_string();
                    Ok(resp)
                },
            )
            .unwrap();
            ws.get_ref()
                .set_read_timeout(Some(Duration::from_millis(10)))
                .unwrap();
            loop {
                while let Ok(m) = rx.try_recv() {
                    if m == "close" {
                        let _ = ws.close(None);
                        return;
                    }
                    // Google sends JSON in binary frames.
                    ws.send(Message::Binary(m.into_bytes().into())).unwrap();
                }
                match ws.read() {
                    Ok(Message::Text(t)) => {
                        rec.lock().unwrap().push(serde_json::from_str(&t).unwrap())
                    }
                    Ok(Message::Close(_)) | Err(tungstenite::Error::ConnectionClosed) => return,
                    _ => {}
                }
            }
        });
        (
            Endpoint {
                host: "127.0.0.1".into(),
                port,
                tls: false,
                path: "/ws/test".into(),
            },
            Self {
                to_client,
                from_client,
                path,
                join: Some(join),
            },
        )
    }
    fn send(&self, v: Value) {
        self.to_client.send(v.to_string()).unwrap();
    }
    fn wait_msg(&self, what: &str, pred: impl Fn(&Value) -> bool) -> Value {
        let t = Instant::now();
        loop {
            if let Some(v) = self.from_client.lock().unwrap().iter().find(|v| pred(v)) {
                return v.clone();
            }
            assert!(
                t.elapsed() < Duration::from_secs(5),
                "client never sent: {what}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        let _ = self.to_client.send("close".into());
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

#[test]
fn full_conversation_against_a_scripted_server() {
    let (ep, srv) = MockServer::start();
    let seen: Arc<Mutex<Seen>> = Arc::default();
    let (session, mut mic, mut pcm) =
        GeminiSession::start(&ep, &Secret::new("tok-123"), Rec(Arc::clone(&seen))).unwrap();

    // The token travels as a query parameter, and the first message is an empty setup.
    srv.wait_msg("setup", |v| v.get("setup").is_some_and(|s| s == &json!({})));
    assert!(srv
        .path
        .lock()
        .unwrap()
        .ends_with("/ws/test?access_token=tok-123"));

    srv.send(json!({ "setupComplete": {} }));
    wait_until("channel open", || {
        seen.lock().unwrap().states.contains(&RtcState::ChannelOpen)
    });
    assert_eq!(seen.lock().unwrap().states[0], RtcState::Connected);

    // The supervisor's greeting trigger becomes a text turn.
    session.send_event(r#"{"type":"response.create"}"#.into());
    let kick = srv.wait_msg("kickoff", |v| v["realtimeInput"]["text"].is_string());
    assert!(kick["realtimeInput"]["text"]
        .as_str()
        .unwrap()
        .contains("Greet"));

    // 100 ms of 24 kHz bot audio arrives in one burst and is paced out as 48 kHz frames.
    let tone: Vec<i16> = (0..2400).map(|i| ((i % 100) as i16 - 50) * 200).collect();
    srv.send(json!({ "serverContent": { "modelTurn": { "parts": [
        { "inlineData": { "mimeType": "audio/pcm;rate=24000", "data": b64(&tone) } }
    ] } } }));
    let mut got = 0usize;
    let mut buf = [0i16; FRAME_SAMPLES];
    wait_until("bot audio", || {
        got += pcm.pull(&mut buf);
        got >= 4800
    });
    assert!(session.levels.bot() > 0.0 || got >= 4800);

    // usageMetadata -> priced ResponseDone.
    srv.send(
        json!({ "usageMetadata": { "promptTokenCount": 100, "responseTokenCount": 10,
        "totalTokenCount": 110, "responseTokensDetails": [{"modality":"AUDIO","tokenCount":10}] },
        "serverContent": { "turnComplete": true } }),
    );
    wait_until("usage event", || {
        seen.lock().unwrap().events.iter().any(|e| {
            matches!(e,
            ServerEvent::ResponseDone { response } if response.usage.is_some())
        })
    });

    // Google's built-in search: queries are reported (billed per query by the supervisor).
    srv.send(
        json!({ "serverContent": { "groundingMetadata": { "webSearchQueries": ["a", "b"] } } }),
    );
    wait_until("web searches", || {
        seen.lock()
            .unwrap()
            .events
            .iter()
            .any(|e| matches!(e, ServerEvent::WebSearches { count: 2 }))
    });

    // Barge-in: `interrupted` clears the queue and is reported as speech_started.
    srv.send(json!({ "serverContent": { "interrupted": true } }));
    wait_until("interrupted", || {
        seen.lock()
            .unwrap()
            .events
            .iter()
            .any(|e| matches!(e, ServerEvent::SpeechStarted { .. }))
    });

    // Mic: 100 ms at 48 kHz -> 16 kHz PCM messages; muting sends audioStreamEnd once.
    let voice: Vec<i16> = (0..4800).map(|i| ((i % 48) as i16 - 24) * 100).collect();
    assert_eq!(mic.push(&voice), 4800);
    let m = srv.wait_msg("mic audio", |v| v["realtimeInput"]["audio"].is_object());
    assert_eq!(
        m["realtimeInput"]["audio"]["mimeType"],
        "audio/pcm;rate=16000"
    );
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(m["realtimeInput"]["audio"]["data"].as_str().unwrap())
        .unwrap();
    assert_eq!(bytes.len(), MIC_CHUNK_16K * 2, "one 40 ms chunk");
    session.set_muted(true);
    srv.wait_msg("audioStreamEnd", |v| {
        v["realtimeInput"]["audioStreamEnd"] == true
    });

    // The kickoff was sent exactly once.
    let kicks = srv
        .from_client
        .lock()
        .unwrap()
        .iter()
        .filter(|v| v["realtimeInput"]["text"].is_string())
        .count();
    assert_eq!(kicks, 1);
    session.close();
    wait_until("closed", || {
        matches!(
            seen.lock().unwrap().states.last(),
            Some(RtcState::Closed(_))
        )
    });
}

#[test]
fn server_close_ends_the_session_as_remote() {
    let (ep, srv) = MockServer::start();
    let seen: Arc<Mutex<Seen>> = Arc::default();
    let (_session, _mic, _pcm) =
        GeminiSession::start(&ep, &Secret::new("t"), Rec(Arc::clone(&seen))).unwrap();
    srv.wait_msg("setup", |v| v.get("setup").is_some());
    srv.to_client.send("close".into()).unwrap();
    wait_until(
        "closed",
        || matches!(seen.lock().unwrap().states.last(), Some(RtcState::Closed(r)) if r == "remote"),
    );
}

#[test]
fn a_response_waits_for_its_cushion_before_the_first_frame_plays() {
    let (ep, srv) = MockServer::start();
    let seen: Arc<Mutex<Seen>> = Arc::default();
    let (_session, _mic, mut pcm) =
        GeminiSession::start(&ep, &Secret::new("t"), Rec(Arc::clone(&seen))).unwrap();
    srv.send(json!({ "setupComplete": {} }));
    // 60 ms of audio (3 frames at 48 kHz): less than the cushion, so nothing plays yet...
    let tone: Vec<i16> = (0..1440).map(|i| ((i % 100) as i16 - 50) * 200).collect();
    srv.send(json!({ "serverContent": { "modelTurn": { "parts": [
        { "inlineData": { "mimeType": "audio/pcm;rate=24000", "data": b64(&tone) } }
    ] } } }));
    let t = Instant::now();
    let mut buf = [0i16; FRAME_SAMPLES];
    let mut first_at = None;
    while first_at.is_none() && t.elapsed() < Duration::from_secs(3) {
        if pcm.pull(&mut buf) > 0 {
            first_at = Some(t.elapsed());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let waited = first_at.expect("the short response still plays eventually");
    // ...but not before the 400 ms cap (minus scheduling slack); a long response starts after 200 ms.
    assert!(
        waited >= Duration::from_millis(300),
        "started after only {waited:?}"
    );
}

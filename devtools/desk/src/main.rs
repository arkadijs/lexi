//! Desktop harness for the Lexi WebRTC path (SPEC §9): same `openai`/`rtc`/`codec`/`jitter`
//! modules as the Android core, with `cpal` for audio.
//!
//! ```text
//! desk --minter http://localhost:8080            # talk to Lexi (use headphones!)
//! OPENAI_API_KEY=... desk --dev-key              # dev only: mint directly, key from env only
//! desk --minter URL --headless --prompt "Say hi" --exit-after-response --out-wav /tmp/lexi.wav
//! ```

mod audio;

use lexi_core::codec::{FRAME_SAMPLES, SAMPLE_RATE};
use lexi_core::openai::{self, ServerEvent};
use lexi_core::rtc::{Handler, RtcConfig, RtcSession, RtcState};
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const USAGE: &str = "usage: desk (--minter URL | --dev-key) [--headless] [--tone] [--prompt TEXT]
            [--exit-after-response] [--duration SECS] [--out-wav FILE] [--install-hash HEX64]
  --minter URL            fetch an ephemeral token from lexi-minter (set LEXI_ATTEST_KEY in the
                          environment if it runs in shared-key attestation mode)
  --dev-key               mint directly with OPENAI_API_KEY from the environment (development only)
  --headless              no mic/speaker; sends silence (or --tone) and discards/records playout
  --tone                  with --headless: send a 440 Hz tone as mic input
  --prompt TEXT           send a text user message and request a response (no mic needed)
  --greet                 send response.create on open, as the app does (the prompt makes the model greet)
  --exit-after-response   exit after the first response.done
  --duration SECS         exit after SECS (default: run until Ctrl-C; 30 with --headless)
  --out-wav FILE          write received audio to a 48 kHz mono WAV (headless)
  --mic-wav FILE          with --headless: use this 48 kHz mono 16-bit WAV as mic input
  --mic-delay SECS        with --mic-wav: start feeding it after SECS (e.g. to interrupt the bot)
  --trace                 print mic/bot levels every 100 ms
  --voice V               output voice (default: cedar with --dev-key, the minter's default with --minter)
  --model M               realtime model (gpt-realtime-2.1-mini | gpt-realtime-2.1; default: mini / the minter's default)
  --instructions TEXT     with --dev-key: system instructions
  --vad-silence-ms N      with --dev-key: server_vad silence_duration_ms (API default 500)
  --semantic-eagerness E  with --dev-key: use semantic_vad (low|medium|high|auto)
  --effort E              with --dev-key: reasoning effort (minimal|low|medium|high|xhigh)
  --check-audio           open the mic and speaker streams for 1 s and report (no network, no token)";

#[derive(Default)]
struct Args {
    minter: Option<String>,
    dev_key: bool,
    headless: bool,
    tone: bool,
    prompt: Option<String>,
    /// Send only `response.create` when the session opens (what the app does) so the model greets first.
    greet: bool,
    exit_after_response: bool,
    duration: Option<u64>,
    out_wav: Option<String>,
    install_hash: Option<String>,
    mic_wav: Option<String>,
    mic_delay: f32,
    trace: bool,
    check_audio: bool,
    // --dev-key session tuning (measurement)
    voice: Option<String>,
    model: Option<String>,
    instructions: Option<String>,
    silence_ms: Option<u32>,
    eagerness: Option<String>,
    effort: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut val = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--minter" => a.minter = Some(val("--minter")?),
            "--dev-key" => a.dev_key = true,
            "--headless" => a.headless = true,
            "--tone" => a.tone = true,
            "--prompt" => a.prompt = Some(val("--prompt")?),
            "--exit-after-response" => a.exit_after_response = true,
            "--duration" => {
                a.duration = Some(val("--duration")?.parse().map_err(|_| "bad --duration")?)
            }
            "--out-wav" => a.out_wav = Some(val("--out-wav")?),
            "--install-hash" => a.install_hash = Some(val("--install-hash")?),
            "--mic-wav" => a.mic_wav = Some(val("--mic-wav")?),
            "--mic-delay" => {
                a.mic_delay = val("--mic-delay")?.parse().map_err(|_| "bad --mic-delay")?
            }
            "--trace" => a.trace = true,
            "--greet" => a.greet = true,
            "--voice" => a.voice = Some(val("--voice")?),
            "--model" => a.model = Some(val("--model")?),
            "--instructions" => a.instructions = Some(val("--instructions")?),
            "--vad-silence-ms" => {
                a.silence_ms = Some(
                    val("--vad-silence-ms")?
                        .parse()
                        .map_err(|_| "bad --vad-silence-ms")?,
                )
            }
            "--semantic-eagerness" => a.eagerness = Some(val("--semantic-eagerness")?),
            "--effort" => a.effort = Some(val("--effort")?),
            "--check-audio" => a.check_audio = true,
            "-h" | "--help" => return Err(USAGE.into()),
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    if !a.check_audio && a.minter.is_none() == !a.dev_key {
        return Err(format!(
            "exactly one of --minter / --dev-key is required\n{USAGE}"
        ));
    }
    Ok(a)
}

/// 64 hex chars standing in for sha256(install-id); random per run.
fn random_install_hash() -> String {
    let s = RandomState::new();
    (0..4)
        .map(|i| {
            let mut h = s.build_hasher();
            h.write_u64(i);
            format!("{:016x}", h.finish())
        })
        .collect()
}

struct Printer {
    done: Arc<AtomicBool>,
    started: Instant,
    /// When the harness began feeding "user speech" (for barge-in latency).
    mic_began: Arc<Mutex<Option<Instant>>>,
    /// `(call_id, query)` of web_search tool calls, answered by the main loop.
    tool_calls: std::sync::mpsc::Sender<(String, String)>,
}

impl Handler for Printer {
    fn on_state(&mut self, s: RtcState) {
        println!(
            "[{:>6.2}s] state: {s:?}",
            self.started.elapsed().as_secs_f32()
        );
    }
    fn on_event(&mut self, ev: ServerEvent) {
        let t = self.started.elapsed().as_secs_f32();
        match ev {
            ServerEvent::SessionCreated => println!("[{t:>6.2}s] session.created"),
            ServerEvent::SessionUpdated => println!("[{t:>6.2}s] session.updated"),
            ServerEvent::SpeechStarted { .. } => {
                let since = self
                    .mic_began
                    .lock()
                    .ok()
                    .and_then(|m| m.map(|i| i.elapsed()));
                match since {
                    Some(d) => println!("[{t:>6.2}s] speech_started -> playout flushed (barge-in; {} ms after mic speech began)", d.as_millis()),
                    None => println!("[{t:>6.2}s] speech_started -> playout flushed (barge-in)"),
                }
            }
            ServerEvent::SpeechStopped => println!("[{t:>6.2}s] speech_stopped"),
            ServerEvent::ResponseDone { response } => {
                let status = response.status.as_deref().unwrap_or("?");
                match &response.usage {
                    Some(u) => println!(
                        "[{t:>6.2}s] response.done status={status} usage: total={} in={} (text {}, audio {}, cached {}) out={} (text {}, audio {})",
                        u.total_tokens,
                        u.input_tokens,
                        u.input_token_details.text_tokens,
                        u.input_token_details.audio_tokens,
                        u.input_token_details.cached_tokens,
                        u.output_tokens,
                        u.output_token_details.text_tokens,
                        u.output_token_details.audio_tokens,
                    ),
                    None => println!("[{t:>6.2}s] response.done status={status} (no usage)"),
                }
                let mut called = false;
                for (call_id, name, args) in response.function_calls() {
                    let q = serde_json::from_str::<serde_json::Value>(args)
                        .ok()
                        .and_then(|v| v["query"].as_str().map(str::to_owned))
                        .unwrap_or_default();
                    println!("[{t:>6.2}s] tool call {name}: {q:?}");
                    called |= self.tool_calls.send((call_id.to_owned(), q)).is_ok();
                }
                // A tool call is not the end: the model speaks after the result.
                if !called {
                    self.done.store(true, Ordering::SeqCst);
                }
            }
            ServerEvent::Error { error } => {
                eprintln!(
                    "[{t:>6.2}s] ERROR {} {}: {}",
                    error.kind.as_deref().unwrap_or("-"),
                    error.code.as_deref().unwrap_or("-"),
                    error.message.as_deref().unwrap_or("-")
                );
            }
            ServerEvent::Other => {}
        }
    }
}

fn write_wav(path: &str, pcm: &[i16]) -> std::io::Result<()> {
    use std::io::Write;
    let data_len = (pcm.len() * 2) as u32;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?; // PCM
    f.write_all(&1u16.to_le_bytes())?; // mono
    f.write_all(&SAMPLE_RATE.to_le_bytes())?;
    f.write_all(&(SAMPLE_RATE * 2).to_le_bytes())?;
    f.write_all(&2u16.to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    for s in pcm {
        f.write_all(&s.to_le_bytes())?;
    }
    f.flush()
}

enum AudioIo {
    Headless {
        mic: lexi_core::rtc::MicProducer,
        pcm: lexi_core::jitter::ring::PcmConsumer,
    },
    Devices(#[allow(dead_code)] Vec<cpal::Stream>),
}

fn read_wav(path: &str) -> Result<Vec<i16>, String> {
    let b = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let u16le = |o: usize| b.get(o..o + 2).map(|x| u16::from_le_bytes([x[0], x[1]]));
    let u32le = |o: usize| {
        b.get(o..o + 4)
            .map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]))
    };
    if b.get(..4) != Some(b"RIFF") || b.get(8..12) != Some(b"WAVE") {
        return Err(format!("{path}: not a WAV file"));
    }
    if (u16le(22), u32le(24), u16le(34)) != (Some(1), Some(SAMPLE_RATE), Some(16)) {
        return Err(format!("{path}: need 48 kHz mono 16-bit PCM"));
    }
    let mut o = 12;
    while o + 8 <= b.len() {
        let len = u32le(o + 4).ok_or("truncated WAV")? as usize;
        if b.get(o..o + 4) == Some(b"data") {
            let end = (o + 8 + len).min(b.len());
            return Ok(b[o + 8..end]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| i16::from_le_bytes(*c))
                .collect());
        }
        o += 8 + len + (len & 1);
    }
    Err(format!("{path}: no data chunk"))
}

/// Open the real audio devices without any network use; prints the mic level for 1 s.
fn check_audio() -> Result<(), String> {
    let (mic, mut mic_c) = lexi_core::rtc::mic_ring();
    let (_pcm_p, pcm_c) = lexi_core::jitter::ring::pcm_ring(8);
    let mic_stream = audio::start_mic(mic)?;
    let spk_stream = audio::start_speaker(pcm_c)?;
    let mut n = 0usize;
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(1) {
        let avail = mic_c.slots();
        if let Ok(chunk) = mic_c.read_chunk(avail) {
            n += avail;
            chunk.commit_all();
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    println!(
        "mic stream ok: {n} samples captured in 1 s (expect ~48000); speaker stream ok (silence)"
    );
    drop((mic_stream, spk_stream));
    if n < 24_000 {
        return Err("mic delivered far fewer samples than 48 kHz real time".into());
    }
    Ok(())
}

/// A fresh `X-Lexi-Attest` value from `LEXI_ATTEST_KEY` (None when unset).
fn attest_now() -> Option<String> {
    let k = std::env::var("LEXI_ATTEST_KEY")
        .ok()
        .filter(|k| !k.is_empty())?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    openai::attest::header(&openai::Secret::new(k), now)
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    if args.check_audio {
        return check_audio();
    }
    let install_hash = args
        .install_hash
        .clone()
        .unwrap_or_else(random_install_hash);

    let minted = if let Some(url) = &args.minter {
        // Deployed minters use `shared-key` attestation: the key comes from the environment only.
        let attest = std::env::var("LEXI_ATTEST_KEY")
            .ok()
            .filter(|k| !k.is_empty())
            .and_then(|k| {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs());
                openai::attest::header(&openai::Secret::new(k), now)
            });
        openai::mint_from_minter(
            url,
            &install_hash,
            attest.as_deref(),
            args.voice.as_deref(),
            args.model.as_deref(),
        )
        .map_err(|e| format!("minter: {e}"))?
    } else {
        let key = std::env::var("OPENAI_API_KEY")
            .map_err(|_| "--dev-key needs OPENAI_API_KEY in the environment")?;
        let mut opts = openai::DevSession::default();
        if let Some(v) = &args.voice {
            opts.voice = v.clone();
        }
        if let Some(m) = &args.model {
            opts.model = m.clone();
        }
        if let Some(i) = &args.instructions {
            opts.instructions = i.clone();
        }
        opts.silence_ms = args.silence_ms;
        opts.eagerness = args.eagerness.clone();
        opts.reasoning_effort = args.effort.clone();
        openai::mint_with_dev_key(
            openai::OPENAI_BASE,
            &openai::Secret::new(key),
            &install_hash,
            &opts,
        )
        .map_err(|e| format!("dev-key mint: {e}"))?
    };
    // Never print the token itself.
    println!(
        "token minted (model {}, expires_at {})",
        minted.model, minted.expires_at
    );

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = Arc::clone(&stop);
        ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst))
            .map_err(|e| format!("ctrl-c handler: {e}"))?;
    }
    let done = Arc::new(AtomicBool::new(false));
    let mic_began = Arc::new(Mutex::new(None));
    let (tool_tx, tool_rx) = std::sync::mpsc::channel::<(String, String)>();
    let handler = Printer {
        done: Arc::clone(&done),
        started: Instant::now(),
        mic_began: Arc::clone(&mic_began),
        tool_calls: tool_tx,
    };
    let (session, mic, pcm) = RtcSession::start(RtcConfig::default(), &minted.token, handler)
        .map_err(|e| e.to_string())?;

    if args.greet {
        session.send_event(r#"{"type":"response.create"}"#.into());
    }
    if let Some(p) = &args.prompt {
        session.send_event(
            serde_json::json!({"type":"conversation.item.create","item":{"type":"message","role":"user",
                "content":[{"type":"input_text","text":p}]}})
            .to_string(),
        );
        session.send_event(r#"{"type":"response.create"}"#.into());
    }

    let mic_samples = args.mic_wav.as_deref().map(read_wav).transpose()?;
    let deadline = args
        .duration
        .or(args.headless.then_some(30))
        .map(|s| Instant::now() + Duration::from_secs(s));
    let mut recorded: Vec<i16> = Vec::new();
    // Real devices (streams must stay alive for the whole call) or simulated ones.
    let mut io = if args.headless {
        AudioIo::Headless { mic, pcm }
    } else {
        let streams = vec![audio::start_mic(mic)?, audio::start_speaker(pcm)?];
        println!("talking to Lexi; Ctrl-C to hang up (use headphones to avoid echo)");
        AudioIo::Devices(streams)
    };

    let start = Instant::now();
    let mut frames = 0u64;
    let mut last_levels = Instant::now();
    let mut last_trace = Instant::now();
    let mut exit_reason = "interrupted";
    let mut done_at: Option<Instant> = None;
    let mut quiet_since: Option<Instant> = None;
    // Latency: end of the user's (WAV) speech -> first bot audio.
    let mut last_voiced: Option<Instant> = None;
    let mut first_audio_reported = false;
    while !stop.load(Ordering::SeqCst) {
        while let Ok((call_id, query)) = tool_rx.try_recv() {
            // Answer like the app does: ask the minter, hand the result back, ask the model to continue.
            let (murl, hash) = (args.minter.clone(), install_hash.clone());
            let t0 = Instant::now();
            let output = match murl {
                Some(u) => {
                    match openai::search_via_minter(&u, &hash, attest_now().as_deref(), &query) {
                        Ok(text) => {
                            serde_json::json!({ "result": text, "how_to_answer": "Tell the user only the one or two key points, in their language, in at most 20 words. No sources, no links, no list." })
                        }
                        Err(e) => serde_json::json!({ "error": e.to_string() }),
                    }
                }
                None => serde_json::json!({ "error": "no minter" }),
            };
            println!(
                "tool result after {} ms: {output}",
                t0.elapsed().as_millis()
            );
            session.send_event(
                serde_json::json!({"type":"conversation.item.create","item":{
                    "type":"function_call_output","call_id":call_id,"output":output.to_string()}})
                .to_string(),
            );
            session.send_event(r#"{"type":"response.create"}"#.into());
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            exit_reason = "duration elapsed";
            break;
        }
        if args.exit_after_response && done.load(Ordering::SeqCst) {
            // response.done is sent before the audio has finished streaming in real time:
            // wait until playout has been quiet for 600 ms (cap 20 s after done).
            let done_at = *done_at.get_or_insert_with(Instant::now);
            // (the server keeps streaming near-silent packets, so ring occupancy says nothing)
            if session.levels.bot() > 0.005 {
                quiet_since = None;
            } else {
                let q = *quiet_since.get_or_insert_with(Instant::now);
                if q.elapsed() >= Duration::from_millis(600) {
                    exit_reason = "response done and audio drained";
                    break;
                }
            }
            if done_at.elapsed() > Duration::from_secs(20) {
                exit_reason = "response done (drain timeout)";
                break;
            }
        }
        if let AudioIo::Headless { mic, pcm } = &mut io {
            // Behave like audio devices: mic frame and speaker pull every 20 ms.
            let due = start.elapsed().as_millis() as u64 / 20;
            while frames < due {
                let mut frame = vec![0i16; FRAME_SAMPLES];
                let at_s = frames as f32 * 0.02;
                if let Some(m) = mic_samples.as_deref().filter(|_| at_s >= args.mic_delay) {
                    let idx = ((at_s - args.mic_delay) * 50.0).round() as usize * FRAME_SAMPLES;
                    if let Some(src) = m.get(idx..) {
                        let n = src.len().min(FRAME_SAMPLES);
                        frame[..n].copy_from_slice(&src[..n]);
                        // Only the first *voiced* frame counts (the WAV may start with silence).
                        let energy: i64 = frame.iter().map(|&x| i64::from(x).pow(2)).sum::<i64>()
                            / FRAME_SAMPLES as i64;
                        if energy > 300 * 300 {
                            last_voiced = Some(Instant::now());
                            if let Ok(mut began) = mic_began.lock() {
                                began.get_or_insert_with(Instant::now);
                            }
                        }
                    }
                } else if args.tone {
                    for (i, x) in frame.iter_mut().enumerate() {
                        let t = (frames as usize * FRAME_SAMPLES + i) as f32 / SAMPLE_RATE as f32;
                        *x = (6000.0 * (2.0 * std::f32::consts::PI * 440.0 * t).sin()) as i16;
                    }
                }
                mic.push(&frame);
                let mut out = vec![0i16; FRAME_SAMPLES];
                let n = pcm.pull(&mut out);
                if args.out_wav.is_some()
                    && (n > 0 || !recorded.is_empty())
                    && recorded.len() < 48_000 * 120
                {
                    recorded.extend_from_slice(&out);
                }
                frames += 1;
            }
        }
        if !first_audio_reported && session.levels.bot() > 0.02 {
            if let Some(t) = last_voiced.filter(|t| t.elapsed() > Duration::from_millis(300)) {
                first_audio_reported = true;
                println!(
                    "LATENCY end of user speech -> first bot audio: {} ms",
                    t.elapsed().as_millis()
                );
            }
        }
        if args.trace && last_trace.elapsed() >= Duration::from_millis(100) {
            last_trace = Instant::now();
            println!(
                "  t={:>5.2}s mic={:.3} bot={:.3}",
                start.elapsed().as_secs_f32(),
                session.levels.mic(),
                session.levels.bot()
            );
        }
        if !args.trace && last_levels.elapsed() >= Duration::from_millis(500) {
            last_levels = Instant::now();
            let (m, b) = (session.levels.mic(), session.levels.bot());
            if m > 0.01 || b > 0.01 {
                println!("  levels mic={m:.2} bot={b:.2}");
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    println!("closing ({exit_reason})...");
    drop(io);
    session.close();
    if let Some(path) = &args.out_wav {
        write_wav(path, &recorded).map_err(|e| format!("wav: {e}"))?;
        println!(
            "wrote {} ({:.1}s of audio)",
            path,
            recorded.len() as f32 / SAMPLE_RATE as f32
        );
    }
    println!("closed cleanly");
    Ok(())
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            std::process::ExitCode::FAILURE
        }
    }
}

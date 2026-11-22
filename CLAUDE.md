# CLAUDE.md — working on Lexi

Lexi: a generic voice chat bot for Android: you talk, it talks back, and it can search the web for current
events (more tools may follow). Rust core (NDK) + thin Kotlin shell + a Rust server (`server/`, crate `lexi`,
deployed on Kubernetes as `lexi`) that mints ephemeral tokens and runs web searches.
Talks WebRTC to the OpenAI Realtime API (`gpt-realtime-2.1-mini`, or `gpt-realtime-2.1` as "Smart");
Google Gemini Live (WebSocket) added as a second provider.

- **Source of truth:** `docs/SPEC.md` (what) and `docs/PLAN.md` (phases, checkboxes). Work one phase at a time; tick boxes and commit at the end of each phase.
- **Never** put an OpenAI or Google API key, keystore, or `.env` in the repo, code, logs, or test fixtures. Ephemeral tokens must never be logged.
- **External APIs change.** Before implementing any OpenAI or Android API marked [VERIFY], check the current official docs and note what you verified in the relevant README.
- **Keep it small:** no new dependency without a one-line justification in the PR/commit message. Prefer `default-features = false`. Android: no Compose; only `androidx.core`.
- **Rust rules:** `cargo fmt`, `cargo clippy --workspace -- -D warnings`, `cargo test --workspace` must pass. No `unwrap()` in non-test code paths reachable from JNI or request handlers. Every JNI export is wrapped in `catch_unwind`. Audio callbacks: no allocation, no locks, no JNI.
- **Platform split:** `core/rtc`, `core/openai`, `core/codec`, `core/jitter` must compile for desktop too (used by `devtools/desk`). Android-only code goes behind `#[cfg(target_os = "android")]` in `core/ffi`, `core/audio`, `core/render`.
- **Commands:**
  - Workspace: `cargo test --workspace`
  - Android lib: `cargo ndk -t arm64-v8a --platform 31 -o android/app/src/main/jniLibs build -p lexi-core --release`
  - App: `cd android && ./gradlew assembleDebug`
  - Server: `OPENAI_API_KEY=... cargo run -p lexi`
  - Desktop harness: `cargo run -p desk -- --service http://localhost:8080`
  - Deploy: `scripts/deploy.sh` (see `docs/RUNBOOK.md`)
- If something in the spec turns out to be wrong or impossible, stop, explain, and propose an edit to `docs/SPEC.md` instead of silently diverging.

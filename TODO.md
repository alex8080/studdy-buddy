# TODO

Project-level work list. Context for each item lives in linked design docs; don't duplicate it here.

## LLM provider — in flight

Hand-off context: [`docs/handoff-llm.md`](docs/handoff-llm.md). Design: [`docs/llm.md`](docs/llm.md).

- [x] `RetryingProvider<P>` + `RetryPolicy` in `src/llm/retry.rs`. Decorator: retry `Transient` per policy, pass `BadInput`/`Config` through, respect `retry_after`. Stub-provider unit tests. Wrapped in `main.rs`.
- [x] Config-file loading: `toml` dep, `studybuddy.toml` resolution (`STUDYBUDDY_CONFIG=/path` env override → `./studybuddy.toml` fallback → defaults), `[server]` / `[store]` / `[llm]` sections with deny-unknown-fields. All hardcoded values replaced in `main.rs`.
- [x] `studybuddy.toml.example` at project root.
- [x] Status updates: `CLAUDE.md`, `docs/llm.md`, `docs/architecture.md`.

## Cloud LLM providers

Trait + retry decorator are already in place — these just add concrete impls.

- [ ] `llm::anthropic` — hand-rolled HTTP via `reqwest`, error-mapped to `LlmError`. API key in `[llm]` config.
- [ ] `llm::openai` — same shape.

## Watcher

Discovery (`discover_notes`, `ingest_directory`) is built in `src/watcher.rs`. Live watching + reconciliation are not.

- [ ] `notify`-based file watcher; debounce flurries of edits.
- [ ] Per-file re-ingest on change → push to `POST /ingest` via the existing client.
- [ ] Reconciliation: diff fresh chunks against stored cards using the `(source_file, source_heading)` anchor; flag orphans (note deleted) and stale (content changed). **Never auto-delete** — per [`DESIGN.md`](DESIGN.md).
- [ ] Reconciliation HTTP endpoints (`GET /cards/stale`, `GET /cards/orphaned`, `POST /cards/{id}/keep`) per [`docs/api.md`](docs/api.md).

## Web UI

- [ ] Frontend over the HTTP API: curation queue + review session. Backend is intentionally cadence-agnostic; UX lives here.

## Answer evaluation (`POST /reviews/evaluate`)

Free-text answer grading via LLM during review. Design: [`DESIGN.md`](DESIGN.md) §3, [`docs/api.md`](docs/api.md#post-reviewsevaluate--built).

- [x] `store::get_card` — single-card lookup on `Repository` trait (currently omitted as YAGNI; needed here).
- [x] `LlmProvider::evaluate_answer(card, user_answer) -> Result<EvaluationResult, LlmError>` — new trait method + prompt (Q&A only); impl for Ollama (and cloud providers when those land). Prompt must anchor the LLM to the card's expected answer, not world knowledge, and accept paraphrasing as correct.
- [x] `EvaluateRequest` / `EvaluateResponse` wire DTOs in `wire.rs`; `client.rs` method; integration tests.
- [x] `POST /reviews/evaluate` handler in `api.rs` — branch on card type: cloze → fuzzy match on fills (no LLM); Q&A → `llm.evaluate_answer`. 404 on unknown card, 503 on transient LLM failure (Q&A path only), 500 on config/IO error.
- [ ] Web UI changes: text area below question; "Reveal" → "Submit" when non-empty; evaluating state (answer hidden, buttons disabled); verdict + explanation display; suggested-rating pre-highlight; error message on 503.

## Later (post-v1, opt-in)

- [ ] FSRS scheduler via `fsrs-rs` — swap behind the existing `Scheduler` trait once review data has accumulated.
- [ ] Additional frontends (mobile, IDE plugin).
- [ ] Optional "expand topic" web augmentation — explicit opt-in only ([`DESIGN.md`](DESIGN.md) v1 keeps the user's notes as the sole source of truth).
- [ ] Multi-device sync — needs a hosted mode or a sync protocol.
- [ ] Sharing decks between users.

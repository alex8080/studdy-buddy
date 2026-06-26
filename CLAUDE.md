# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Status

StudyBuddy is in **early implementation**. Up and tested (112 tests): axum server on `127.0.0.1:8080` with the full HTTP surface — `/health`, content-based `POST /ingest` (push model: `{ source_file, content }` → chunk → LLM → persist `Pending`), curation (`GET /cards/pending`, `POST /cards/{id}/accept|reject`, `PATCH /cards/{id}` → 409 if not pending) and review (`GET /cards/due`, `POST /reviews`). SM-2 scheduler, `ingest_text` parser/chunker, the `store` (`Repository` trait + in-memory + file backend with sha256 sidecars), a typed HTTP `client` over shared `wire` DTOs, the `sb` CLI (`push`/`curate`/`review`), and a `watcher` feeder that pushes a vault to `/ingest`, all wired into `AppState { llm, store, scheduler, api_token }`. Ollama `LlmProvider` + `RetryingProvider` + config-file loading (`studybuddy.toml`) exist. Bearer-token auth is implemented (see Auth section below). Still to build: cloud LLM providers (`llm::anthropic`/`openai`), the watcher's change-detection + `notify` live-watching + reconciliation protocol, and a web UI.

**Design docs:**
- `DESIGN.md` — high-level vision and load-bearing decisions. Read before non-trivial changes.
- `docs/architecture.md` — subsystem map, traits, data shapes, current status.
- `docs/api.md` — HTTP endpoint contracts and per-endpoint internal flows.

## What StudyBuddy is

A self-hosted local HTTP server that ingests a directory of markdown notes (Obsidian-compatible), uses an LLM to propose flashcards, lets the user curate them, and schedules reviews via spaced repetition. Backend-first: future frontends (web first, then others) are clients of the HTTP API.

## Tech stack

- **Rust 2024 edition**, single binary crate (`studybuddy`) with `lib.rs` + `main.rs`.
- **In `Cargo.toml`**: `axum`, `tokio`, `serde`, `serde_json`, `serde_yaml` (frontmatter), `toml` (config-file parsing), `chrono`, `uuid`, `sha2` (sidecar filenames), `thiserror`, `anyhow`, `async-trait`, `reqwest` (HTTP client), `clap` (the `sb` CLI, with `derive`/`env`), `tracing`, `tracing-subscriber`. Dev: `tempfile`, `tower` (with `util` feature, for `ServiceExt::oneshot` in API tests), `wiremock` (client unit tests).
- **Planned, not yet added**: `notify` (file watcher), `pulldown-cmark` (richer markdown if line-based parsing isn't enough), hand-rolled cloud LLM client types (`llm::anthropic`/`openai` — no first-party Anthropic/OpenAI Rust SDK; built on the existing `reqwest`), `fsrs-rs` (future), `rusqlite`/`sqlx` (storage — only if a multi-tenant hosted mode arrives; the v1 `store` is file-based behind a `Repository` trait, see `docs/architecture.md`).

Iteration cost on prompt design is the main downside of Rust here; keep LLM card-generation logic in an isolated module with quick integration tests so prompts can be iterated without recompiling the whole server.

## Build / test / run

- `cargo build` — build everything.
- `cargo run` — start the server on `127.0.0.1:8080`. Smoke test: `curl http://127.0.0.1:8080/health` → `{"status":"ok"}`.
- `cargo test` — run all tests.
- `cargo test scheduler::tests::sm2_again_resets_interval` — run a single test by path.
- `cargo clippy --all-targets` — lint (currently clean; keep it that way).
- `cargo fmt` — format.
- Log level: `RUST_LOG=debug cargo run` (uses `tracing-subscriber`'s `EnvFilter`).

## Module layout

```
src/
  main.rs        # entry: tracing init, build AppState (store from STUDYBUDDY_DATA_DIR), axum::serve
  lib.rs         # module declarations
  api.rs         # axum Router + handlers; AppState { llm, store, scheduler }
  error.rs       # AppError, Result alias
  model.rs       # Card, CardContent (Qa | Cloze), CardStatus, Rating, Review
  wire.rs        # shared HTTP request/response DTOs (server handlers + client)
  client.rs      # typed reqwest Client — one method per endpoint (shared by sb + watcher)
  cli.rs         # run_push/run_review/run_curate command logic (injected I/O)
  scheduler.rs   # Scheduler trait + Sm2 impl (with unit tests)
  llm/           # LlmProvider trait, ChunkContext, ProposedCard, ollama (+ retry, prompt)
  ingest.rs      # ChunkConfig, ingest_text (content→chunks) — BUILT; 24 unit tests
  store/         # Repository trait + InMemoryRepository + FileRepository (sha256 sidecars)
  watcher.rs     # vault walking: ingest_directory/ingest_file + discover_notes (raw push)
  bin/sb.rs      # `sb` CLI: thin clap shell over cli.rs (push/curate/review, $EDITOR for edits)
  bin/watcher.rs # feeder binary: discover_notes → Client::ingest (one-shot vault push)
tests/
  watcher.rs                 # 11 acceptance tests: ingest_directory + discover_notes (real fixtures)
  store.rs                   # 6 acceptance tests for FileRepository (tempdir)
  api.rs                     # 15 integration tests driving the Router via tower::ServiceExt::oneshot
  client.rs                  # acceptance: Client vs a real spawned server (the wire contract)
  cli.rs                     # acceptance: cli::run_* with injected I/O vs a spawned server
  common/mod.rs              # shared harness: FakeLlmProvider + spawn_server / in_memory_router
  fixtures/ingest/           # nested/, mixed_extensions/, hidden_dirs/, sample_vault/ (used by tests/watcher.rs)
```

### Test split convention

- **Unit tests** (in `src/<module>.rs` under `#[cfg(test)] mod tests`) — exercise parser/chunker edge cases with synthetic inline input via `ingest_text(content, source_file, config)`. No filesystem.
- **Acceptance tests** (in `tests/<module>.rs`) — exercise the full pipeline through its public API against real fixture files in `tests/fixtures/`. Only verify high-level pipeline behavior; leave edge cases to unit tests.

When adding behavior to ingest: write a failing unit test first (synthetic input), then add an acceptance test only if it changes how the system composes end-to-end on a realistic vault.

Subsystems still to build: the watcher's change-detection + `notify` live-watching + reconciliation protocol, and concrete cloud LLM providers (`llm::anthropic`, `llm::openai`; `llm::ollama` exists).

## Ingest behavior (the contract the tests enforce)

`ingest_text` (in `src/ingest.rs`) parses one note's content; **discovery** lives in the watcher (`src/watcher.rs`, tested by `tests/watcher.rs`). In the push model the server receives `{ source_file, content }` per file — it never walks the filesystem.

- **Discovery** (watcher): walk dir recursively, only `.md` files, skip any dir whose name starts with `.` (covers `.git`, `.obsidian`).
- **Frontmatter**: parse YAML; `studybuddy.exclude: true` → `ingest_text` returns no chunks; `tags:` array merges into chunk tags.
- **Tags**: extract `#tag` and `#hierarchical/tag` from prose only — NOT from fenced code, inline code, or markdown headings.
- **Chunking**: heading-based, with size constraints from `ChunkConfig { target_words: 500, max_words: 1500, min_words: 50 }`.
  - Combine small sibling sections under their common parent (attribution rolls up to parent path).
  - Split oversized sections at sub-headings if present; otherwise at paragraph boundaries; never mid-sentence.
  - `source_heading` is the path `"Linear Algebra > Vectors > Dot Product"`; `None` for content before the first heading.
- **Obsidian syntax**: `[[link]]` → `link`, `[[target|alias]]` → `alias`, `![[embed]]` → dropped, callout `[!type]` marker stripped (content kept).
- **Paths**: `source_file` is the note's vault-relative path; the feeder sends it, the server validates it (relative, no `..`) and records it verbatim as the card anchor.

Per-card leaf anchoring within a merged chunk is **out of scope for ingest** — it requires LLM cooperation during card generation. v1 attributes all cards from a merged chunk to the chunk's heading (common ancestor).

## Auth

- Bearer token: `[server] api_token = "..."` in `studybuddy.toml` or `STUDYBUDDY_API_TOKEN` env var.
- `GET /health` is intentionally unprotected (reverse-proxy health checks); all other routes require the token when one is configured.
- Token comparison uses SHA-256 digests (timing-safe, no new dep — `sha2` already present).
- Token validated at CLI startup via `client::validate_api_token()`, not deep in the client.
- `tests/common/mod.rs` has `in_memory_router_with_token(llm, token)` for auth integration tests.

## Load-bearing constraints (don't violate without re-opening the design)

- **Self-hosted local only in v1.** No SaaS, no multi-user concerns. Bearer-token auth exists for remote access (`[server] api_token` / `STUDYBUDDY_API_TOKEN`).
- **Source of truth is the user's markdown.** No web augmentation in v1 — the LLM works from notes only.
- **Cards keep a `(source_file, source_heading)` anchor.** Cheap to add now, painful to retrofit. Every card type must carry it.
- **SRS lives behind a `Scheduler` trait.** SM-2 is the v1 implementation; FSRS swaps in later via `fsrs-rs`. Do not let SM-2-specific state leak into the rest of the code.
- **LLM backend is pluggable.** Both user-supplied cloud API key (Anthropic/OpenAI) and local Ollama must work. Code against a trait, not a concrete provider.
- **Sync is reconciliation, not deletion.** When a note changes or disappears, orphaned/stale cards are flagged for user review — never auto-removed.
- **Quiz cadence (notifications, daily session shape) is a frontend concern.** The backend only exposes "what's due now"; do not bake scheduling UX into the server.
- **Card format is per-card.** Both Q&A and cloze coexist; the LLM picks per chunk. Storage and rendering must handle both.
- **Obsidian syntax handling**: use `#tags` and YAML frontmatter as signal; strip wikilinks and callouts. Don't expand into full Obsidian-aware parsing for v1.

# Architecture

StudyBuddy is a self-hosted local HTTP server that ingests a directory of markdown notes, asks an LLM to propose flashcards, lets the user curate them, and schedules reviews via spaced repetition. See [`../DESIGN.md`](../DESIGN.md) for *why* this shape.

This doc maps the *internal* subsystems: what each one owns, how they're connected, and what's built vs planned.

## Subsystem map

```
   sb CLI (src/bin/sb.rs + cli.rs) ───┐
   watcher (src/bin/watcher.rs)    ───┤ via client.rs (typed reqwest, wire.rs DTOs)
                                      │  HTTP: POST /ingest { source_file, content }, etc.
                                      ▼
                                     ┌───────────────────┐
                                     │   api  (axum)     │  src/api.rs
                                     └─────────┬─────────┘
                                               │ composes
                     ┌───────────────┬─────────┼───────────┬──────────────┐
                     ▼               ▼         ▼           ▼              ▼
                 ┌────────┐     ┌────────┐  ┌───────────┐ ┌────────┐
                 │ ingest │     │  llm   │  │ scheduler │ │ store  │
                 │(text→  │     └────────┘  └───────────┘ └───┬────┘
                 │ chunks)│                                   │
                 └────────┘                          ┌────────┴───────┐
                                                     │ file backend   │
                                                     │ (data dir)     │
                                                     └────────────────┘

Shared by all: `model.rs` (domain types) and `error.rs` (AppError, Result).
```

The watcher and the `sb` CLI are **clients** of the HTTP API, not in-process subsystems: they push note content (and, for `sb`, drive curation/review); the server parses, chunks, proposes, and persists. The server never reads the vault filesystem. Both talk to it through the one shared `client.rs`, which speaks the `wire.rs` DTOs the server handlers also use.

Subsystems talk only through explicit interfaces (traits or plain function signatures). Handlers in `api.rs` compose them; nothing else reaches across.

## Subsystems

### `model` (`src/model.rs`) — built

Shared domain types. No logic, no I/O.

| Type | What it is |
|---|---|
| `Card { id, content, source_file, source_heading, tags, status, created_at }` | A single flashcard |
| `CardContent::{Qa, Cloze}` | The two card formats; the LLM picks per chunk |
| `CardStatus::{Pending, Accepted, Orphaned, Stale, Rejected}` | Curation/reconciliation state |
| `Rating::{Again, Hard, Good, Easy}` | User's review outcome (see the doc comment for naming rationale) |
| `Review { card_id, reviewed_at, rating, next_due }` | A single review event |

Every other subsystem depends on this.

### `ingest` (`src/ingest.rs`) — built

Parses one markdown string into `Vec<ChunkContext>` ready for the LLM. Filesystem-free — in the push model the server receives note *content*, never a path, so `ingest` no longer walks directories (that moved to [`watcher`](#watcher--separate-feeder-app)).

Public API:

| Function | Use |
|---|---|
| `ingest_text(content, source_file, config) -> Result<Vec<ChunkContext>>` | Parse one markdown string. The only ingest entry point; `source_file` is recorded verbatim on each chunk. Returns `[]` if frontmatter sets `studybuddy.exclude: true`. |

Internal pipeline:

1. Split YAML frontmatter from body; respect `studybuddy.exclude: true`.
2. Walk body line-by-line, building heading-anchored sections (path like `"Linear Algebra > Vectors > Dot Product"`).
3. Apply Obsidian transforms: `[[link]] → link`, `[[t|alias]] → alias`, `![[embed]] → dropped`, callout `[!type]` marker stripped.
4. Extract `#tags` from prose only (skip code blocks, inline code, headings).
5. Merge small sibling sections under their common parent; split oversized sections at sub-headings or paragraph boundaries (see `ChunkConfig { target_words: 500, max_words: 1500, min_words: 50 }`).
6. Emit `Vec<ChunkContext>` carrying `(source_file, source_heading, tags, text)`.

`ChunkContext` (defined in `llm.rs`) is the bridge to card generation.

### `scheduler` (`src/scheduler.rs`) — built (SM-2 only)

The SRS engine. Lives behind a `Scheduler` trait so FSRS can drop in later via `fsrs-rs` without touching anything upstream.

- `Scheduler::on_review(state, rating, now) -> ReviewOutcome { state, next_due }`
- `Sm2` — SuperMemo SM-2 v1 implementation. The quality-score mapping is SM-2-local (see in-source comment).
- `SchedulerState { interval_days, ease, repetitions }` — per-card persisted scheduler state. SM-2-shaped today; FSRS will need a different shape, which is the main reason the trait owns this type.

### `llm` (`src/llm.rs`) — trait + types only

Defines the boundary between "chunked text" and "proposed cards." Pluggable for cloud (Anthropic/OpenAI via user key) and local (Ollama).

```
LlmProvider::propose_cards(chunk: &ChunkContext) -> Result<Vec<ProposedCard>, LlmError>
LlmProvider::evaluate_answer(card: &Card, user_answer: &str) -> Result<EvaluationResult, LlmError>
```

| Type | Role |
|---|---|
| `ChunkContext { source_file, source_heading, tags, text }` | LLM input — produced by `ingest` |
| `ProposedCard { content, rationale }` | LLM output — goes into the pending-review queue |
| `EvaluationResult { verdict, explanation, suggested_rating }` | LLM output for `POST /reviews/evaluate` — verdict is `correct`/`partial`/`incorrect` |
| `LlmError::{Transient, BadInput, Config}` | Provider failure classified by recovery action |

A `RetryingProvider<P>` decorator handles `Transient` retries once for all providers — concrete impls (`llm::ollama` first, then `llm::anthropic` / `llm::openai`) only need to make one attempt and classify the error. Prompt text lives in `src/llm/prompt.rs` so it can be iterated without recompiling the rest of the server.

Concrete providers are **not** yet built. See [llm.md](llm.md) for the full design — error taxonomy, retry decorator, Ollama specifics, config-file shape, and the testing strategy.

### `api` (`src/api.rs`) — built (+ one endpoint planned)

Axum router + handlers over `AppState { llm, store, scheduler }`. Endpoints built: `/health`, `POST /ingest`, `GET /cards/pending`, `GET /cards/due`, `POST /reviews`, `POST /cards/{id}/accept`, `POST /cards/{id}/reject`, `PATCH /cards/{id}`. Planned: `POST /reviews/evaluate` (LLM-graded free-text answer — see [api.md](api.md)). Returns JSON. `AppError` has an `IntoResponse` impl here mapping each variant to a status (`BadRequest`/`Parse` → 400, `NotFound` → 404, `Conflict` → 409, `Io`/`Llm` → 500, `Unavailable` → 503 for transient LLM failures on evaluate), so handlers can `?` errors directly. Cards serialize in full. See [api.md](api.md) for the full contract and per-endpoint flows.

### `error` (`src/error.rs`) — built

Crate-wide `AppError` and `Result<T> = std::result::Result<T, AppError>`.

| Variant | Source / status |
|---|---|
| `Io(std::io::Error)` | Filesystem / network → 500 |
| `NotFound` | Resource lookup miss → 404 |
| `Llm(String)` | LLM-provider errors → 500 |
| `Parse(String)` | Frontmatter YAML errors → 400 |
| `BadRequest(String)` | Invalid input (e.g. bad `source_file`) → 400 |
| `Conflict(String)` | State conflict (e.g. editing a non-`Pending` card) → 409 |

### `store` — built (in-memory + file backend)

Owns persistence of `Card`, `Review`, and `SchedulerState`. Everything sits behind a **`Repository` trait** — the rest of the code (api handlers, the future watcher) depends only on the trait, never on a concrete backend. `InMemoryRepository` is the test double; `FileRepository` is the v1 file backend. Both implement the full trait.

```rust
#[async_trait]
pub trait Repository: Send + Sync {
    // ingest — persist freshly proposed cards (status = Pending)
    async fn save_pending(&self, cards: &[Card]) -> Result<()>;

    // curation — GET /cards/pending, PATCH /cards/{id}, accept/reject
    async fn list_pending(&self) -> Result<Vec<Card>>;
    async fn update_content(&self, card: CardId, content: CardContent) -> Result<()>;
    async fn set_status(&self, card: CardId, status: CardStatus) -> Result<()>;

    // review — GET /cards/due, POST /reviews
    async fn list_due(&self, now: DateTime<Utc>) -> Result<Vec<Card>>;
    async fn load_state(&self, card: CardId) -> Result<SchedulerState>;
    async fn save_state(&self, card: CardId, state: SchedulerState, next_due: DateTime<Utc>) -> Result<()>;
    async fn save_review(&self, review: &Review) -> Result<()>;
}
```

Every method maps to a documented call site in [api.md](api.md); nothing speculative. Async to match `LlmProvider` (the other I/O trait) and to let an async `sqlx` backend drop in unchanged. Deliberately omitted for now (YAGNI): single-card `get` — the file backend reads-to-write so `update_content` enforces the `Pending`-only guard in place, but `POST /reviews/evaluate` will need it (added when that endpoint lands). `Note`-metadata persistence and by-`source_file` lookups (watcher-reconciliation needs, added when the watcher lands), and any `delete` (cards are flagged, never removed).

**v1 backend: files, not a database.** The store holds *derived* data — cards and SRS state are generated, not authored; the markdown vault stays the source of truth for notes. The store lives in a **configured server data dir** (`store.data_dir` in `studybuddy.toml`, default `./studybuddy-data`), independent of any vault — one store per server. At single-user scale a file backend is enough, stays inspectable, and avoids a DB dependency. Directly under the data dir:

- **Cards: one file per note.** All cards for a note live in one sidecar at `cards/<sha256(source_file)>.json`, keyed by the `(source_file, source_heading)` anchor (the readable path lives inside each card). The filename is a hash of the note's vault-relative path because that path arrives as **untrusted HTTP input** — hashing is flat, fixed-length, and traversal-proof. One-file-per-note matches reconciliation: re-ingest one note → rewrite one sidecar. Each card carries a stable UUID (assigned at acceptance) so content edits don't churn the IDs scheduling state references.
- **Current SRS state: one `state.json`.** A compact `card_id → (SchedulerState, next_due)` map. Storing `next_due` here (decision B) is what makes it a genuine due-index: a due-scan reads this one file, never the unbounded review log. Tiny even at thousands of cards; each review rewrites it via atomic temp-write-and-rename.
- **Review history: one append-only `reviews.jsonl`.** Never rewritten. This is the durable grade-and-timestamp log FSRS will train on later.

We deliberately do **not** shard SRS state by the user's directory structure. That couples the layout to folder taste (a flat vault collapses to a single hot file), amplifies writes (one review rewrites a whole directory's state), and gives the append-only review log nowhere good to live. Two global files beat per-directory files on every axis that matters below the scale where a database wins outright.

**When a database becomes necessary.** If StudyBuddy grows into a multi-tenant hosted web service, files stop being viable — concurrent tenants, transactional integrity, and indexed cross-user queries demand SQLite or Postgres. Because everything is behind `Repository`, that is a new trait impl, not a rewrite. SQLite is also the natural *local* upgrade if watcher concurrency or review-log size ever outgrows the file backend. Schema migrations become the store's problem then, not now.

### `client` (`src/client.rs`) + `wire` (`src/wire.rs`) — built

`wire.rs` holds the HTTP request/response DTOs (`IngestRequest`/`IngestResponse`, `CardsResponse`, `ReviewRequest`/`ReviewResponse`, `UpdateContentRequest`) shared by the server handlers and the client, so the contract has one definition and can't drift. `client.rs` is a typed `reqwest` wrapper — one async method per endpoint (`ingest`/`pending`/`due`/`accept`/`reject`/`patch`/`review`), with response/error handling factored into `json_or_err`/`empty_or_err`/`error_from`. It holds no domain logic and is the single place anything talks to the server. Tested with `wiremock` unit tests plus `tests/client.rs` (the lifecycle against a real spawned server — the wire-contract test).

### `cli` (`src/cli.rs`) + `sb` (`src/bin/sb.rs`) — built

`cli.rs` is the `sb` command logic with injected I/O — `run_push`/`run_review`/`run_curate` each take a `Client` plus reader/writer handles (and `run_curate` an editor closure), so `tests/cli.rs` drives them with `Cursor`/`Vec<u8>` against a spawned server. `src/bin/sb.rs` is the thin clap shell that wires real `stdin`/`stdout` and `$EDITOR`; all logic and tests live in the lib. Like the watcher, it's a pure client over `client.rs` — no domain logic.

### `watcher` — separate feeder app

In the push model the watcher is a **standalone client**, not part of the server: it owns the vault filesystem, the server doesn't touch it. It walks a directory and pushes each note's content to `POST /ingest`. This decouples the server from the filesystem (any feeder — watcher, manual upload, the `sb` CLI — pushes the same way through `client.rs`) and is forward-compatible with a hosted mode (clients push; the server has no access to their disks).

- **`src/watcher.rs`** — the filesystem walking: `ingest_directory`/`ingest_file` (legacy, still chunks locally) plus `discover_notes` (push-model: returns each `.md` note's relative path + raw content, unchunked, for the server to process). 11 acceptance tests in `tests/watcher.rs`.
- **`src/bin/watcher.rs`** — the feeder binary: `discover_notes` → `Client::ingest` per file, a one-shot full vault sweep. Still to build: content-hash change detection (skip unchanged notes) and `notify`-based live watching.
- **Reconciliation (still to build):** the server no longer re-walks the disk, so the feeder must report deletions / send a manifest for the server to flag orphans (note deleted) and stale (content changed) against the `(source_file, source_heading)` anchor. Per DESIGN.md, cards are *flagged*, never auto-deleted. This sync protocol is deferred to the real watcher build.

## Cross-subsystem flows

The shared spine is:

```
HTTP request → api handler → ingest + llm + scheduler + store → JSON response
```

Per-endpoint flows live in [api.md](api.md). The main one — `POST /ingest` — runs `ingest_text` on the pushed note, feeds each chunk through `llm.propose_cards`, and persists the results via `store.save_pending`.

## Status snapshot

| Subsystem | State |
|---|---|
| model, error | done |
| scheduler (SM-2) | done |
| ingest | done — `ingest_text` (content→chunks); 24 unit tests |
| api | full HTTP surface: `/health`, content-based `POST /ingest`, curation (`/cards/pending`, accept/reject, `PATCH`) + review (`/cards/due`, `POST /reviews`); 15 integration tests. `POST /reviews/evaluate` planned |
| wire + client | shared DTOs + typed `reqwest` client; `wiremock` unit tests + `tests/client.rs` lifecycle vs a real spawned server. `EvaluateRequest`/`EvaluateResponse` DTOs planned |
| cli (`sb`) | `push`/`curate`/`review` over the client (injected-I/O logic in `cli.rs`, clap shell in `bin/sb.rs`); `tests/cli.rs` acceptance |
| config | `src/config.rs` — `[server]`, `[store]`, `[llm]` sections; `STUDYBUDDY_CONFIG` env override; deny-unknown-fields; 5 unit tests |
| llm | trait + types + `OllamaProvider` + `RetryingProvider` + prompt builder; config-file wired; cloud providers not built |
| store | `Repository` trait + in-memory (10 unit) + file backend (6 acceptance), sha256 sidecars under the data dir; wired into `AppState` |
| watcher | feeder binary pushes a vault via `Client::ingest` (`discover_notes`); walkers in `src/watcher.rs`, 11 acceptance tests. Change-detection + `notify` live-watching + reconciliation not yet built |

Next planned steps: concrete cloud `LlmProvider` impls (design in [llm.md](llm.md)), then the watcher's change detection + `notify` live-watching + reconciliation protocol (the feeder reports deletions/manifests so the server can flag orphans/stale).

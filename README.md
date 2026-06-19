# StudyBuddy

A self-hosted local HTTP server that ingests your markdown notes (Obsidian-compatible), uses an LLM to propose flashcards, lets you curate them, and schedules reviews with spaced repetition. Backend-first: future frontends are clients of the HTTP API.

> **Status:** early implementation. The full HTTP surface is up and tested — `POST /ingest` (push a note → LLM proposes cards), curation (`/cards/pending`, accept/reject, edit), and review (`/cards/due`, `/reviews`) backed by an SM-2 scheduler and a file store. Concrete cloud LLM providers and the watcher's live push/reconciliation are still to come. See [`DESIGN.md`](DESIGN.md) and [`docs/`](docs/).

## Run the server

There are two binaries (`studybuddy` and `watcher`), so name the one you want:

```bash
cargo run --bin studybuddy
```

It listens on `127.0.0.1:8080`. Useful env vars:

```bash
# where cards/state/reviews are written (default: ./studybuddy-data)
# log level via tracing's EnvFilter
STUDYBUDDY_DATA_DIR=./my-data RUST_LOG=debug cargo run --bin studybuddy
```

Smoke-check it's up:

```bash
curl -s localhost:8080/health      # → {"status":"ok"}
```

## Card generation needs Ollama

`src/main.rs` wires the LLM to **Ollama** at `http://127.0.0.1:11434`, model **`gpt-oss:120b-cloud`**. Before `/ingest` can propose cards, Ollama must be running and serving that model:

```bash
ollama serve                       # if not already running
ollama pull gpt-oss:120b-cloud     # or change the model in src/main.rs
```

If Ollama isn't up, `/ingest` still returns `200`, but with `proposed_cards: 0` and `failed_chunks > 0` (the per-chunk LLM calls fail as transient).

## Submit a note for card generation

`POST /ingest` takes the note's **content**, not a path — `{ source_file, content }`. `source_file` is the note's vault-relative path (no leading `/`, no `..`); it becomes the card's source anchor.

Inline content:

```bash
curl -s -X POST localhost:8080/ingest \
  -H 'content-type: application/json' \
  -d '{"source_file":"linear-algebra/vectors.md","content":"# Vectors\n\nA vector has both magnitude and direction. The dot product of two vectors multiplies their corresponding components and sums them into a scalar."}'
```

From a file on disk (use `jq` to JSON-encode the content safely):

```bash
jq -n --arg sf "linear-algebra/vectors.md" --rawfile c ./notes/vectors.md \
  '{source_file:$sf, content:$c}' \
| curl -s -X POST localhost:8080/ingest -H 'content-type: application/json' -d @-
```

Response:

```json
{ "chunks": 1, "proposed_cards": 3, "failed_chunks": 0, "skipped_chunks": 0 }
```

## Curate and review

```bash
# list cards awaiting curation
curl -s localhost:8080/cards/pending | jq

# accept a card into the review pool (due immediately), or reject it
curl -s -X POST localhost:8080/cards/<id>/accept      # → 204
curl -s -X POST localhost:8080/cards/<id>/reject      # → 204

# edit a pending card's content before accepting (409 if already accepted)
curl -s -X PATCH localhost:8080/cards/<id> -H 'content-type: application/json' \
  -d '{"content":{"type":"qa","front":"...","back":"..."}}'

# run a review session
curl -s localhost:8080/cards/due | jq
curl -s -X POST localhost:8080/reviews -H 'content-type: application/json' \
  -d '{"card_id":"<id>","rating":"good"}'    # rating: again | hard | good | easy
```

> The **watcher** (`cargo run --bin watcher <dir>`) is currently a skeleton — it walks a directory and reports what it found, but doesn't push to the server yet. For now, feed notes in via `curl` (or any HTTP client).

## Build / test

```bash
cargo build              # build everything
cargo test               # run all tests
cargo clippy --all-targets
cargo fmt
```

## Docs

- [`DESIGN.md`](DESIGN.md) — vision and load-bearing decisions (read before non-trivial changes)
- [`docs/architecture.md`](docs/architecture.md) — subsystem map, traits, data shapes, what's built vs planned
- [`docs/api.md`](docs/api.md) — full HTTP API contract and per-endpoint flows
- [`docs/llm.md`](docs/llm.md) — LLM provider design
- [`CLAUDE.md`](CLAUDE.md) — build/test cheat sheet and constraints

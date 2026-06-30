# StudyBuddy

A self-hosted local HTTP server that ingests your markdown notes (Obsidian-compatible), uses an LLM to propose flashcards, lets you curate them, and schedules reviews with spaced repetition. Backend-first: future frontends are clients of the HTTP API.

> **Status:** early implementation. The full HTTP surface is up and tested — `POST /ingest` (push a note → LLM proposes cards), curation (`/cards/pending`, accept/reject, edit), review (`/cards/due`, `/reviews`), and LLM-graded free-text answer evaluation (`POST /reviews/evaluate`) — all backed by an SM-2 scheduler and a file store. An `sb` CLI drives all of it (see below), and the `watcher` pushes a vault to the server. Concrete cloud LLM providers and the watcher's change-detection / live-watching are still to come. See [`DESIGN.md`](DESIGN.md) and [`docs/`](docs/).

## Run the server

There are three binaries — the `studybuddy` server, the `sb` CLI, and the `watcher` feeder — so name the one you want:

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

## Drive it with the `sb` CLI

`sb` is a thin client over the HTTP API — the easiest way to use the server day to day, no `curl` or hand-copied UUIDs. It targets `http://127.0.0.1:8080` by default (override with `--server` or `$STUDYBUDDY_SERVER`).

```bash
# push one note for card generation
# (its anchor is the path relative to --vault, which defaults to the cwd)
cargo run --bin sb -- push --vault ./notes ./notes/linear-algebra/vectors.md

# curate the proposals interactively:
#   [a]ccept · [r]eject · [e]dit (opens $EDITOR on the card JSON) · [s]kip · [q]uit
cargo run --bin sb -- curate

# run a review session:
#   Q&A cards: type your answer and press Enter — the LLM grades it, shows a
#   verdict (Correct/Partial/Incorrect) and explanation, reveals the expected
#   answer, and pre-highlights a suggested rating. Press Enter without typing
#   to skip evaluation and just reveal the answer.
#   Cloze cards: Enter reveals the filled text, then rate as usual.
#   Rate 1–4 (again/hard/good/easy) to record the review.
cargo run --bin sb -- review
```

Tip: `cargo install --path . --bin sb` puts `sb` on your `PATH`, so you can drop the `cargo run --bin sb --` prefix.

To push a whole vault at once, use the `watcher` (below). For the raw HTTP contract behind each command — request/response shapes, status codes, and per-endpoint flows — see [`docs/api.md`](docs/api.md).

## Push a whole vault with the watcher

`cargo run --bin watcher <vault-dir>` discovers every `.md` note under the directory (skipping hidden dirs like `.git`/`.obsidian`) and pushes each to `/ingest` through the same client `sb push` uses — letting the server chunk. It targets `$STUDYBUDDY_SERVER` (default `http://127.0.0.1:8080`).

```bash
cargo run --bin watcher ./notes
```

This is a one-shot full sweep today; content-hash change detection (push only what changed) and `notify`-based live watching are still to come.

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

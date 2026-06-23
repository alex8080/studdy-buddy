# CLI (`sb`) — design

A terminal client for driving the StudyBuddy server: push a note for card
generation, curate the proposed cards, and run a spaced-repetition review
session — without hand-writing `curl` and copy-pasting UUIDs.

See [`../DESIGN.md`](../DESIGN.md) for *why* the system is backend-first (future
frontends, this CLI included, are clients of the HTTP API) and
[`api.md`](api.md) for the endpoint contracts the CLI consumes.

**Status:** built. `sb push`/`review`/`curate` and the watcher rewire to
`Client::ingest` are implemented and tested (lib unit + wiremock + real-server
acceptance). This doc describes the shipped design.

## Why a CLI

The server's HTTP surface is complete, but two of its core flows — **curation**
and **review** — are interactive loops that `curl` handles badly (read JSON,
copy an id, issue the next request, repeat). The CLI makes the server actually
usable day-to-day before any web UI exists, and doubles as a living integration
test of the API.

It stays a **thin client**: HTTP + terminal I/O only. No card logic, no
chunking, no scheduling — those live in the server. The CLI must never become a
second place domain logic lives.

## Shape

```
   sb (CLI) ─────┐
                 │  reqwest
   watcher ──────┤  ── HTTP ──►  studybuddy server (axum)
                 │
            src/client.rs  ◄── the one place that talks to the server
```

The HTTP client lives in the **lib** (`src/client.rs`), so both the `sb` binary
and the watcher's future HTTP push call the *same* `Client::ingest`. The watcher
keeps owning discovery + change-detection + `notify`; the "send this file" step
is the shared client. `sb push` is essentially the watcher's push step,
triggered manually for one file.

## Module layout

```
src/
  wire.rs        # shared request/response DTOs (used by api.rs handlers AND client)
  client.rs      # Client: typed reqwest wrapper, one method per endpoint
  cli.rs         # run_push / run_review / run_curate — command logic, injected I/O
  bin/sb.rs      # thin clap shell: parse args → build Client → call cli::run_*
```

Command logic lives in the **lib** (`cli.rs`), not the bin, because `tests/`
crates can't import a binary. `bin/sb.rs` is a trivial clap shell; everything
meaningful is in `cli.rs` and is directly testable.

### `wire.rs` — shared DTOs

The request/response envelopes currently defined inside `api.rs` move to a
shared module so the server and the client share one definition (no drift).
Each derives both `Serialize` and `Deserialize` (server uses one direction, the
client the other). `Card` / `CardContent` / `Rating` already live in
`model.rs`; `wire.rs` is just the envelopes:

| Type | Body |
|---|---|
| `IngestRequest` | `{ source_file, content }` |
| `IngestResponse` | `{ chunks, proposed_cards, failed_chunks, skipped_chunks }` |
| `CardsResponse` | `{ cards: Vec<Card> }` (pending + due) |
| `UpdateContentRequest` | `{ content: CardContent }` |
| `ReviewRequest` | `{ card_id, rating }` |
| `ReviewResponse` | `{ next_due, interval_days }` |

`api.rs` imports these instead of defining them — a no-behavior-change refactor;
the existing API tests stay green.

### `client.rs` — the shared HTTP client

```
Client::new(base_url)                              // reqwest::Client + base URL
  .ingest(source_file, content) -> IngestResponse  // the push core
  .pending() / .due()           -> Vec<Card>
  .accept(id) / .reject(id)     -> ()
  .patch(id, content)           -> ()
  .review(card_id, rating)      -> ReviewResponse
```

Pure HTTP + serde. No domain logic.

## Commands

Global flag: `--server <url>` (default `http://127.0.0.1:8080`, overridable via
`STUDYBUDDY_SERVER`).

### `sb push [--vault <root>] <file>`

Pushes **exactly one file**. `--vault` defaults to the current directory. The
CLI computes `source_file` = the file's path **relative to the vault root**
(rejecting a file that escapes the root — that would yield a `..` the server
refuses), reads the content, calls `client.ingest`, and prints the counts line
(`chunks / proposed_cards / failed_chunks / skipped_chunks`). Computing the
anchor relative to a vault root mirrors how the watcher will derive it.

One invocation is a 1:1 mirror of the server's one-file-per-request `/ingest`:
one request, one counts line, an exit code that maps directly to that push — no
partial-failure aggregation to reason about. Pushing several files by hand is a
shell loop (`for f in notes/*.md; do sb push --vault . "$f" || break; done`);
bulk and recursive ingestion stay the watcher's job (its whole reason to
exist).

### `sb review`

Drives a review session from `GET /cards/due`. Per card:

1. show the question,
2. wait for Enter to reveal,
3. show the answer,
4. read a rating key — `1`/`2`/`3`/`4` → `again`/`hard`/`good`/`easy`,
5. `POST /reviews`, print the next interval.

Loops until the due queue is empty. The key→`Rating` mapping is a pure,
unit-tested function.

### `sb curate`

Walks `GET /cards/pending`. Per card, show the content and read one of:
`a`ccept · `r`eject · `e`dit · `s`kip · `q`uit.

**Edit** opens the card's `content` as pretty JSON in `$EDITOR`, parses the
result on save, and `PATCH`es it (re-prompting on parse error). JSON handles
both `Qa` and `Cloze` uniformly. The editor invocation is an **injected
closure** (`Fn(&CardContent) -> Result<CardContent>`) — the real bin passes the
`$EDITOR`-spawning impl; tests pass a closure that returns edited content, so
the edit path needs no subprocess and no fake-`$EDITOR` script.

## Testing

Per the repo's split (see [`../CLAUDE.md`](../CLAUDE.md)): synthetic edge cases
in unit tests; high-level behavior through the public API in acceptance tests.

Because the CLI makes *real* HTTP (unlike `tests/api.rs`, which uses
`tower::oneshot` with no socket), the acceptance tests stand up a real server:
a shared `tests/common/mod.rs` harness moves `FakeLlmProvider` out of
`tests/api.rs` and adds `spawn_server()` — bind `127.0.0.1:0`,
`tokio::spawn(axum::serve(...))` over an in-memory store + fake LLM, return the
base URL.

**Unit (in `src/`, no socket):**
- `client.rs` — request shaping against `wiremock` (each method hits the right
  path / verb / body).
- `cli.rs` — pure helpers: `rating_from_key`, `vault_relative(root, file)`
  including escaping-path rejection.

**Acceptance — `tests/client.rs`:** `Client` against a real spawned server; the
full lifecycle push → pending → accept → due → review → patch. This is the
**wire-contract test** — the reason `wire.rs` exists; it catches client/server
drift that `oneshot` tests can't.

**Acceptance — `tests/cli.rs`:** call `cli::run_*` with scripted `Cursor<&[u8]>`
stdin and a `Vec<u8>` stdout against a spawned server, then assert on both the
captured output **and** the resulting server state (e.g. after `run_curate`
accept the card appears in `due()`; after `run_review` the card's `next_due`
advances). The edit path uses a test closure for the editor. Happy-path /
high-level only — edge cases live in the unit layer.

No subprocess-level test: the `bin/sb.rs` shell is trivial and clap validates
its own args, so the injected-I/O tests cover everything meaningful.

## Build order

Each step lands with its tests and a green `build` / `test` / `clippy`.

1. Extract `wire.rs`; refactor `api.rs` to use it (no behavior change — existing
   tests pass).
2. `client.rs` + wiremock unit tests + `tests/common` harness + `tests/client.rs`
   lifecycle.
3. `cli.rs` `run_push` + helpers (unit) + `tests/cli.rs` push case.
4. `run_review` + `tests/cli.rs` review case.
5. `run_curate` (+ editor closure) + `tests/cli.rs` curate accept / reject / edit.
6. `bin/sb.rs` clap shell; rewire `src/bin/watcher.rs` to `Client::ingest`.

## New dependencies

- `clap` (derive) — the only genuinely new dependency.
- HTTP client: none — `reqwest` is already present.
- Temp file for the editor: none — `std::env::temp_dir`.

## Constraints honored

- **Thin client of the HTTP API** — no domain logic in the CLI (DESIGN:
  backend-first).
- **One place talks to the server** — `client.rs` in the lib, shared with the
  watcher, so there's no second feeder reimplementing the push.
- **No drift** — server and client share `wire.rs`; the lifecycle acceptance
  test fails if they diverge.

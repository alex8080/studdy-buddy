# HTTP API

Self-hosted local server on `127.0.0.1:8080`. All endpoints return JSON. v1 is single-user with no auth (see [`../DESIGN.md`](../DESIGN.md) for why).

For per-subsystem internals, see [architecture.md](architecture.md).

## Conventions

- All request and response bodies are JSON (`Content-Type: application/json`).
- IDs are UUIDv4 strings.
- Timestamps are RFC 3339 / ISO 8601 (`chrono::DateTime<Utc>`).
- Errors come back as `{ "error": "<message>" }` with the appropriate HTTP status code: `400` (bad request / invalid `source_file` / malformed body), `404` (unknown card), `409` (conflict — e.g. editing a non-`Pending` card), `500` (I/O, store, or `LlmError::Config`).
- Endpoints are listed in roughly the order they'll be implemented; see each one's **Status** line.

---

## `GET /health` — built

Liveness probe. Used for the smoke test in `CLAUDE.md`.

**Response 200:**

```json
{ "status": "ok" }
```

No side effects. No internal flow.

---

## `POST /ingest` — built

Ingest **one pushed note**. The server never reads the filesystem: a feeder (the [watcher](architecture.md), a manual upload, a CLI) sends a note's vault-relative path and raw markdown, and the server parses → chunks → proposes cards → persists them as `Pending`. Synchronous in v1; one file per request keeps each request short (no whole-vault long-poll). See [llm.md](llm.md#sequential-and-synchronous-for-v1).

**Request:**

```json
{ "source_file": "linear-algebra/vectors.md", "content": "# Vectors\n\n..." }
```

`source_file` is the note's path **relative to the vault** — it becomes the card's `(source_file, source_heading)` anchor. It must be relative (no leading `/`, no `..`); absolute or traversing paths are rejected `400`.

**Response 200:**

```json
{ "chunks": 3, "proposed_cards": 5, "failed_chunks": 0, "skipped_chunks": 1 }
```

| Field | Meaning |
|---|---|
| `chunks` | Chunks produced from this note. `0` if the note's frontmatter sets `studybuddy.exclude: true`. |
| `proposed_cards` | Cards the LLM returned across all successful chunks (saved as `Pending`). |
| `failed_chunks` | Chunks where the LLM call failed with `Transient` after retries exhausted. |
| `skipped_chunks` | Chunks the LLM declined to produce cards for (`BadInput`: refusal, context too long, malformed output). |

Best-effort over chunks: a per-chunk LLM failure doesn't abort the note. Per-chunk details (heading, error reason) go to the server log, not the response. See [llm.md](llm.md#per-chunk-handling-at-the-api-layer).

**Internal flow:**

```
POST /ingest { source_file, content }
   │
   ├─► validate source_file (relative, no `..`)  → else 400
   ├─► ingest_text(content, source_file)         → Vec<ChunkContext>  (empty if excluded)
   │
   ├─► for each chunk:
   │     match llm.propose_cards(chunk).await {
   │       Ok(cards)         → build Card{Pending, anchor, tags}; proposed_cards += cards.len()
   │       Err(Transient(_)) → failed_chunks  += 1   // log warn, continue
   │       Err(BadInput(_))  → skipped_chunks += 1   // log debug, continue
   │       Err(Config(e))    → return 500            // abort note
   │     }
   │
   ├─► store.save_pending(cards)
   └─► return counts
```

**Errors:**

- `400` — `source_file` empty, absolute, or containing `..`.
- `500` — `LlmError::Config` (broken model name, auth failure — fails identically for every chunk, so we don't keep calling), or a store write error.

**Errors come back as:** `{ "error": "source_file must be a relative path without '..': ../x.md" }`

**Status:** live and wired end-to-end (validation → `ingest_text` → LLM → `save_pending`). Nine integration tests in `tests/api.rs` cover the counts path, exclusion, each LLM-error category, and both `400` validations. The directory **walking** that used to live here now belongs to the watcher feeder (`src/watcher.rs` + `src/bin/watcher.rs`); the server keeps only `ingest_text`.

---

## `GET /cards/due` — built

Returns cards whose `next_due ≤ now`. Frontends use this to drive review sessions; the backend is intentionally cadence-agnostic (see DESIGN.md). Cards serialize in full (the whole stored `Card`, including `status` and `created_at`).

**Response 200:**

```json
{
  "cards": [
    {
      "id": "uuid",
      "content": { "type": "qa", "front": "...", "back": "..." },
      "source_file": "linear-algebra/vectors.md",
      "source_heading": "Vectors > Dot Product",
      "tags": ["math"],
      "status": "accepted",
      "created_at": "2026-06-22T00:00:00Z"
    }
  ]
}
```

**Internal flow:**

```
GET /cards/due
   └─► store.list_due(now)                       → Vec<Card>
```

`now` is the server's `Utc::now()`. Filtered to `CardStatus::Accepted`, so a card rejected after acceptance (which may still carry a lingering state entry) doesn't resurface. `GET /cards/pending` has the same `{ "cards": [...] }` shape over `CardStatus::Pending`.

---

## `POST /reviews` — built

Record a user's review of a card. Updates SRS state and `next_due`.

**Request:**

```json
{ "card_id": "uuid", "rating": "good" }
```

Valid `rating` values: `again`, `hard`, `good`, `easy`.

**Response 200:**

```json
{ "next_due": "2026-06-23T00:00:00Z", "interval_days": 4 }
```

`404` if the card has no SRS state (never accepted, or unknown id).

**Internal flow:**

```
POST /reviews { card_id, rating }
   │
   ├─► store.load_state(card_id)                 → SchedulerState   (404 if absent)
   ├─► scheduler.on_review(state, rating, now)   → ReviewOutcome { state, next_due }
   ├─► store.save_review(card_id, rating, next_due)
   └─► store.save_state(card_id, outcome.state, outcome.next_due)
```

---

## `POST /reviews/evaluate` — built

Evaluate a user's free-text answer against a card before rating. Called by the
web frontend when the user types an answer and clicks "Submit". The server asks
the LLM to compare the user's answer to the card content and returns a verdict
plus a suggested rating. The user always submits the final rating separately via
`POST /reviews` — this endpoint is advisory only.

**Request:**

```json
{ "card_id": "uuid", "user_answer": "The dot product of two vectors produces a scalar..." }
```

**Response 200:**

```json
{
  "verdict": "correct",
  "explanation": "The user correctly identified that the dot product produces a scalar result.",
  "suggested_rating": "good"
}
```

| Field | Values |
|---|---|
| `verdict` | `"correct"` / `"partial"` / `"incorrect"` |
| `explanation` | Short LLM-generated rationale shown to the user |
| `suggested_rating` | Default mapping: `correct → good`, `partial → hard`, `incorrect → again` |

The suggested rating pre-highlights a button in the UI; the user may override it.

**Errors:**

- `404` — unknown `card_id`.
- `503` — LLM call failed (`Transient` after retries exhausted, or `BadInput`). Body: `{ "error": "LLM evaluation unavailable: <reason>" }`. The frontend shows this message and enables all four rating buttons without a suggestion so the session continues.
- `500` — `LlmError::Config` (misconfigured provider) or store I/O error.

**Internal flow:**

```
POST /reviews/evaluate { card_id, user_answer }
   │
   ├─► store.get_card(card_id)                  → Card   (404 if absent)
   ├─► match card.content {
   │     Cloze → fuzzy_match(user_answer, cloze_fills) → EvaluationResult  // no LLM
   │     Qa    → llm.evaluate_answer(&card, &user_answer) → EvaluationResult
   │               Err(Transient(_)) / Err(BadInput(_))  → 503
   │               Err(Config(_))                        → 500
   │   }
   └─► return { verdict, explanation, suggested_rating }
```

For Q&A evaluation the LLM prompt includes the card question, the expected
answer, and the user's answer; the LLM is explicitly instructed to evaluate
against the card's expected answer (not world knowledge) and to accept
paraphrasing as correct.

**Status:** built. `store.get_card`, `LlmProvider::evaluate_answer`, wire DTOs,
and the handler are all implemented. Cloze uses fuzzy match; Q&A uses the LLM.
Integration tests in `tests/api.rs`.

---

## Curation endpoints — built

The flow that turns LLM-proposed cards into reviewable ones. `accept`, `reject`, and `PATCH` return `204 No Content` on success.

| Method + path | Purpose |
|---|---|
| `GET /cards/pending` | List cards in `CardStatus::Pending` for the user to review |
| `POST /cards/{id}/accept` | Move a pending card into the SRS pool (`Accepted`) |
| `POST /cards/{id}/reject` | Drop a pending card |
| `PATCH /cards/{id}` | Edit a `Pending` card's `content` (curation fix-up — see below) |

**Internal flow (accept):**

```
POST /cards/{id}/accept
   │
   ├─► store.set_status(id, Accepted)
   └─► store.save_state(id, SchedulerState::default(), now)   // seed initial SRS state; due immediately
```

**`PATCH /cards/{id}` — edit a pending card**

Content-only, and only while the card is `Pending`. This is the curation fix-up path: the LLM's proposal is close but needs a tweak before it enters the SRS pool. Without it, the user's only options are accept-as-is or reject (re-authoring by hand).

Out of scope for v1: editing **accepted** cards. That forces SRS-reset semantics (does a content change invalidate scheduling history?) and reconciliation "is this stale?" decisions that the watcher — not yet built — hasn't designed. We add it as a fast-follow once the watcher forces those choices anyway.

Not editable here: the `(source_file, source_heading)` anchor (it's the source link), `tags` (note-inherited), and `status` (moves via accept/reject).

**Request:**

```json
{ "content": { "type": "qa", "front": "...", "back": "..." } }
```

**Internal flow:**

```
PATCH /cards/{id} { content }
   │
   ├─► 409 if the card's status isn't Pending     (edits are curation-only)
   └─► store.update_content(id, content)
```

The store enforces the `Pending` guard (it reads-to-write anyway), returning `AppError::Conflict` → `409`. A missing card is `404`.

**Status:** all four endpoints are live, wired to the store (and the SM-2 scheduler for `/reviews`). Integration tests in `tests/api.rs` cover the full lifecycle (ingest → pending → accept → due → review), the PATCH edit + 409-after-accept, reject, and review-of-unknown-card → 404.

---

## User-managed card endpoints — planned

For manual creation/deletion of cards outside the LLM-proposed flow.

| Method + path | Purpose |
|---|---|
| `GET /cards` | List all cards (filterable by tag, status) |
| `POST /cards` | Create a card manually; `source_file`/`source_heading` are optional here |
| `DELETE /cards/{id}` | Hard-delete a user-managed card. Note: cards proposed by the LLM are flagged (`Orphaned`/`Rejected`), not deleted, so this only applies to manual cards. |

---

## Reconciliation endpoints — planned

For when the watcher (not yet built) flags cards as stale or orphaned and the user wants to act on them.

| Method + path | Purpose |
|---|---|
| `GET /cards/stale` | Cards whose source note changed; user decides keep/edit/regenerate |
| `GET /cards/orphaned` | Cards whose source note was deleted; user decides keep/remove |
| `POST /cards/{id}/keep` | Mark a flagged card as still-valid |

The reconciliation policy itself lives in the (future) `watcher` subsystem; these endpoints just expose the queue to the frontend.

# StudyBuddy — Design Document

## Overview

StudyBuddy helps a learner retain new material by turning their own notes into
flashcards and scheduling reviews with spaced repetition. The user points it at
a directory of markdown notes (e.g. a folder inside an Obsidian vault);
StudyBuddy uses an LLM to propose flashcards, lets the user curate them, and
then schedules reviews so the user is quizzed just before they would forget.

The differentiator: **bring your own notes**. Users keep writing in Obsidian (or
any markdown editor) and StudyBuddy reads their existing files — no migration,
no separate note-taking app.

## Goals

- Take a directory of markdown files as input and propose high-quality cards
  from them.
- Let the user curate proposed cards: accept, edit, reject, or add their own.
- Schedule reviews with a spaced-repetition algorithm.
- Keep cards linked to their source note so the user can jump back to context.
- Stay in sync as notes change.

## Non-goals (v1)

- Augmenting cards with content from the web. Source of truth is the user's
  notes only.
- Multi-user / SaaS. v1 is single-user, self-hosted.
- Mobile-first UX. Frontends can come later; the server is what we ship first.
- Full Obsidian feature parity (graph, embeds, plugin compatibility).
- Notification scheduling, daily-session UX, reminder emails. These are
  frontend concerns.

## Positioning

| Tool | Notes live in | AI card generation | SRS | Curation loop |
|---|---|---|---|---|
| Anki | — (cards only) | No (manual or plugin) | Yes (FSRS) | N/A |
| RemNote | RemNote's outliner | Yes | Yes | Yes |
| Obsidian-to-Anki | Obsidian | No (manual tags) | Via Anki | Manual |
| **StudyBuddy** | **Any markdown directory** | **Yes** | **Yes** | **Yes** |

The gap StudyBuddy fills: *Obsidian-native, AI-proposed, user-curated, with SRS
built in.* No existing tool does all four.

## Architecture

**Self-hosted local web server with an HTTP API.** Frontends — the `sb` CLI
today, a web UI next, others later — are clients of this API. So are the
*feeders* that supply notes: the server never reads the vault filesystem itself,
it receives pushed note content. The server runs on the user's machine (e.g.
`localhost:8080`) and keeps cards and SRS state in a local file-based store.

```
                 read       +------------------+
+-----------------+ ------> |  feeder          |
|  Markdown dir   |         |  (watcher, or    |
|  (Obsidian      |         |   sb push)       |
|   folder)       |         +--------+---------+
+-----------------+                  | HTTP push
                                     | POST /ingest {source_file, content}
                                     v
                            +------------------+   HTTP   +----------------+
                            |  StudyBuddy      | <------> |  Frontends     |
                            |  local server    |         |  (sb CLI now,  |
                            |  (API + SRS +    |         |   web UI next) |
                            |   LLM adapter)   |         +----------------+
                            +--------+---------+
                                     |
                                     v
                            +------------------+
                            |  Local store     |
                            |  (files: cards,  |
                            |   state, reviews)|
                            +------------------+
```

The `sb` CLI is both: it pushes notes (a feeder) *and* drives curation/review (a
frontend). The watcher is feeder-only.

### Why backend-first

- Notes are local; running the server locally means no upload, no sync agent,
  no privacy concern.
- A single API decouples SRS/LLM logic from any specific UI.
- Frontends (the `sb` CLI today; web, mobile, IDE plugins later) don't require
  rewriting the core.

## Core flows

### 1. Ingest

1. A feeder is pointed at the source directory — the `watcher` for a whole
   vault, or `sb push` for a single file.
2. The feeder discovers `.md` files and pushes each note's content to the server
   (`POST /ingest { source_file, content }`); the server never reads the
   filesystem.
3. The server parses the pushed markdown. Obsidian-specific syntax: `#tags` and
   YAML frontmatter are extracted as signal; wikilinks and callouts are stripped
   to plain text. Frontmatter may carry per-file config (e.g.
   `studybuddy: { exclude: true }`).
4. Content is chunked (heading-based) and sent to the configured LLM for card
   proposals.
5. Proposed cards land in a "pending review" queue.

### 2. Curate

For each pending card the user can: **accept**, **edit**, **reject**, or
**create from scratch**. Cards keep an anchor `(source_file, heading)` so the
user can open the originating context.

The LLM picks the card format per chunk:
- **Q&A** for concepts, definitions, relationships.
- **Cloze** for terminology and quoted passages where preserving the original
  wording matters.

### 3. Review (SRS)

Accepted cards enter the SRS scheduler. On each review the user rates the card
(Again / Hard / Good / Easy), and the scheduler updates the next-review date.

The server exposes a `GET /cards/due` endpoint that returns the cards due now.
Frontends decide when and how to surface them — daily session, on-demand,
browser notifications, etc.

**Free-text answer evaluation (web UI).** The web frontend lets the user type a
free-text answer before seeing the correct one. When the user types anything and
clicks "Submit", the client calls `POST /reviews/evaluate { card_id,
user_answer }`. The server evaluates based on card type:

- **Q&A cards** — LLM compares the user's answer to the card back, evaluating
  for conceptual equivalence (not exact wording). The LLM prompt is anchored to
  the card's expected answer, not its own world knowledge.
- **Cloze cards** — deterministic fuzzy match against the cloze fill(s); no LLM
  call.

While evaluating, the answer stays hidden and the rating buttons are disabled.
On success the frontend reveals the correct answer, shows the verdict (`correct`
/ `partial` / `incorrect`) and an explanation, and pre-highlights the suggested
rating button — but the user always makes the final rating call via `POST
/reviews`. If the LLM evaluation fails (Q&A only), the frontend shows an error
message and enables all rating buttons without a suggestion so the session can
continue unblocked. If the user skips typing and clicks "Reveal" directly, the
evaluate step is omitted and the user self-rates as usual.

### 4. Note sync (reconciliation)

The watcher feeder watches the source directory and pushes changes; the server
reconciles (still to build). When a note changes:
- New/changed chunks → new card proposals into the pending queue.
- Cards whose source heading was removed → flagged **orphaned**, not deleted.
  User decides whether to keep, edit, or remove.
- Cards whose source content changed materially → flagged **stale** for review.

This keeps cards trustworthy without surprising the user with silent deletions.

## Key design decisions

| Decision | Choice | Rationale |
|---|---|---|
| Ingest model | Feeder **pushes** note content to the server (`POST /ingest { source_file, content }`), one file per request; the server never reads the vault filesystem | Decouples the server from the filesystem (any feeder can push), keeps each request short, and is forward-compatible with a hosted mode. Directory walking lives in a separate watcher feeder. |
| Server model | Self-hosted local | No infra, no privacy concerns. |
| LLM | Pluggable: cloud (user-supplied API key) **or** local (Ollama) | Cloud for quality, local for privacy. User picks. |
| Card format | Both Q&A and cloze; LLM picks per card | Best learning outcome; small UI cost to render two types. |
| SRS algorithm | **SM-2** for v1, behind a `Scheduler` interface | Simple, debuggable; FSRS can drop in once we have review data. |
| Rating type | `Rating { Again, Hard, Good, Easy }` | Matches Anki UI labels and the `fsrs-rs` crate's enum; keeps the FSRS swap trivial. `Again` is technically on a different axis than the other three (recall-or-not vs. how-well), but ecosystem alignment wins over semantic purity. |
| Answer evaluation | **Q&A cards**: LLM-graded via `POST /reviews/evaluate`; prompt anchored to card back (not world knowledge), evaluates conceptual equivalence. **Cloze cards**: deterministic fuzzy match on the fill — no LLM. Returns verdict + suggested rating, never forces it. | LLM is warranted only for free-text Q&A answers where paraphrase detection matters; cloze fills are short and specific enough for string matching. Gives the user a nudge without removing agency. Evaluate step is optional; clients that skip it (CLI, direct reveal) use the four-button self-rate flow as before. |
| Card ↔ source | Every card stores `(file, heading)` anchor | Cheap now, painful to retrofit. |
| Sync | A separate watcher feeder watches the vault and pushes changes; the server reconciles, flagging orphans/stale, never auto-deleting | Cards stay fresh without destroying user work. Because the server can't re-walk the disk, the feeder reports deletions/manifests — sync protocol deferred to the watcher build. |
| Storage | v1: file-based behind a `Repository` trait in a configured server data dir — cards one-file-per-note (filename = `sha256(source_file)`), plus one `state.json` and one append-only `reviews.jsonl` | Store holds *derived* data at single-user scale; files stay inspectable, no DB dependency. The path arrives as untrusted input, so the sidecar name is hashed (flat, traversal-proof). Swap to SQLite/Postgres behind the trait if a multi-tenant hosted mode arrives. |
| Obsidian syntax | Use `#tags` + frontmatter as signal; strip the rest | Cheap wins (tag-based filtering) without a full parser. |
| Web augmentation | Out of scope v1 | Blurs source of truth; risks hallucination. Revisit as opt-in "expand topic" later. |

## Data model (sketch)

```
Note
  path             # relative to source dir
  hash             # content hash, for change detection
  frontmatter      # parsed YAML
  tags             # extracted #tags

Card
  id
  type             # 'qa' | 'cloze'
  front / back     # for qa
  text / cloze_spans  # for cloze
  source_file
  source_heading
  tags             # inherited from source note
  status           # 'pending' | 'accepted' | 'orphaned' | 'stale' | 'rejected'

Review
  card_id
  reviewed_at
  rating           # again | hard | good | easy
  next_due
  sm2_state        # ease, interval, repetitions
```

## v1 scope (what we ship)

1. Local HTTP server that ingests pushed note content + parses markdown (with
   Obsidian tag/frontmatter handling).
2. A watcher feeder that walks/watches a vault and pushes notes to the server.
3. Pluggable LLM adapter: cloud (Anthropic / OpenAI via user key) and local
   (Ollama).
4. Card proposal → curation queue → accepted-card store.
5. SM-2 scheduler behind a `Scheduler` interface.
6. Reconciliation (orphan/stale flagging) driven by the feeder's change reports.
7. An `sb` CLI client that drives push, curation, and review over the HTTP API.
8. A minimal web UI to drive curation and review.

## Open for later

- Frontend cadence/UX (notifications, daily session shape).
- FSRS migration once we have real review data.
- Optional web augmentation as explicit "expand this topic" action.
- Multi-device sync (would require a hosted mode or a sync protocol).
- Multi-tenant hosted service (would force a real database behind the `Repository` trait, replacing the v1 file backend).
- Sharing decks between users.

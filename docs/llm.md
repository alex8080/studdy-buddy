# LLM provider

The `llm` subsystem turns ingested chunks into proposed flashcards. It sits behind a trait so cloud providers (Anthropic, OpenAI) and local providers (Ollama) are interchangeable. v1 ships the Ollama provider first; cloud providers follow without touching the trait or the API handler.

This document captures the design decisions made before implementation. For *what's built vs planned*, see [architecture.md](architecture.md); for the HTTP contract the handler exposes, see [api.md](api.md).

## Trait

```rust
#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn propose_cards(&self, chunk: &ChunkContext)
        -> Result<Vec<ProposedCard>, LlmError>;
}
```

| Type | Role |
|---|---|
| `ChunkContext { source_file, source_heading, tags, text }` | LLM input — produced by `ingest`. |
| `ProposedCard { content, rationale }` | LLM output — feeds the pending-review queue. |
| `LlmError` | Provider-level failure, classified by recovery action (see below). |

`propose_cards` returns `LlmError`, not `AppError`. The API handler maps from one to the other; this keeps providers from depending on HTTP concerns.

A successful call returning `Ok(vec![])` is *not* a failure — it means "this chunk produced no cards." Counted separately from errors in the response.

## Error taxonomy

Three error classes, distinguished by the recovery action they imply.

| Variant | Meaning | Retry? | Handler action |
|---|---|---|---|
| `Transient { reason, retry_after }` | Network blip, 5xx, timeout, rate limit. Will likely succeed on retry. | Yes (retry layer) | Count as `failed_chunks` if retries exhausted, continue ingest. |
| `BadInput { reason }` | Model couldn't produce cards for this chunk (refusal, malformed output, context too long). Same input → same outcome. | No | Count as `skipped_chunks`, continue ingest. |
| `Config { reason }` | Configuration or code bug — wrong model name, malformed request body, auth failure. Will fail identically for every chunk. | No | Abort ingest with HTTP 500. |

Classification rule for provider implementors:

- Comes back the same for every input → `Config`.
- Depends on this chunk → `BadInput`.
- Comes and goes → `Transient`.

"Context too long" is per-chunk → `BadInput`, not `Config`.

`BadInput::reason` carries a short message only. The full model output (truncated if large) is logged at `debug` from inside the provider's mapper — recoverable for debugging without leaking through the error type.

## Retry

Retry is a cross-cutting concern: every provider hits transient failures. We model it as a decorator rather than re-implementing it per provider.

```rust
pub struct RetryingProvider<P> {
    inner: P,
    policy: RetryPolicy,
}

impl<P: LlmProvider> LlmProvider for RetryingProvider<P> {
    // Retries Transient up to policy.max_attempts with backoff;
    // BadInput and Config pass through unchanged.
}
```

- **Provider's job**: one attempt, classify the result. No retries, no sleeping. If the response carries a `Retry-After`, include it in `Transient { retry_after }`.
- **Retry layer's job**: respect `retry_after` if present, otherwise use the policy's backoff. Capped at `policy.max_backoff`. Stops at `policy.max_attempts`.
- **Composed in `main.rs`**: real provider wrapped in `RetryingProvider` before being handed to the router. Tests can wrap or skip the wrapper based on what's being tested.

`RetryPolicy` defaults live inside the struct so they can change without touching the rest of the code; the right values will be picked once we have real usage data.

## Per-chunk handling at the API layer

The `POST /ingest` handler iterates chunks sequentially (see [#sequential](#sequential-and-synchronous-for-v1)). The per-chunk loop is best-effort:

```text
for chunk in chunks {
    match llm.propose_cards(chunk).await {
        Ok(cards)          => proposed_cards += cards.len(),
        Err(Transient(e))  => { failed_chunks += 1;  log::warn(file, heading, e); }
        Err(BadInput(e))   => { skipped_chunks += 1; log::debug(file, heading, e); }
        Err(Config(e))     => return Err(AppError::Llm(e)),  // 500, aborts batch
    }
}
```

- **`Config` is the only error that aborts.** It will fail identically for every chunk; continuing would waste time and (for cloud providers) money.
- **`Transient` after retry exhaustion and `BadInput` never fail the request.** The response counts both. The frontend can branch on `failed_chunks == chunks` if it wants to surface "nothing worked."
- **Per-chunk details live in logs**, not the response. Counts in the body, `(source_file, source_heading, kind, reason)` at the appropriate level in the server log.

## Sequential and synchronous for v1

- **Sequential.** Ollama serializes requests anyway; the cost of parallel calls is real (cloud cost, local GPU contention) and the benefit is small at v1 scale.
- **Synchronous HTTP request.** A 200-chunk vault at 2s/chunk = ~7 minute request. Ugly but functional. A job-id + polling model lands when vaults get large or when the watcher starts ingesting in the background.

Both are explicit v1 simplifications, not load-bearing.

## Card format

v1 generates **Q&A only**. `CardContent::Cloze` requires byte-accurate `spans` into the original chunk text; LLMs can't reliably emit those in one shot. Cloze lands once we have a postprocessor that locates spans in the chunk text from a separate "cloze hint" the model returns.

The trait already returns `ProposedCard { content: CardContent, ... }`, so adding cloze later is additive — no shape changes required.

## Ollama provider (first concrete impl)

`src/llm/ollama.rs` — talks HTTP to a local Ollama server.

- **Endpoint**: `POST {base_url}/api/chat`.
- **Stream**: `false`. One response per call.
- **Format**: a JSON Schema constraining the response body to `{ "cards": [{ "front": str, "back": str, "rationale": str? }] }`. Ollama enforces this server-side for capable models; the provider also validates on parse so older models degrading to `"format": "json"` don't crash the server.
- **Options**: `temperature` (default 0.2 — we want repeatable cards), `num_predict` (max tokens, configurable).
- **Default model**: `gpt-oss:120b-cloud` — a cloud-hosted model accessed via Ollama Cloud. Reliable structured-JSON output, no local GPU required. Local alternatives (`qwen3:8b`, `qwen2.5:7b`) work through the same provider for users who can run models locally; the only thing that changes is the model string in the config. The choice ships in the example config; nothing is hardcoded.

### Local vs. cloud models

Ollama Cloud routes through the same local API (`http://127.0.0.1:11434/api/chat`) once the user has authenticated. The provider doesn't distinguish — cloud models just have a `-cloud` suffix in their name and the local daemon offloads them transparently.

Setup is a one-time user action, not something the server does:

```bash
ollama signin                       # free account
ollama pull gpt-oss:120b-cloud      # or any other model named in studybuddy.toml
```

This means cloud-only users and self-hosters share one provider implementation. Direct cloud API access (`https://ollama.com/api/chat` with a bearer token) is out of scope — it would need a separate provider for the auth header.

### Error mapping

| Source | LlmError variant |
|---|---|
| Reqwest connection error, timeout | `Transient` |
| HTTP 5xx | `Transient { retry_after: header.get("Retry-After") }` |
| HTTP 429 | `Transient { retry_after: ... }` |
| HTTP 400, "context length exceeded" | `BadInput` |
| HTTP 400, malformed request | `Config` (our bug) |
| HTTP 404 (model not pulled) | `Config` |
| Auth failure (cloud providers) | `Config` |
| JSON parse failure, schema mismatch | `BadInput` (model didn't honor `format`) |
| Schema-valid `{"cards": []}` | `Ok(vec![])` — not an error |

### Prompt

Prompt text lives in `src/llm/prompt.rs`. Keeping it isolated lets us iterate without recompiling the rest of the server and makes the prompt itself unit-testable as a pure string builder.

System prompt (sketch — final wording will be tuned):

> You author concise flashcards from study material. Given a chunk of source text, produce 1–5 self-contained Q&A cards. Each card's front is one question; the back is a minimal complete answer (1–3 sentences). Don't invent content not present in the source. If the chunk is unsuitable, return `{"cards": []}`.

User message: `{ "heading": ..., "tags": [...], "text": ... }` as JSON.

## Configuration

All server-side config lives in a single TOML file. Path resolution:

1. `STUDYBUDDY_CONFIG=/path/to/config.toml` env var, if set.
2. `./studybuddy.toml` in the process cwd, otherwise.
3. Built-in defaults if neither file exists — the server starts with no config file.

Full shape (see `studybuddy.toml.example` at the project root):

```toml
[server]
bind = "127.0.0.1:8080"

[store]
data_dir = "./studybuddy-data"

[llm]
provider    = "ollama"
model       = "gpt-oss:120b-cloud"   # or e.g. "qwen3:8b" for local
base_url    = "http://127.0.0.1:11434"
temperature = 0.2
# num_predict = 2048                 # omit to use Ollama's default
# api_key     = ""                   # reserved for future cloud providers
```

Unknown fields and unknown sections are rejected at startup (typos surface immediately rather than silently using the wrong default). All fields have defaults, so the file can be partial — only override what you need.

`main.rs` matches on `llm.provider`, rejects unknown values with a clear error, constructs the appropriate provider, wraps it in `RetryingProvider`, and hands the `Arc<dyn LlmProvider>` to the router via state.

## Module layout

```
src/llm/
  mod.rs       trait, ChunkContext, ProposedCard, LlmError
  retry.rs     RetryingProvider<P>, RetryPolicy
  prompt.rs    prompt text + builders (pure, no I/O)
  ollama.rs    OllamaProvider + OllamaConfig + response types
```

Adding Anthropic or OpenAI later means adding a sibling file; nothing else changes.

## Testing

Per the convention in CLAUDE.md (unit tests in-module for edge cases; acceptance tests in `tests/` for end-to-end behavior):

**Unit tests** (in `src/llm/...`):

- `prompt.rs` — rendered prompt contains heading, tags, text exactly.
- `ollama.rs` — `wiremock`-backed: happy 2-card response, empty `cards: []`, malformed JSON, HTTP 400/404/500/503 each mapped to the right `LlmError` variant.
- `retry.rs` — stub provider with queued responses; verify retry counts, backoff, `Retry-After` respected, `BadInput`/`Config` pass through without retry.

**Acceptance tests** (in `tests/`):

- `tests/api.rs` — drives `POST /ingest` with a `FakeLlmProvider` (canned by chunk content). Covers: counts in response are right; excluded files don't trigger LLM calls; `BadInput` counted as skipped not failed; `Transient` exhausted counted as failed; `Config` aborts with 500.
- `tests/llm_ollama.rs` — `#[ignore]`-d live test against a real Ollama. Gated by `STUDYBUDDY_OLLAMA_LIVE=1`, reads model from config or env override. Asserts ≥1 well-formed `Qa` card with non-empty front/back. Documented in CLAUDE.md as the way to smoke-test Ollama integration.

The retry layer and Ollama provider are tested in isolation; the handler-with-fake-provider tests exercise the integration without touching the network.

## Status

Built. Trait, error taxonomy, `RetryingProvider` with exponential backoff, `OllamaProvider` with structured-JSON output, prompt builder, and config-file parsing (`src/config.rs`, `studybuddy.toml.example`) are all in place. Still to build: `llm::anthropic` and `llm::openai` concrete providers.

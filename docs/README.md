# StudyBuddy design docs

Top-level vision and load-bearing decisions live in [`../DESIGN.md`](../DESIGN.md). These docs go a level deeper — subsystem interfaces, data shapes, HTTP endpoints, and how a request flows through the server.

- [architecture.md](architecture.md) — subsystem map, traits, data shapes, what's built vs planned
- [api.md](api.md) — HTTP API contract and the internal flow each endpoint triggers
- [llm.md](llm.md) — LLM provider design: trait, error taxonomy, retry decorator, Ollama specifics, config

If you're starting a new feature, read DESIGN.md → architecture.md → the relevant endpoint in api.md, then look at the source. CLAUDE.md is the cheat sheet for build/test commands and load-bearing constraints; these docs are the design.

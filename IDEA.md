# AI-Gateway

A self-hosted API gateway that unifies access to multiple LLM providers behind one endpoint, with routing, fallback, and usage analytics.

**Goals:**

- Expose a single OpenAI-compatible endpoint that proxies requests to OpenAI, Anthropic, Google, and local models (Ollama), selected via model-name routing rules.
- Implement automatic failover and retry logic so requests reroute to a backup provider when the primary fails or times out.
- Track per-key token usage and estimated cost in a lightweight SQLite store, with a simple dashboard endpoint for reporting.
- Support response caching and per-key rate limiting to reduce spend and prevent abuse.
- Ship as a single Docker container with env-based configuration, deployable in under five minutes.

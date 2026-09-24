-- Records where each usage observation came from and what shape it is,
-- instead of every `UsageRecorded` row looking identical regardless of
-- origin. `kind` defaults to 'cumulative' because that is the observed,
-- documented behavior of every provider this gateway currently supports
-- (OpenAI, OpenRouter, llama.cpp/llama-server, vLLM, Ollama, Gemini's
-- OpenAI-compat layer): usage is reported once per attempt as a total for
-- the whole response, never as a running per-chunk delta. The column
-- exists so a future provider that violates this convention can be
-- represented without another schema change, not because deltas are
-- expected today.
--
-- See docs/frontend/API_GAPS.md item 2.

ALTER TABLE generation_usage ADD COLUMN source TEXT;
ALTER TABLE generation_usage ADD COLUMN kind TEXT NOT NULL DEFAULT 'cumulative';

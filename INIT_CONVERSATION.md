# Initial Conversation about Project

## Me

I use llama-server, openrouter (main 2, far more freq than others), ollama, ollama-cloud, huggingface api, vllm, transformers (pretty rare, but does/has happened), google ai(not vertex), and may sometimes use others. (Venice.ai, featherless.ai, and xai to name a few)

Is it possible, manageable, and would it be worth it to create a server that I send all my requests to and route it from this server (I'm assuming something like what openrouter is at it's core, just different providers, a normalization/transformation layer, and a data collection/metrics/logging/etc. added if desired)

## LLM

Yes. What you’re describing is usually called an **LLM Gateway**, **AI Gateway**, **Inference Gateway**, or **Model Gateway/Router**.

For the way you use models, I think it would be particularly useful because you have an unusually heterogeneous inference environment: `llama-server`, OpenRouter, Ollama/Ollama Cloud, Hugging Face, vLLM, Gemini, and occasional direct providers.

The useful mental model is:

```text
                    ┌──────────────────────────┐
Your applications → │     Your LLM Gateway     │
                    │                          │
                    │ normalization            │
                    │ model registry           │
                    │ routing                  │
                    │ fallbacks                │
                    │ logging / metrics        │
                    │ cost accounting          │
                    │ retries / rate limits    │
                    └────────────┬─────────────┘
                                 │
         ┌───────────────┬───────┼────────┬──────────────┐
         ▼               ▼       ▼        ▼              ▼
    llama-server      OpenRouter Ollama  Gemini     Hugging Face
         │                          │                     │
    local models                Ollama Cloud          providers
         │
        vLLM
```

And the good news is that **this is substantially easier to build today than it would have been a few years ago**, because a large part of your provider set already converges around OpenAI-compatible APIs. `llama-server` exposes OpenAI-compatible Chat Completions, Responses, embeddings and other routes; vLLM has Chat Completions, Responses, embeddings and more; Gemini has an OpenAI-compatibility API; and Hugging Face exposes an OpenAI-compatible inference endpoint. ([GitHub][1])

## The architecture I'd use

I would **not** make OpenAI's request schema your actual internal architecture.

Expose OpenAI compatibility externally:

```text
POST /v1/chat/completions
POST /v1/responses
POST /v1/embeddings
GET  /v1/models
```

so almost any application can point at:

```text
http://localhost:8999/v1
```

instead of:

```text
https://openrouter.ai/api/v1
http://localhost:8080/v1
http://localhost:11434/v1
https://generativelanguage.googleapis.com/...
...
```

But internally I'd use your own canonical objects:

```text
GatewayRequest
    model
    messages/input
    temperature
    max_tokens
    tools
    structured_output
    reasoning
    modalities
    stream
    routing
    provider_options
    metadata
```

followed by:

```text
GatewayRequest
      │
      ▼
Model Resolver
      │
      ▼
Routing Engine
      │
      ▼
Provider Adapter
      │
      ▼
Native / compatible provider request
```

That distinction will become extremely important.

If your internal representation is literally just `OpenAIChatCompletionRequest`, you're eventually going to encounter something Gemini, llama.cpp, Ollama, xAI, etc. can do that doesn't map cleanly onto it.

Instead:

```python
class ProviderAdapter(Protocol):
    async def chat(...)
    async def responses(...)
    async def embeddings(...)
    async def models(...)
    def capabilities(...)
```

and implementations like:

```text
OpenRouterAdapter
LlamaCppAdapter
OllamaAdapter
OllamaCloudAdapter
VLLMAdapter
GeminiAdapter
HuggingFaceAdapter
XAIAdapter
VeniceAdapter
FeatherlessAdapter
```

Several of those could actually inherit from one generic:

```text
OpenAICompatibleAdapter
```

with only config/capability overrides.

That dramatically reduces the amount of code.

---

# The really valuable part isn't normalization

Normalization is useful, but I think the **control plane you can build on top of it** is the thing that makes this worthwhile for you.

Imagine your application simply asks for:

```json
{
  "model": "coding-fast",
  "messages": [...]
}
```

and your config says:

```yaml
models:
  coding-fast:
    candidates:
      - provider: llama
        model: qwen3.5-9b
        priority: 1

      - provider: openrouter
        model: qwen/qwen3.5-35b
        priority: 2

      - provider: google
        model: gemini-3.8-flash
        priority: 3
```

Your gateway can then determine:

```text
Is local llama-server alive?
        │
       yes
        ↓
Is the requested context small enough?
        │
       yes
        ↓
Use local Qwen
```

or:

```text
local server unavailable
        ↓
OpenRouter
        ↓
provider unavailable / rate limited
        ↓
Gemini
```

Now every application you build gets this behavior automatically.

OpenRouter itself distinguishes these concepts as **model routing** and **provider routing**, which is a useful conceptual separation to preserve in your implementation. ([OpenRouter][2])

## Your model registry becomes especially useful

Rather than applications knowing:

```text
openrouter/google/gemini-...
ollama/qwen3.5:...
hf/...
http://localhost:8080
```

you could have:

```text
qwen-local
qwen-large
cheap-reasoning
fast-coding
strong-coding
long-context
image-prompt-enhancer
embedding-default
reranker-default
```

or still allow explicit addressing:

```text
llama/qwen3.5-9b
ollama/qwen3.5:9b
openrouter/anthropic/claude-...
google/gemini-...
hf/moonshotai/...
xai/grok-...
```

I would support **both aliases and explicit model IDs**.

Aliases enable policy.

Explicit IDs enable reproducibility.

---

# Metrics are where this could get really interesting for you

Since everything goes through one gateway, every inference call becomes an observation.

For each request you could record:

```text
request_id
timestamp

application
task
user-defined tags

requested_model
resolved_model
provider
provider_endpoint

input_tokens
cached_input_tokens
output_tokens
reasoning_tokens

TTFT
generation_time
total_latency
tokens/sec

HTTP status
retry count
fallback count
error type

estimated_cost
actual reported cost

context utilization

streaming?
tools?
structured output?
vision?

local:
    GPU memory
    slot
    queue duration
```

Then you're building your **own inference dataset**.

That could eventually answer things like:

> How often do I actually use Qwen vs Gemini?

> Which OpenRouter provider has been fastest for this model?

> Does local inference actually save enough money to justify using it?

> Which model do my coding applications use most?

> What percentage of requests fall back from local inference?

> What's my average TTFT from llama-server versus OpenRouter?

> What context lengths am I actually using?

> Which models have the highest error rate with tool calling?

That sort of information is otherwise scattered across several dashboards and local logs.

OpenRouter itself tracks latency, throughput and uptime when deciding among providers, so you'd essentially be bringing that same type of telemetry one level higher across **your entire inference infrastructure**. ([OpenRouter][3])

---

# Routing can become much more sophisticated later

I would **not** start with an AI model deciding where requests go.

Start deterministically.

For example:

```text
RouteRequest
    │
    ├── explicit provider? ───────────────► use it
    │
    ├── explicit model? ─────────────────► resolve providers
    │
    ├── capability requirements
    │       tools?
    │       vision?
    │       reasoning?
    │       JSON schema?
    │       context >= 100k?
    │
    ├── exclude unhealthy providers
    │
    ├── apply policy
    │       local-first
    │       cheapest
    │       fastest
    │       highest-quality
    │       balanced
    │
    ▼
selected deployment
```

Later you could introduce a learned router if you wanted.

For instance:

```text
"Write a regex"
     ↓
small local model

"Architect distributed indexing system"
     ↓
frontier cloud model

"Summarize this 80k document"
     ↓
long-context model

"Improve this Flux prompt"
     ↓
your fine-tuned local prompt model
```

That would be an interesting application for one of the small models you've been experimenting with.

But that's definitely a **later stage**.

---

# Local inference makes your case somewhat different from LiteLLM/OpenRouter

There's already a project extremely close to this: **LiteLLM Proxy**.

It provides a central LLM gateway, translates requests between provider APIs, supports 100+ LLM APIs, retries/fallbacks, cost tracking, auth, rate limits, and OpenAI-compatible endpoints. ([LiteLLM][4])

So there are really two reasonable approaches.

### Use LiteLLM

If the primary goal is:

> "I just want one endpoint for all of my models."

I would seriously consider running LiteLLM and configuring:

```text
OpenRouter
Ollama
Gemini
Hugging Face
xAI
...
```

behind it.

You'd get most of the boring provider compatibility work immediately.

### Build your own

For **you specifically**, I think building your own is more compelling than it would be for most developers because the gateway itself could become an important piece of your local-AI infrastructure.

You're interested in model evaluation, inference performance, llama.cpp, vLLM, quantization, local serving, embeddings/reranking, and agent systems.

A custom gateway could therefore evolve into considerably more than:

> OpenRouter running on localhost.

It could become:

```text
                    AI Control Plane

       ┌────────────────────────────────┐
       │        Inference Gateway        │
       └────────────────┬───────────────┘
                        │
          ┌─────────────┼──────────────┐
          ▼             ▼              ▼
     Cloud models   Local models   Specialized models
                        │
                 ┌──────┼──────┐
                 ▼      ▼      ▼
              llama   vLLM   Ollama

                        │
                        ▼
                 Telemetry Store
                        │
          ┌─────────────┼──────────────┐
          ▼             ▼              ▼
       Metrics       Evaluation      Routing
```

That's much more interesting.

---

## One particularly important design choice

Don't hide provider differences completely.

Do:

```json
{
  "model": "qwen-local",
  "temperature": 0.7,

  "provider_options": {
    "llama_cpp": {
      "min_p": 0.05,
      "repeat_penalty": 1.05
    }
  }
}
```

rather than forcing everything into some lowest-common-denominator schema.

vLLM explicitly supports provider-specific parameters alongside OpenAI-compatible requests, for example `top_k` and structured-output controls. llama.cpp likewise has server-specific features on top of its compatibility layer. ([vLLM][5])

Your abstraction should therefore work like:

```text
80–90% normalized
10–20% escape hatch
```

rather than:

```text
100% normalized
```

The latter tends to become frustrating.

---

# I would keep v1 surprisingly small

I wouldn't initially build a dashboard, dynamic AI routing, distributed workers, Kubernetes, multi-user auth, or elaborate billing.

I'd make v1 roughly:

- OpenAI-compatible Chat Completions + Responses API, a model/provider registry with aliases and capability metadata, adapters for `llama-server`, OpenRouter, Ollama, vLLM, Gemini and Hugging Face, streaming SSE passthrough, normalized errors/usage, retries and ordered fallback, SQLite or Postgres request telemetry, plus `/health`, `/providers`, `/models`, and `/metrics`.

That is very manageable.

After it has actually become the default endpoint your applications use, add:

```text
cost routing
latency routing
local-first routing
circuit breakers
provider health scoring
load balancing
rate limiting
prompt caching
semantic caching
tracing
evaluation hooks
dashboard
dynamic model selection
```

The architectural pattern is essentially:

```text
                      Public API
                          │
                    Request Parser
                          │
                   Canonical Request
                          │
                  Capability Resolver
                          │
                     Model Registry
                          │
                    Routing Engine
                          │
                  Deployment Selector
                          │
                    Adapter Registry
                          │
             ┌────────────┼────────────┐
             ▼            ▼            ▼
          OpenAI       Gemini       Ollama
       Compatible       Native       Native
          Adapter       Adapter      Adapter
             │            │            │
      ┌──────┼─────┐      │            │
      ▼      ▼     ▼      ▼            ▼
    llama   vLLM OpenRouter Gemini    Ollama
                          │
                     Observability
                          │
               ┌──────────┴──────────┐
               ▼                     ▼
            Database              Metrics
```

### My recommendation

**Yes, I'd build it.**

But I'd treat the project as a **personal inference gateway/control plane**, not merely a compatibility proxy.

The compatibility proxy is almost the easy part now. Most of your important endpoints already expose OpenAI-style interfaces, including local llama.cpp and vLLM. ([GitHub][1])

The part that makes the project valuable is having one place where you own **model identity, capability discovery, routing, local-vs-cloud policy, failover, telemetry, cost accounting, and eventually model-selection logic**.

And unlike many infrastructure projects, this is one you'd likely end up using in virtually every AI application you build.

[1]: https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md?utm_source=chatgpt.com "llama.cpp/tools/server/README.md at master · ggml-org/llama.cpp · GitHub"
[2]: https://openrouter.ai/blog/insights/model-routing/?utm_source=chatgpt.com "How OpenRouter Model Routing Works: Providers, Fallbacks & Auto Router — OpenRouter Blog"
[3]: https://openrouter.ai/providers/apply?utm_source=chatgpt.com "Become a Provider | OpenRouter"
[4]: https://docs.litellm.ai/?utm_source=chatgpt.com "LiteLLM - Getting Started | liteLLM"
[5]: https://docs.vllm.ai/en/latest/serving/online_serving/openai_compatible_server/?utm_source=chatgpt.com "OpenAI-Compatible Server - vLLM"

## Me

```
request_id
timestamp

application
task
user-defined tags

requested_model
resolved_model
provider
provider_endpoint

input_tokens
cached_input_tokens
output_tokens
reasoning_tokens

TTFT
generation_time
total_latency
tokens/sec

HTTP status
retry count
fallback count
error type

estimated_cost
actual reported cost

context utilization

streaming?
tools?
structured output?
vision?

local:
    GPU memory
    slot
    queue duration
```

Then you're building your **own inference dataset**.

That could eventually answer things like:

> How often do I actually use Qwen vs Gemini?

> Which OpenRouter provider has been fastest for this model?

> Does local inference actually save enough money to justify using it?

> Which model do my coding applications use most?

> What percentage of requests fall back from local inference?

> What's my average TTFT from llama-server versus OpenRouter?

> What context lengths am I actually using?

> Which models have the highest error rate with tool calling?

That sort of information is otherwise scattered across several dashboards and local logs.

OpenRouter itself tracks latency, throughput and uptime when deciding among providers, so you'd essentially be bringing that same type of telemetry one level higher across **your entire inference infrastructure**.

This is the most compelling part of this to me. This is exactly what I am hoping to do (and more potentially as far as data/metrics). I have been interested in using personal data/habits/requests/conversations as a fine-tuning / dataset tool eventually.

I would also always want all of this information as it's one of my main frustrations now is that sometimes I can't get these things, or it's set up in ways I'm not familiar with or whatever. globally being able to track all requests, all responses, all thinking output, all generation time, tokens/s, tokens generated, and so many more that I wish I had been saving all in db a year ago. (Would like to use postgresql for this so it's a bit more scalable and safely "permanent")

I would like to make sure, as we are going to start to do a more in depth planning, we do not skimp on the data/metrics/analysis/data presentation/visualization layers of this project in the slightest.

I am only interested in deterministic routing currently.

For this type of project, would Rust be the most efficient choice? For the gateway itself?

Yes. For **this specific project**, I would choose **Rust for the gateway/core**, especially now that you've clarified that the gateway is intended to become a permanent telemetry and inference-data collection layer rather than merely a convenience proxy.

The qualification is that Rust is probably the **most runtime-efficient** choice, but not automatically the fastest language to develop the system in. If development speed were the only priority, Go would be extremely competitive. Given what you want this project to become, though, I think Rust's tradeoffs are justified.

### Why Rust fits this gateway unusually well

The gateway's hot path is essentially:

```text
Client
  ↓
parse / normalize
  ↓
route deterministically
  ↓
outbound HTTP request
  ↓
stream provider response
  ↓
inspect / timestamp / meter chunks
  ↓
stream to client
  ↓
persist telemetry
```

That's almost an ideal async systems-programming workload. Tokio is designed for highly concurrent networking, Axum supports streaming/SSE natively, Reqwest exposes response bodies as byte streams, and SQLx provides async PostgreSQL access. ([Tokio][1])

More importantly, Rust lets the gateway do a **lot of instrumentation without the instrumentation becoming the bottleneck**.

You could have:

```text
provider stream
      │
      ▼
┌─────────────────────────────┐
│ Stream Instrumentation      │
│                             │
│ first-byte timestamp        │
│ first-token timestamp       │
│ chunk timestamps            │
│ byte counts                 │
│ parsed token counts         │
│ reasoning events            │
│ provider usage              │
│ errors                      │
│ finish reason               │
└──────────────┬──────────────┘
               │
         ┌─────┴─────┐
         ▼           ▼
      client      telemetry
                   channel
                       │
                       ▼
                   PostgreSQL
```

without buffering the entire generation before giving it back to your application.

That architectural property matters a **lot** for what you're describing.

---

## I would split the project by responsibility

I would not interpret "use Rust" as "everything must be Rust."

I'd probably build this:

| Component                   | Technology                 |
| --------------------------- | -------------------------- |
| Gateway/core                | **Rust**                   |
| HTTP framework              | Axum                       |
| Async runtime               | Tokio                      |
| Provider HTTP               | Reqwest/Hyper              |
| Serialization               | Serde                      |
| Middleware                  | Tower                      |
| DB access                   | SQLx                       |
| Primary datastore           | **PostgreSQL**             |
| Gateway instrumentation     | `tracing`                  |
| Standard telemetry export   | OpenTelemetry/OTLP         |
| Dashboard                   | Next.js/TypeScript         |
| Analytics API               | Rust initially             |
| Heavy data analysis         | Python as needed           |
| Fine-tuning/dataset tooling | Python                     |
| Offline SQL analytics       | PostgreSQL + Python        |
| Visualization               | Next.js + charting library |

The result would look more like:

```text
                       ┌──────────────────────┐
Apps / MCPs / CLIs ───►│   Rust LLM Gateway  │
                       └──────────┬───────────┘
                                  │
             ┌────────────────────┼─────────────────────┐
             │                    │                     │
             ▼                    ▼                     ▼
        llama-server         OpenRouter            Ollama/etc.
             │                    │                     │
             └────────────────────┼─────────────────────┘
                                  │
                           Instrumentation
                                  │
                     ┌────────────┴────────────┐
                     ▼                         ▼
                PostgreSQL                  OTLP
                     │
       ┌─────────────┼──────────────┐
       ▼             ▼              ▼
   Dashboard      Analysis      Dataset Builder
   Next.js         Python         Python
```

That gives Rust the job it is particularly good at without forcing it into places where Python is substantially more convenient.

---

# The database should be a first-class subsystem

Your clarification changes how I would design this compared with a normal AI gateway.

I would **not** bolt metrics on afterward.

I'd design the entire gateway around:

```text
Inference Request
       =
Routing operation
       +
Provider operation
       +
Telemetry event
```

Every inference becomes a durable record.

And I would distinguish between **normalized data** and **raw observations**.

For example:

```text
inference_requests
provider_attempts
generations
generation_usage
stream_events
tool_calls
routing_decisions
errors
model_snapshots
provider_snapshots
cost_records
request_tags
```

rather than one enormous:

```text
requests
```

table.

At the same time, I'd preserve:

```text
raw_request JSONB
raw_response JSONB
raw_provider_usage JSONB
raw_headers JSONB
```

where appropriate.

That means that if six months from now you realize:

> I wish I had extracted field X.

you potentially **still have it**.

That's particularly important given your reason for building this system in the first place.

---

## I would capture more than the metrics we discussed earlier

I'd treat the earlier fields as merely the beginning.

A generation should potentially allow reconstruction of a timeline like:

```text
14:03:21.034  gateway received request
14:03:21.036  request normalized
14:03:21.037  route selected
14:03:21.039  provider request started
14:03:21.112  provider connection established
14:03:21.487  first provider byte
14:03:21.503  first generation chunk
14:03:21.503  chunk 1
14:03:21.537  chunk 2
14:03:21.558  chunk 3
...
14:03:24.892  final chunk
14:03:24.895  provider stream closed
14:03:24.898  usage received
14:03:24.902  client stream closed
```

Now you can derive:

```text
routing_latency
connection_latency
TTFB
TTFT
prefill_estimate
generation_duration
inter_chunk_latency
stream duration
output throughput
end-to-end throughput
provider overhead
gateway overhead
```

That is **much more powerful than merely storing `total_latency = 3.86s`**.

And Rust is very attractive here because this is exactly the kind of fine-grained stream instrumentation it handles cleanly.

OpenTelemetry itself supports timestamps down to nanosecond precision at the specification level. ([OpenTelemetry][2])

---

# One metric I'd absolutely preserve: provider attempts

Don't just store:

```text
provider = openrouter
```

Store the whole attempt hierarchy.

For example:

```text
request
│
├── routing_decision
│
│   requested: coding-fast
│   selected: qwen3.5-9b
│
├── attempt #1
│   provider: llama-server
│   status: failed
│   error: connection_refused
│   duration: 42ms
│
└── attempt #2
    provider: openrouter
    model: qwen/...
    upstream_provider: DeepInfra
    status: success
    TTFT: 382ms
    tokens/sec: 71.4
```

Then:

```text
fallback_count = 1
```

is just a derived metric.

That's a recurring principle I'd use throughout this system:

> **Store observations; calculate summaries.**

Don't permanently reduce:

```text
[37ms, 31ms, 42ms, 29ms, 116ms, ...]
```

into merely:

```text
avg_chunk_latency = 43.2ms
```

You can always calculate the latter.

You cannot reconstruct the former.

That philosophy is extremely important if you want this database to eventually become a dataset rather than just an operations dashboard.

---

# This also changes how I'd handle request/response content

Given your eventual fine-tuning/data-mining goal, I'd preserve several representations:

```text
RAW
provider-native request
provider-native response

NORMALIZED
canonical messages
canonical generation
canonical tool calls
canonical reasoning data

DERIVED
token counts
task classification
language
code/content ratios
embedding
quality signals
latency metrics
cost metrics

CURATED
accepted training pair
rejected training pair
preference pair
evaluation result
annotation
```

That creates a natural progression:

```text
Raw inference history
        ↓
Normalized corpus
        ↓
Analysis / filtering
        ↓
Dataset candidates
        ↓
Curated datasets
        ↓
SFT / DPO / eval sets
```

That could eventually become one of the most useful parts of the system for you.

---

## One important caveat about "all thinking output"

We should distinguish:

```text
reasoning content exposed by provider
reasoning summaries exposed by provider
reasoning token counts
hidden/internal chain-of-thought
```

You can persist the first three when they're exposed.

You **cannot generally capture a provider's hidden internal chain-of-thought** if the provider doesn't return it.

So I would model it explicitly instead of assuming reasoning is always text:

```text
reasoning:
    exposed_content: ...
    summary: ...
    token_count: ...
    encrypted_content: ...
    provider_metadata: ...
    visibility: exposed | summary_only | count_only | unavailable
```

That prevents the database schema from treating "no reasoning text returned" as "the model didn't reason."

---

# Rust vs Go vs TypeScript vs Python

For this project specifically:

|                                  |          Rust |            Go | TypeScript |        Python |
| -------------------------------- | ------------: | ------------: | ---------: | ------------: |
| Gateway throughput               |     Excellent |     Excellent |       Good |          Good |
| Memory efficiency                |     Excellent |     Very good |   Moderate |      Moderate |
| Streaming proxy                  |     Excellent |     Excellent |       Good |          Good |
| Fine-grained instrumentation     | **Excellent** |     Excellent |       Good |          Good |
| Async concurrency                |     Excellent | **Excellent** |  Very good |          Good |
| Type safety for provider schemas | **Excellent** |     Very good |  Very good |      Moderate |
| Development speed                |      Moderate | **Excellent** |  Excellent |     Excellent |
| ML/data ecosystem                |          Weak |          Weak |   Moderate | **Excellent** |
| Long-running daemon              | **Excellent** |     Excellent |       Good |          Good |
| Single binary deployment         | **Excellent** |     Excellent |         No |            No |
| Learning curve                   |       Highest |           Low |        Low |           Low |

There is one interesting current disadvantage: Rust's official OpenTelemetry implementation still labels traces, metrics, and logs **Beta**, whereas Go's traces and metrics are Stable and logs are currently Release Candidate. ([OpenTelemetry][3])

I don't consider that a dealbreaker. I'd use Rust's `tracing` ecosystem internally and make OTLP an **export mechanism**, not the canonical source of your inference telemetry.

Your PostgreSQL records should be authoritative.

---

# More importantly, don't put the database directly in the stream's critical path

Something like:

```rust
for chunk in provider_stream {
    sqlx::query!("INSERT ...").execute().await;
    client.send(chunk).await;
}
```

would defeat a lot of the purpose.

Instead:

```text
                        ┌───── client
Provider chunk ─────────┤
                        │
                        └────► telemetry event
                                  │
                            bounded channel
                                  │
                                  ▼
                           telemetry writer
                                  │
                           batch / transaction
                                  │
                                  ▼
                              Postgres
```

And because you specifically care about the data being **permanent**, I'd go one step beyond an ordinary fire-and-forget metrics queue.

Eventually I'd give the telemetry subsystem a small durable ingestion/outbox mechanism:

```text
Inference stream
      ↓
telemetry event
      ↓
durable local journal
      ↓
Postgres writer
      ↓
acknowledged / checkpointed
```

That way:

```text
Postgres temporarily unavailable
```

doesn't mean:

```text
those generations never existed
```

You don't necessarily need that in the very first implementation, but I would design the interfaces with it in mind.

---

# Rust also gives us a nice provider abstraction

Something approximately like:

```text
Provider
│
├── capabilities()
├── models()
├── health()
│
└── execute()
      │
      ├── request metadata
      └── Stream<ProviderEvent>
```

Where the stream might emit:

```text
ProviderEvent

Started
Connected
ResponseHeaders
ReasoningDelta
ContentDelta
ToolCallDelta
UsageUpdate
Completed
Error
```

This is much better for your project than having:

```rust
async fn generate() -> String
```

because telemetry becomes inherent to the execution model.

Your normalizer can then produce:

```text
ProviderEvent
      ↓
CanonicalGenerationEvent
      ├──────────────► Client serializer
      └──────────────► Telemetry recorder
```

That is one of the architectural areas I would spend considerable design effort on before implementation.

---

# And deterministic routing fits Rust particularly nicely

Since you're explicitly **not interested in AI routing**, I wouldn't introduce any classifier/model into the critical path.

Instead:

```text
Request
   ↓
constraints
   ↓
candidate deployments
   ↓
filter unavailable
   ↓
filter incompatible
   ↓
policy
   ↓
ordered candidates
```

For example:

```yaml
routes:
  local-first:
    strategy: ordered

    candidates:
      - llama/qwen3.5-9b
      - ollama/qwen3.5:9b
      - openrouter/qwen/qwen3.5-35b

    retry:
      max_attempts_per_provider: 2

    fallback_on:
      - timeout
      - rate_limit
      - unavailable
      - server_error
```

Every decision itself becomes data:

```text
routing_decision:
    candidates_considered: 3
    candidates_rejected: 1
    rejection_reason:
        missing_required_capability
    primary_selected:
        llama/qwen3.5-9b
```

Again, don't just store the final destination.

Store **why it became the destination**.

---

## So: would I choose Rust?

**Yes — for the gateway/core, I would.**

Not because Python or TypeScript couldn't handle your request volume. Your personal usage is extremely unlikely to overwhelm any of them.

I'd choose Rust because the gateway you're describing is fundamentally becoming a **long-lived piece of infrastructure** that:

```text
proxies streams
measures streams
transforms protocols
maintains connections
routes traffic
tracks precise timings
handles concurrency
buffers events
records telemetry
survives provider failures
and should ideally add almost no observable overhead
```

That's exactly where Rust becomes more compelling.

And it gives you room for this gateway to eventually handle not just:

```text
chat completions
```

but:

```text
chat
responses
embeddings
reranking
image generation
audio
TTS
ASR
vision
possibly video
```

without needing to rethink the core architecture.

I would therefore make **Rust + PostgreSQL** the foundation, **Next.js/TypeScript** the data exploration/dashboard layer, and **Python** the eventual analysis/dataset/fine-tuning companion. That combination fits both the infrastructure side and the much more important long-term goal you just described: turning years of inference activity into a queryable, analyzable, reusable corpus rather than letting it disappear into provider logs.

And when we move into the actual architecture planning, I'd make **telemetry/data provenance one of the top-level domains alongside routing and providers—not a later "observability" module**.

[1]: https://tokio.rs/tokio/tutorial?utm_source=chatgpt.com "Tutorial | Tokio - An asynchronous Rust runtime"
[2]: https://opentelemetry.io/docs/specs/otel/trace/api/?utm_source=chatgpt.com "Tracing API | OpenTelemetry"
[3]: https://opentelemetry.io/docs/languages/rust/?utm_source=chatgpt.com "Rust | OpenTelemetry"

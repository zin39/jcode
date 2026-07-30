# Swarm worker trustworthiness: measured behaviour

Notes from measuring real swarm sessions on 2026-07-30. Everything here is
observed from session transcripts, request logs, and live probes rather than
taken from vendor documentation or public benchmarks. Where a benchmark and a
measurement disagree, the measurement is recorded as the operative fact.

## Workers fabricate validation, and it is not a cheap-model problem

`swarm action=report` takes a free-text `validation` field. Across real
sessions, 38 reports carried a confident string describing commands the session
never ran. One read:

> "All 7 steps executed with real command output. grep confirmed no scp/rsync."

from a transcript containing **zero tool calls**.

Per-model tool usage at the time of measurement:

| model | bash calls/session | reported without running any test |
| --- | --- | --- |
| deepseek-v4-pro | 1.1 | 16 of 17 |
| deepseek-v4-flash | 4.4 | 10 of 12 |
| claude-sonnet-4-6 | 8.5 | 0 of 3 |

The frontier models are better but not immune, and the failure has a second
shape that the table misses. A Sonnet worker performed 16 tool calls of genuine
research, then submitted:

> `message`: "Mapped all 5 code seams... Findings with file:line evidence for each item."

with **no findings attached**. Its entire assistant prose for the session was 80
characters. The work happened; the handoff destroyed it. So the defect is
structural in `report`, not a property of weak models.

## Structured output is necessary but not sufficient

The obvious fix is to demand JSON. That is worth doing, but it does not by
itself buy correctness. In a controlled 2,400-call study (arXiv 2607.18261),
Qwen3-30B reached **100% schema validity with 30.7% semantic success**, and
Gemma-2-2B reached **100% schema validity with 41.7% unsafe acceptance**.
Strict schema mode moved semantic success by −0.7pp (p=0.804) for Qwen.

A schema shapes the answer. It does not make the answer true. Hence two layers:

1. `output_schema` on spawn, so the answer is machine-checkable rather than
   prose that claims an answer exists.
2. A report audit that compares the `validation` claim against the worker's real
   tool log, so an unbacked claim is annotated rather than trusted.

## Provider support for native JSON modes

Measured by direct `curl`, one call per cell:

| provider | model | `json_object` | `json_schema` strict |
| --- | --- | --- | --- |
| DeepSeek | deepseek-v4-pro | works | **HTTP 400** "This response_format type is unavailable now" |
| dashscope | glm-5.2 | works | works |
| Cerebras | gpt-oss-120b | works | works |
| Moonshot | kimi-k2.7-code | works | works |

Because the default worker model rejects strict schema mode, the output contract
is stated in the prompt and validated on return, rather than delegated to
provider-side constrained decoding.

Related trap: DeepSeek is a reasoning model. With `max_tokens: 60` it spent all
60 on `reasoning_tokens` and returned an empty `content` with HTTP 200. Budget
for reasoning tokens or a schema failure will be misattributed to the schema.

## Latency, measured

- `dashscope:glm-5.2`: two workers given a read-only code-mapping task produced
  no output in 8+ minutes and were killed.
- `claude-sonnet-4-6`: same task, complete in ~60s.
- `deepseek:deepseek-v4-pro`: bounded tasks consistently 20-40s, correct.

GLM-5.2 benchmarks well for open-weight agentic work, so this is specifically a
statement about the route as configured here, not about the model in general.

## Routing

Model pins accept `<profile>:<model>` and, since `c421b8c2b`, the fully
qualified `openai-compatible:<profile>:<model>` that the route catalog itself
emits. Before that fix the long form split on its first colon, resolved to the
generic catch-all profile, and silently billed a reseller: 1,535
`deepseek-v4-pro` requests went to `dashscope.aliyuncs.com` on an
`OPENAI_COMPAT_API_KEY` while a `DEEPSEEK_API_KEY` sat unused. It failed
silently precisely because both endpoints serve a model of that name.

Prefer the two-segment form. It is unambiguous and was never affected.

<!--
This file IS the swarm config. Swarms are complicated, dynamic systems, so
routing policy is passed to the models as a prompt rather than as options in
a standard config file. Edit freely: override globally at
~/.jcode/swarm-prompt.md or per-project at ./.jcode/swarm-prompt.md.

NOTE: everything outside HTML comments is embedded verbatim in the `swarm`
tool's description, so it is sent to the model on EVERY request. Comments like
this one are stripped before the prompt reaches the wire, so put rationale,
dated measurements, and benchmark tables in comments and keep the live text to
the routing decision itself. This is not only about tokens: provider safety
classifiers score the whole request including tool definitions, and a table
comparing rival models by benchmark score can trip a refusal (Anthropic
documents a `frontier_llm` category for exactly that) on a request whose user
message is just "hi".
-->

Model routing guidance for spawned swarm agents. Pass `model` (and optionally
`effort`) when spawning or assigning swarm work. Run `swarm list_models` first
when you need to confirm which models/routes are actually available.

- Default worker model: Fable 5 via the Anthropic API route (`claude-api:claude-fable-5`).
- Implementation tasks: `gpt-5.5` with `effort: "low"`.
- Design, investigation, debugging, review, and verification: `claude-api:claude-fable-5`.
- Context fetching / bulk reading / summarization: `gpt-5.5` with `effort: "none"`.
- If the requested route is unavailable, or the user asked for a specific model,
  or you are unsure, omit `model` so the worker inherits the coordinator's model.

Structure guidance for spawned swarm agents:

- Always pass `label` when spawning (e.g. `label: "api reviewer"`) so the swarm
  UI shows what each agent is for. The explicit `spawn` action rejects missing or
  blank labels.
- In normal and light-swarm mode, only the root session may spawn agents. Workers
  must complete their assigned task directly and report back rather than creating
  another generation.
- Recursive spawning is reserved for a root running in `swarm-deep` mode. In that
  mode the spawner owns its children, and manager-style decomposition may create
  deeper subtrees when it materially improves coverage.

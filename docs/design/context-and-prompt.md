# Context and prompt

> Status: target design contract.
> Last updated: 2026-09-02.

Context and prompt owns context assembly, prompt-cache stability, dynamic state blocks, compaction, and memory injection boundaries. It does not own provider routing or tool execution.

This document defines the target behavior that implementation should converge toward.

The database-inspired foundation for this design is described in
[ContextPipe: Database-Inspired Context Assembly for Long-Horizon
Agents](https://arxiv.org/abs/2609.00749), accepted at
[ADS 2026](https://vldb-ads.top/#program), co-located with VLDB 2026.

## Principle

```text
Prompt cache stability comes from stable structure, not from hiding runtime truth.
```

## Context layers

| Layer | Purpose |
| --- | --- |
| System contract | Stable rules, tool protocol, provider contract, safety boundaries. |
| Session state | Current session/run/turn/task summary. |
| Provider state | Compact capability/provider decisions and availability. |
| Transcript | User-visible conversation facts. |
| Memory | Retrieved cross-session or long-term facts. |
| Artifacts | Explicitly referenced files, outputs, captures, and manifests. |
| Reflection | Agent self-assessment and strategy when allowed. |

## Stable prefix

The stable prefix should include:

- agent contract;
- tool protocol;
- provider decision schema;
- safety policy summary;
- trace/event schema;
- response formatting rules.

It should not include volatile provider online/offline status, large task lists, or transient sync counters.

## Dynamic blocks

For OpenAI-compatible requests, the provider projection has at most one system
message, at the beginning. Stable agent/platform rules and typed runtime
instructions are consolidated there. Runtime facts and advisory evidence are
separate user-role messages marked `astra-runtime-context`; this wire role does
not turn them into canonical human requests. Genuine user content and real
assistant/tool groups retain their identity and order.

Delivery and authority are independent. Required context is not automatically a
system instruction. The producer-owned injection kind selects policy; the
active-turn frame's fixed instruction is separated from its user/goal/round
facts before rendering. User text and wrapper-like strings never grant authority.
Platform integrations must put invariant rules in `stable_runtime_system_prompt`
and per-turn data in `runtime_system_prompt`. Switching a runtime policy can
change the system prefix; changing round facts must not.

The explicitly selected append-only layout keeps its existing runtime-owned
user frames, lifetimes, and durable history protocol. It is already a single-
system wire shape and is not flattened into ordinary human messages. On other
OpenAI-compatible layouts, typed policies join the leading system, while facts
use the marked user-context projection. The invariant focus policy applies to
all layouts; exact turn text stays outside the system prefix.

Dynamic state belongs in compact blocks with stable keys:

```text
run_state
provider_state
task_projection
sync_state
memory_recall
artifact_manifest
```

Values may change; keys and structure should remain stable to preserve prompt-cache utility.

## Compaction

Compaction should preserve:

- active user intent;
- unresolved constraints;
- provider decisions relevant to pending actions;
- task state;
- recent failures and degraded states;
- audit-critical facts;
- links to recoverable artifacts.

Compaction should not turn transient tool output into permanent truth without attribution.

## Memory injection

Memory is injected as evidence with provenance and confidence. It should not override current session facts without an explicit conflict signal.

Memory loading belongs to this domain. Memory storage and lifecycle belong to [memory.md](memory.md).

## ForkPrefix

ForkPrefix is a prompt-cache and diagnostic optimization. It is not a recovery correctness mechanism.

Restore correctness depends on:

- checkpoint;
- transcript;
- C2 audit facts;
- C3 trace facts;
- artifact manifest.

## Provider state in prompt

Provider state should be summarized from capability decisions:

- available providers;
- blocked/offline/degraded capabilities;
- fallback selected;
- user action required.

The prompt should not re-describe all tools every time a provider state changes.

## Test obligations

- Equivalent stable inputs produce byte-stable stable prefix.
- Provider offline changes compact provider state, not the whole prompt contract.
- Compaction preserves active tasks and pending blocked reasons.
- Memory conflict is represented explicitly.

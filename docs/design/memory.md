# Memory

> Status: target design contract.
> Last updated: 2026-07-07.

Memory owns durable knowledge across and within sessions. Context injection uses memory, but memory lifecycle, provenance, confidence, and deletion belong here.

## Memory classes

| Class | Meaning |
| --- | --- |
| Working memory | Current turn/session scratch state. |
| Episodic memory | Past session events and outcomes. |
| Semantic memory | Durable facts and concepts. |
| Procedural memory | Reusable procedures, preferences, and skills. |
| Project memory | Workspace/repository-specific facts. |

## Requirements

- Every memory item has provenance.
- Confidence and freshness are explicit.
- Conflicts are represented, not silently overwritten.
- User deletion propagates to derived memory.
- Memory used in a response is traceable.
- Memory injection is bounded by context budget and task relevance.

## Loading policy

Memory loading is intent-driven:

- current task determines candidate memories;
- recent session facts outrank old memory on conflict;
- procedural memory should be loaded at point of use;
- low-confidence memory should be marked as uncertain;
- sensitive memory follows permission and redaction policy.

## Backend boundary

The design does not require one physical backend. Vector, fulltext, graph, tabular, or MCP-backed memory can coexist as long as they satisfy the same provenance, confidence, deletion, and trace contract.

## Scoped credential admission

The Server uses the application-scoped credential resolver owned by authentication. Each operation resolves the current owner binding and generation; no master-key fallback is inferred by a generic pool builder.

- Missing binding or `none`: normal disabled state. Prompt recall reports `NotAttempted` and does not contact Memoria.
- `read_only`: recall is allowed; write-oriented extraction, reflection and session-end cleanup are not admitted.
- `read_write`: read and write operations are allowed. Transport checks remain in place to catch revocation or changes after admission.

The background coordinator may launch a lightweight admission task, but it checks consent before loading snapshots, resolving an LLM, generating memory, or scheduling persistence. See [authentication](authentication.md) for issuer, credential replacement and retention.

## Learning boundary

Memory is not training data by default. Learning artifacts require consent, redaction, quality gate, lineage, and deletion propagation.

# Testing Guide

This repository is validated through Rust-first checks, contract tests (fast, many with stub services), and optional **live MatrixOne** system E2E.

## Primary Commands

```bash
# Full suite: workspace + e2e-hooks + online #[ignore] Matrix E2E + multi-agent integration (requires MatrixOne and Memoria for the online portion)
make test

# Workspace + server E2E hooks only (no online #[ignore] suites)
make test-offline

# Online #[ignore] suites only (exports ASTRA_TEST_DB_IT=1; set ASTRA_TEST_DB_IT_TEST_THREADS=1 for serial execution)
make test-online

# Narrow contract smoke (HTTP + admin integration binaries; settings JSON via astra-core lib tests)
make test-contract

# Static validation
make check
make format-check
make lint
make type-check
```

Direct `cargo` usage:

```bash
cargo test --manifest-path Cargo.toml -q
cargo check --manifest-path Cargo.toml
```

## Where Tests Live

- `crates/runtime/tests/` — HTTP integration tests for `astra-runtime` (including `*_contract.rs`, `system_matrix_http_e2e/`, bridge E2E).
- `crates/services/tests/` — service-layer tests (e.g. `multi_agent_integration` with live DB when `ASTRA_TEST_DB_IT=1`).
- `fixtures/contracts/` — JSON fixtures for contract tests that load shared request/response shapes.
- Capability ↔ route ↔ E2E mapping: [`docs/testing/system-e2e-matrix.md`](../testing/system-e2e-matrix.md).
- SaaS capability test plan: [`docs/testing/saas-test-plan.md`](../testing/saas-test-plan.md) (`make test-saas`; Rust HTTP E2E plus optional remote `@astra/sdk` coverage).
- Coverage matrix (what replaced stub tests, large-binary audit): [`docs/testing/coverage-matrix.md`](../testing/coverage-matrix.md).

## Live MatrixOne system E2E

Memoria identity/credential fixtures require `ASTRA_TEST_DB_IT=1` and an
explicit `ASTRA_TEST_DATABASE` matching the effective database name. The normal
online runner supplies its isolated lane database; local callers must designate
their disposable database rather than relying on a developer-specific prefix.

Contracts requiring an external scoped-key Memoria API or a real provider are
separately gated by the services `external-contract-tests` feature. They are not
selected by ordinary MatrixOne online lanes. To run the scoped Memoria contract,
provision a test Memoria API with scoped keys, set `ASTRA_TEST_MEMORIA_URL`,
`ASTRA_TEST_MEMORIA_MASTER_KEY`, `ASTRA_TEST_DATABASE` and the test MatrixOne
connection variables, then run `make test-memoria-auth-online-contract`.
Missing configuration fails the explicitly selected contract; it is not reported
as a passing test. No production credentials are needed in fork CI.

The key-free BYOK network smoke is also explicit:

```bash
ASTRA_BYOK_DNS_SERVERS=tcp://223.5.5.5 \
ASTRA_TEST_BYOK_MODELS_URL=https://api.moonshot.cn/v1/models \
CARGO_INCREMENTAL=0 cargo test --locked -p astra-services \
  --features external-contract-tests --test byok_live_network -- --ignored
```

It expects HTTP 401 with no provider key and proves DNS/TLS/HTTP reachability,
not successful model inference.

The provider-wire regression uses a strict loopback HTTP fixture and a disposable
MatrixOne database, without real provider keys. Create the designated database
first and supply its `MATRIXONE_*` connection settings. Choose an unused loopback
port for the fixture:

```bash
ASTRA_TEST_DB_IT=1 ASTRA_DATABASE_PREFIX= \
ASTRA_DATABASE=astra_probe_test ASTRA_TEST_DATABASE=astra_probe_test \
ASTRA_ALLOW_INSECURE_DEFAULTS=1 \
ASTRA_BYOK_DEEPSEEK_BASE_URL=http://127.0.0.1:18994 \
CARGO_INCREMENTAL=0 cargo test --locked -p astra-services \
  --features external-contract-tests --test user_model_probe_db_it -- --ignored
```

This covers create, credential rotation, explicit probe and failed-write
preservation. Official OpenAI/Anthropic probe and rotation tests seed only their
fixture rows with loopback endpoints; production official endpoints remain fixed.
`memoria_reauthentication_http` separately covers same-key reconnect, pending
proof invalidation and an in-flight verification crossing disconnect/reconnect.

```bash
ASTRA_TEST_DB_IT=1 \
ASTRA_TEST_E2E_SECRET=system-matrix-e2e-secret \
ASTRA_BACKEND_SERVICE_KEY=test-service-key-e2e \
ASTRA_LLM_RETRY_BASE_MS=10 ASTRA_DEFAULT_RETRY_AFTER_MS=10 ASTRA_BCRYPT_COST=4 \
RUST_MIN_STACK=16777216 \
cargo test -p astra-runtime --test system_matrix_http_e2e --features e2e-hooks -- \
  --ignored --nocapture
```

Requires the same environment as `astra-server`: `MATRIXONE_*`,
`ASTRA_JWT_SECRET`, `ASTRA_TOKEN_ENCRYPTION_KEY`, Memoria, and embedding
settings parsed by `astra_core::AppSettings::from_env`. Use a local `.env` if
you use one for development. To isolate from production on one MatrixOne host,
set **`ASTRA_DATABASE_PREFIX`** (effective DB = prefix + `ASTRA_DATABASE`).
Optionally set **`ASTRA_AUTO_CREATE_DATABASE=1`** so the first
`ensure_core_schema` (server or online tests) runs `CREATE DATABASE IF NOT
EXISTS` for that effective name (bootstrap catalog defaults to `mysql`).

## Recommended Workflow

```bash
# 1. Smallest relevant target while iterating
cargo test --manifest-path Cargo.toml -p astra-runtime --test http_contract

# 2. Core HTTP contract smoke
make test-contract

# 3. Full workspace + server E2E hooks (no online #[ignore] suites)
make test-offline

# 4. With MatrixOne + Memoria up: add online #[ignore] suites (same as the second half of `make test`)
make test-online
```

## What "done" Means

A change is not complete until:

- formatting passes
- compile/type checks pass
- clippy passes
- the relevant Rust tests pass (including PR Matrix E2E when touching server/persistence paths)

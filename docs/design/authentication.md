# Authentication

Authentication owns identity, session issuance and credential binding. Request-authorized external providers keep their existing authorization protocol; verified scoped-key providers reuse the canonical identity mapping and Astra refresh-session lifecycle.

## Identity and provenance

External principals are identified by issuer/provider and subject, never subject or email alone. Memoria's provider ID is `memoria:` followed by the SHA-256 of its normalized issuer URL. The issuer defaults to the configured API base URL; an explicitly stable `MEMORIA_ISSUER` lets administrators move the transport without changing the identity authority.

`auth_external_identities` has primary key `(provider_id, external_subject)`. The mapping resolves an Astra account; it does not replace its roles or lifecycle. JWT origin and the runtime principal retain the verified provider identity. Provider-request authorization is a different principal variant and is not granted by scoped-key login.

## One composition boundary

The auth service captures validated Memoria settings during application composition and supplies one credential resolver to login, refresh, proxy routes and memory runtime consumers. A pool setter never selects a memory transport. Explicit fixture/admin transport overrides cannot redirect the user-scoped credential.

The provider verifies the scoped-key API contract through `/auth/whoami`: active non-master personal key, exact owner, nonempty key ID, API version 1, scopes capability and memory-filter capability. HTTP redirects are not followed. Identity, model-provider and Memoria secrets must not be logged.

## Atomic binding and sessions

Login verifies the key, takes the canonical account lock, rechecks the key after waiting, and commits identity, encrypted binding and refresh session together. Concurrent first logins resolve one account. A deterministic credential token primary key identifies one binding per provider/account; replacements remove superseded ciphertext in the same transaction. The verified key ID identifies the upstream key generation. Astra also persists a local connection lifecycle nonce in credential metadata: ordinary login with the same active binding preserves it; key replacement or reconnect after disconnect generates a new nonce, even when the upstream key ID is unchanged. Failed issuance rolls back every login-owned row.

Refresh validates the current Memoria credential online. Compare-and-revoke of the old refresh token prevents a concurrent disconnect from resurrecting its session. Access TTL is bounded to 15 minutes. Runtime consumers independently resolve current consent and deny inactive/missing accounts.

## Disconnect and retention

- Logout revokes one Astra session. It does not revoke other devices or turn off memory sharing.
- Disconnect removes Astra's stored Memoria credential and revokes the account's Astra refresh sessions atomically. It retains identity mapping, models and Work so explicit relinking recovers the same account.
- Disabling memory sharing is different: an identity-only key remains usable for sign-in.
- Memoria owns upstream key revocation and deletion of its accounts/memories. Astra disconnect must not claim to perform those actions.
- Deactivated/deleted Astra accounts cannot resolve their scoped credential. Bounded maintenance removes their stored ciphertext. Existing external mappings do not automatically recreate a deleted local account.
- Account-wide Work/history erasure remains governed by the account retention contract; sign-out, disconnect and temporary verification outages never erase it.

## Legacy migration

The old `auth_memoria_identities` table is a read-only migration source. Its missing issuer cannot safely be guessed. Administrators must set `MEMORIA_LEGACY_ISSUER` to the known original issuer, matching the configured issuer. A fresh verified login then moves that mapping to the canonical table, preserves the Astra user ID and its models/Work, replaces the credential, and revokes old sessions in one transaction. Without that assertion, login fails with 409 and legacy refresh sessions fail closed. Do not set this option when pointing an old Astra database at a different Memoria instance.

## Fresh reauthentication

Normal sign-in is separate from authorizing device trust, device re-enrollment,
or forced session takeover. `GET /auth/reauthenticate` is authenticated discovery:
local accounts receive `method: password`; Memoria accounts receive
`method: memoria` and a verification page under the configured `MEMORIA_WEB_URL`.
This URL is a trusted identity-verification endpoint, not a caller-selected URL.
It requires HTTPS except for explicit loopback development.

The website requires a fresh email code sent to the current account's email.
This also works for GitHub/Google-created accounts with an accessible email;
an existing OAuth login session by itself is not fresh verification. Private
API-key login deployments keep their existing local-password reauthentication.
The email names the sensitive action. Codes are purpose/account/key-generation
bound, expire in five minutes, allow five attempts, and have a 60-second resend
cooldown. Code hashes use an application-keyed HMAC. Normal login codes cannot
be used here. Verification neither rotates the connection key nor changes
memory consent.

Successful verification returns an opaque `msu_` proof, valid for two minutes.
The user copies it back to the originating action. It stays out of URLs,
browser storage, and logs. The SDK accepts
`reauthenticate({ memoriaProof }, purpose)`; the HTTP request is
`POST /auth/reauthenticate` with `{ "memoria_proof": "msu_…", "purpose": "device_trust" }`.
The existing password request remains supported for local accounts. Mixing
methods is rejected.

Astra validates the active issuer/subject/connection generation online, then
consumes the website proof over the fixed HTTPS backchannel
`/api/auth/astra/reauthentication/consume`, with redirects disabled and a bounded
response. It checks the returned subject, key generation, purpose and timestamps
and revalidates its binding before issuing the existing five-minute `rp_` proof.
That proof is single-use, purpose-bound and additionally bound to the Memoria
identity, upstream key generation and Astra connection lifecycle. Disconnect
deletes pending proofs in the same account-locked transaction as the binding.
The lifecycle binding also prevents an in-flight proof issuance from becoming
usable after reconnect with the same upstream key. Legacy credential metadata
without a lifecycle nonce remains readable; the next login assigns one. Proofs
issued before this binding-format upgrade must be obtained again (their maximum
lifetime is five minutes). Device trust still requires the separate
device-possession challenge. Both proof exchanges fail closed on upstream
revocation, disconnect, identity mismatch or replay; failed exchanges require
fresh evidence rather than bypassing proof checks.

The website must be deployed with the reauthentication API before Astra clients
can use this path. An unavailable verifier blocks only the sensitive operation,
not ordinary sign-in or chat. Astra does not need a Memoria master key or a new
shared signing secret.

## Client/deployment contract

`GET /auth/methods` advertises the Server's website and issuer. An unset website retains interactive password login, including all-in-one deployments. The CLI falls back to the older password journey only on discovery 404, not on outages or malformed configuration. Explicit username/password and manual scoped-key login remain available.

Browser login URLs require HTTPS except for explicit loopback development addresses. The callback remains bound to 127.0.0.1 with exact Origin, nonce, method, content-type and bounded request validation. Windows passes the URL as child-process environment data, not shell source.

## Verification

Focused coverage lives in `memoria_auth_db_it`, `memoria_auth_http`, CLI auth-flow tests and runtime consent-admission tests. It includes issuer separation, concurrent login/relink, post-write failure rollback, legacy migration, disconnect, inactive-account retention, source-configuration mismatch, read-only extraction and login discovery. Actual Windows browser launch and live OAuth callbacks require platform/deployment testing in addition to deterministic contracts.

`memoria_live_contract_it` additionally runs against a real Memoria API implementing scoped-key API version 1. It creates disposable identity-only, read-only and read-write keys, verifies stable Astra account mapping, and revokes each key through Memoria before checking refresh rejection. Run only against an isolated Memoria test service and an explicitly designated test database:

```bash
# Set the isolated MatrixOne connection variables as in the DB testing guide.
export ASTRA_TEST_DB_IT=1
export ASTRA_TEST_DATABASE=review_memoria_contract
export ASTRA_TEST_MEMORIA_URL=http://127.0.0.1:18104
# Set ASTRA_TEST_MEMORIA_MASTER_KEY to the isolated Memoria service's test key.
make test-memoria-auth-online-contract
```

The test issues and revokes upstream keys; never point it at a production service.

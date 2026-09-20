//! Live MatrixOne coverage for cancellation-safe run persistence.
//!
//! Run with:
//! ASTRA_TEST_DB_IT=1 cargo test -p astra-services \
//!   --test run_persistence_cancellation_db_it -- --ignored --test-threads=1

mod common;

use astra_services::runs::{
    AtomicRunInteractionBatchRegistration, AtomicRunInteractionBatchRegistrationRequest,
    AtomicRunInteractionWaitRequest, DatabaseRunStateStore, DurableRunInteractionKind,
    DurableRunInteractionResolveOutcome, DurableRunInteractionWaitOutcome, DurableRunStartClaim,
    RunStateStore,
};
use serial_test::serial;
use uuid::Uuid;

const TEST_OWNER_POD_ID: &str = "run-persistence-cancel-owner";

async fn seed_run(pool: &sqlx::Pool<sqlx::MySql>, user_id: &str, session_id: &str, run_id: &str) {
    sqlx::query(
        "INSERT INTO agent_sessions
         (session_id, user_id, status, event_count, project_retention_policy,
          created_at, updated_at, last_active_at)
         VALUES (?, ?, 'active', 0, 'session', NOW(6), NOW(6), NOW(6))",
    )
    .bind(session_id)
    .bind(user_id)
    .execute(pool)
    .await
    .expect("seed run persistence session");
    sqlx::query(
        "INSERT INTO agent_session_lifecycle_fences
         (user_id, session_id, created_at, updated_at)
         VALUES (?, ?, NOW(6), NOW(6))",
    )
    .bind(user_id)
    .bind(session_id)
    .execute(pool)
    .await
    .expect("seed run persistence lifecycle fence");
    sqlx::query(
        "INSERT INTO agent_runs
         (run_id, user_id, session_id, root_run_id, ancestor_path, depth, retry_scope,
          status, execution_mode, owner_pod_id, owner_lease_expires_at,
          run_generation, last_event_idx, retry_count,
          total_prompt_tokens, total_completion_tokens, total_tool_calls,
          created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, 0, 'node', 'running', 'web_agent', ?,
                 TIMESTAMPADD(MINUTE, 5, NOW(6)), 0, -1, 0,
                 0, 0, 0, NOW(6), NOW(6))",
    )
    .bind(run_id)
    .bind(user_id)
    .bind(session_id)
    .bind(run_id)
    .bind(run_id)
    .bind(TEST_OWNER_POD_ID)
    .execute(pool)
    .await
    .expect("seed run persistence run");
}

async fn hold_pool_checkouts(
    pool: &sqlx::Pool<sqlx::MySql>,
    count: usize,
) -> Vec<sqlx::pool::PoolConnection<sqlx::MySql>> {
    let mut held = Vec::with_capacity(count);
    for _ in 0..count {
        held.push(
            tokio::time::timeout(std::time::Duration::from_secs(5), pool.acquire())
                .await
                .expect("acquire run cancellation fixture before deadline")
                .expect("acquire run cancellation fixture"),
        );
    }
    held
}

async fn cleanup(pool: &sqlx::Pool<sqlx::MySql>, user_id: &str, session_id: &str, run_id: &str) {
    for statement in [
        "DELETE FROM run_display_projections WHERE user_id = ? AND run_id = ?",
        "DELETE FROM agent_run_events WHERE user_id = ? AND run_id = ?",
        "DELETE FROM agent_runs WHERE user_id = ? AND run_id = ?",
    ] {
        sqlx::query(statement)
            .bind(user_id)
            .bind(run_id)
            .execute(pool)
            .await
            .expect("clean run persistence fixture");
    }
    sqlx::query("DELETE FROM agent_session_lifecycle_fences WHERE user_id = ? AND session_id = ?")
        .bind(user_id)
        .bind(session_id)
        .execute(pool)
        .await
        .expect("clean run persistence lifecycle fence");
    sqlx::query("DELETE FROM agent_sessions WHERE user_id = ? AND session_id = ?")
        .bind(user_id)
        .bind(session_id)
        .execute(pool)
        .await
        .expect("clean run persistence session");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires live DB: run with ASTRA_TEST_DB_IT=1"]
#[serial]
async fn cancelled_run_event_append_closes_its_physical_checkout() {
    let shared_pool = common::setup_pool().await;
    let pool = shared_pool.get();
    let max_connections = shared_pool.stats().max_connections as usize;
    assert!(
        max_connections >= 3,
        "cancellation isolation requires blocker, worker, and health-query capacity"
    );

    let suffix = Uuid::new_v4().simple().to_string();
    let user_id = format!("run-cancel-user-{suffix}");
    let session_id = format!("run-cancel-session-{suffix}");
    let run_id = format!("run-cancel-run-{suffix}");
    seed_run(pool, &user_id, &session_id, &run_id).await;
    let store =
        DatabaseRunStateStore::new(shared_pool.clone()).with_owner_pod_id(TEST_OWNER_POD_ID);

    // The append acquires the session lifecycle fence first and then blocks on
    // this run row. Cancelling it at that point must close its checkout; merely
    // returning the connection to the pool would preserve the open transaction
    // and strand the lifecycle fence.
    let mut run_blocker = pool.begin().await.expect("begin run row blocker");
    sqlx::query(
        "SELECT run_id FROM agent_runs
         WHERE user_id = ? AND session_id = ? AND run_id = ? FOR UPDATE",
    )
    .bind(&user_id)
    .bind(&session_id)
    .bind(&run_id)
    .fetch_one(&mut *run_blocker)
    .await
    .expect("lock run row");
    let held = hold_pool_checkouts(pool, max_connections - 2).await;

    let event = serde_json::json!({
        "event_type": "cancellation_safe_append_probe",
        "idempotency_key": format!("append-probe-{suffix}"),
        "data": {},
    });
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            store
                .append_events_batch(&user_id, &session_id, &run_id, std::slice::from_ref(&event),),
        )
        .await
        .is_err(),
        "run event append must still be blocked when its caller cancels"
    );

    let value: i64 = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        sqlx::query_scalar("SELECT 1").fetch_one(pool),
    )
    .await
    .expect("cancelled run checkout must release pool capacity")
    .expect("independent query after run append cancellation");
    assert_eq!(value, 1);

    drop(held);
    run_blocker
        .rollback()
        .await
        .expect("release run row blocker");
    store
        .append_events_batch(&user_id, &session_id, &run_id, std::slice::from_ref(&event))
        .await
        .expect("retry append after cancellation");

    cleanup(pool, &user_id, &session_id, &run_id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires live DB: run with ASTRA_TEST_DB_IT=1"]
#[serial]
async fn existing_run_start_claim_releases_rollback_checkout_before_reread() {
    let shared_pool = common::setup_pool().await;
    let pool = shared_pool.get();
    let max_connections = shared_pool.stats().max_connections as usize;
    assert!(
        max_connections >= 2,
        "idempotent replay requires worker and fixture capacity"
    );

    let suffix = Uuid::new_v4().simple().to_string();
    let user_id = format!("run-replay-user-{suffix}");
    let session_id = format!("run-replay-session-{suffix}");
    let run_id = format!("run-replay-run-{suffix}");
    seed_run(pool, &user_id, &session_id, &run_id).await;
    let store =
        DatabaseRunStateStore::new(shared_pool.clone()).with_owner_pod_id(TEST_OWNER_POD_ID);
    let existing = store
        .load_run(&user_id, &run_id)
        .await
        .expect("load replay fixture")
        .expect("replay fixture exists");

    // Leave exactly one checkout available. The replay transaction must
    // release that checkout after rollback before its authoritative reread,
    // otherwise it waits forever for capacity held by itself.
    let held = hold_pool_checkouts(pool, max_connections - 1).await;
    let claim = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        store.claim_run_start(existing, Some(&session_id)),
    )
    .await
    .expect("idempotent replay must not self-deadlock")
    .expect("idempotent replay claim");
    assert_eq!(
        claim,
        DurableRunStartClaim::Existing {
            session_id: session_id.clone(),
            start_request_fingerprint: None,
        }
    );

    drop(held);
    cleanup(pool, &user_id, &session_id, &run_id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires live DB: run with ASTRA_TEST_DB_IT=1"]
#[serial]
async fn queued_interaction_conflict_releases_rollback_checkout_before_control_reread() {
    let shared_pool = common::setup_pool().await;
    let pool = shared_pool.get();
    let max_connections = shared_pool.stats().max_connections as usize;
    assert!(
        max_connections >= 2,
        "queued interaction conflict requires worker and fixture capacity"
    );

    let suffix = Uuid::new_v4().simple().to_string();
    let user_id = format!("interaction-conflict-user-{suffix}");
    let session_id = format!("interaction-conflict-session-{suffix}");
    let run_id = format!("interaction-conflict-run-{suffix}");
    let request_id = format!("interaction-conflict-request-{suffix}");
    seed_run(pool, &user_id, &session_id, &run_id).await;
    let store =
        DatabaseRunStateStore::new(shared_pool.clone()).with_owner_pod_id(TEST_OWNER_POD_ID);
    let required = serde_json::json!({
        "event_type": "approval_required",
        "idempotency_key": format!("approval:{request_id}:required"),
        "data": {
            "request_id": request_id,
            "session_id": session_id,
            "tool": "bash",
            "approval_kind": "standard",
        }
    });
    assert_eq!(
        store
            .register_guarded_interaction_batch(AtomicRunInteractionBatchRegistrationRequest {
                user_id: &user_id,
                run_id: &run_id,
                expected_session_id: &session_id,
                expected_control_epoch: -1,
                expected_owner_generation: 0,
                events: std::slice::from_ref(&required),
            })
            .await
            .expect("register queued interaction fixture"),
        AtomicRunInteractionBatchRegistration::Registered
    );
    assert!(matches!(
        store
            .resolve_run_interaction(
                &user_id,
                &session_id,
                &run_id,
                &request_id,
                DurableRunInteractionKind::Approval,
                serde_json::json!({
                    "request_id": request_id,
                    "outcome": "approved",
                    "decision": "allow",
                    "tool": "bash",
                    "approval_kind": "standard",
                }),
            )
            .await
            .expect("queue interaction response before wait"),
        DurableRunInteractionResolveOutcome::Queued(_)
    ));
    sqlx::query(
        "UPDATE agent_runs SET cancellation_requested_at = NOW(6)
         WHERE user_id = ? AND session_id = ? AND run_id = ?",
    )
    .bind(&user_id)
    .bind(&session_id)
    .bind(&run_id)
    .execute(pool)
    .await
    .expect("make queued promotion CAS lose authority");

    // Leave one checkout for the wait transaction. Its failed promotion must
    // release that checkout before the pool-backed cancellation reread.
    let held = hold_pool_checkouts(pool, max_connections - 1).await;
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        store.begin_run_interaction_wait(AtomicRunInteractionWaitRequest {
            user_id: &user_id,
            run_id: &run_id,
            expected_session_id: &session_id,
            request_id: &request_id,
            kind: DurableRunInteractionKind::Approval,
            expected_control_epoch: -1,
            expected_owner_generation: 0,
        }),
    )
    .await
    .expect("queued interaction control reread must not self-deadlock")
    .expect("queued interaction conflict outcome");
    assert_eq!(outcome, DurableRunInteractionWaitOutcome::NoLongerActive);

    drop(held);
    cleanup(pool, &user_id, &session_id, &run_id).await;
}

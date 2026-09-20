//! Live MatrixOne regression tests for context-manifest transactional writes.
//!
//! ```text
//! ASTRA_TEST_DB_IT=1 cargo test -p astra-services --test context_manifest_db_it -- --ignored --nocapture --test-threads=1
//! ```

use astra_core::SharedPool;
use astra_services::{
    ContextManifestError, ContextManifestItemWrite, ContextManifestWrite,
    DatabaseContextManifestStore,
};
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

mod common;

fn id(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::new_v4().simple())
}

async fn insert_session(pool: &SharedPool, user_id: &str, session_id: &str) {
    sqlx::query(
        "INSERT INTO agent_sessions
         (session_id, user_id, agent_id, title, status, metadata, created_at, updated_at)
         VALUES (?, ?, 'manifest-db-it', 'manifest db integration', 'active', '{}', NOW(6), NOW(6))",
    )
    .bind(session_id)
    .bind(user_id)
    .execute(pool.get())
    .await
    .expect("insert active session");
}

fn manifest(
    manifest_id: &str,
    user_id: &str,
    session_id: &str,
    run_id: Option<&str>,
) -> ContextManifestWrite {
    ContextManifestWrite {
        manifest_id: manifest_id.to_string(),
        user_id: user_id.to_string(),
        session_id: session_id.to_string(),
        run_id: run_id.map(str::to_string),
        turn_id: id("turn"),
        model_provider: "test".to_string(),
        model_name: "manifest-db-it".to_string(),
        context_window_tokens: 8_000,
        max_output_tokens: 512,
        total_estimated_tokens: 1_024,
        policy_version: "context_manifest_v1".to_string(),
        tokenizer_id: None,
        budget_template_id: None,
        turn_intent: None,
        reason: "normal_turn".to_string(),
        manifest_json: json!({"test": "context_manifest_db_it"}),
    }
}

fn item(session_id: &str, item_order: i32) -> ContextManifestItemWrite {
    ContextManifestItemWrite {
        session_id: session_id.to_string(),
        item_order,
        zone: "recent_tail".to_string(),
        source_table: "runtime_messages".to_string(),
        source_id: format!("message-{item_order}"),
        source_hash: None,
        included: true,
        token_estimate: 8,
        budget_tokens: 16,
        reason: "normal_turn".to_string(),
        render_mode: "plain_text".to_string(),
        raw_ref: None,
    }
}

fn assert_duplicate_item_insert(error: ContextManifestError) {
    match error {
        ContextManifestError::Database {
            operation: "insert_context_manifest_items",
            source,
            ..
        } => {
            let database_error = source
                .as_database_error()
                .expect("duplicate item order must be a database error");
            assert_eq!(
                database_error.code().as_deref(),
                Some("23000"),
                "expected integrity-constraint SQLSTATE"
            );
            assert!(
                database_error
                    .message()
                    .to_ascii_lowercase()
                    .contains("duplicate"),
                "expected duplicate-key message, got {}",
                database_error.message()
            );
        }
        other => panic!("expected duplicate item insert failure, got {other:?}"),
    }
}

#[tokio::test]
#[ignore = "requires live MatrixOne (ASTRA_TEST_DB_IT=1)"]
async fn later_item_batch_failure_rolls_back_manifest_and_all_items() {
    let pool = common::setup_pool().await;
    let user_id = id("manifest-rollback-user");
    let session_id = id("manifest-rollback-session");
    let manifest_id = id("manifest-rollback");
    insert_session(&pool, &user_id, &session_id).await;

    let mut items = (0..129)
        .map(|order| item(&session_id, order))
        .collect::<Vec<_>>();
    // Item 128 is in the second SQL batch and conflicts with item 0 from the
    // already-successful first batch.
    items[128].item_order = 0;

    let store = DatabaseContextManifestStore::new(pool.clone());
    let control_id = id("manifest-multibatch-control");
    store
        .save_manifest(
            manifest(&control_id, &user_id, &session_id, None),
            (0..129).map(|order| item(&session_id, order)).collect(),
        )
        .await
        .expect("129-item control must cross the batch boundary successfully");
    let control_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM context_manifest_items WHERE manifest_id = ?")
            .bind(&control_id)
            .fetch_one(pool.get())
            .await
            .expect("count multi-batch control items");
    assert_eq!(control_count, 129);

    let error = store
        .save_manifest(manifest(&manifest_id, &user_id, &session_id, None), items)
        .await
        .expect_err("duplicate item order must fail the write");
    assert_duplicate_item_insert(error);

    let manifest_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM context_manifests WHERE manifest_id = ?")
            .bind(&manifest_id)
            .fetch_one(pool.get())
            .await
            .expect("count rolled-back manifest");
    let item_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM context_manifest_items WHERE manifest_id = ?")
            .bind(&manifest_id)
            .fetch_one(pool.get())
            .await
            .expect("count rolled-back manifest items");
    assert_eq!((manifest_count, item_count), (0, 0));
}

#[tokio::test]
#[ignore = "requires live MatrixOne (ASTRA_TEST_DB_IT=1)"]
async fn artifact_reference_updates_are_exact_and_roll_back_after_later_update_failure() {
    let pool = common::setup_pool().await;
    let user_id = id("artifact-rollback-user");
    let session_id = id("artifact-rollback-session");
    let manifest_id = id("artifact-rollback-manifest");
    let first_artifact_id = format!("a-{}", Uuid::new_v4().simple());
    let failing_artifact_id = format!("z-{}", Uuid::new_v4().simple());
    let unaffected_artifact_id = id("unaffected-artifact");
    insert_session(&pool, &user_id, &session_id).await;
    for (artifact_id, reference_count) in [
        (&first_artifact_id, 7_i64),
        (&failing_artifact_id, i32::MAX as i64),
        (&unaffected_artifact_id, 11_i64),
    ] {
        sqlx::query(
            "INSERT INTO session_artifacts
         (artifact_id, session_id, user_id, artifact_kind, content_json, metadata,
          retention_policy, status, referenced_by_manifest_count, created_at, updated_at)
         VALUES (?, ?, ?, 'test', '{}', '{}', 'default', 'active', ?, NOW(6), NOW(6))",
        )
        .bind(artifact_id)
        .bind(&session_id)
        .bind(&user_id)
        .bind(reference_count)
        .execute(pool.get())
        .await
        .expect("insert artifact");
    }

    let control_id = id("artifact-reference-control");
    let mut control_items = vec![
        item(&session_id, 0),
        item(&session_id, 1),
        item(&session_id, 2),
    ];
    for value in &mut control_items[..2] {
        value.source_table = "session_artifacts".to_string();
        value.source_id = first_artifact_id.clone();
    }
    control_items[2].source_table = "session_artifacts".to_string();
    control_items[2].source_id = unaffected_artifact_id.clone();
    DatabaseContextManifestStore::new(pool.clone())
        .save_manifest(
            manifest(&control_id, &user_id, &session_id, None),
            control_items,
        )
        .await
        .expect("successful artifact references must commit");
    for (artifact_id, expected) in [
        (&first_artifact_id, 9_i64),
        (&unaffected_artifact_id, 12_i64),
    ] {
        let actual: i64 = sqlx::query_scalar(
            "SELECT referenced_by_manifest_count FROM session_artifacts
             WHERE user_id = ? AND session_id = ? AND artifact_id = ?",
        )
        .bind(&user_id)
        .bind(&session_id)
        .bind(artifact_id)
        .fetch_one(pool.get())
        .await
        .expect("load committed artifact reference count");
        assert_eq!(
            actual, expected,
            "committed references must preserve multiplicity"
        );
    }

    let items = (0..129)
        .map(|order| {
            let mut value = item(&session_id, order);
            value.source_table = "session_artifacts".to_string();
            value.source_id = if order < 128 {
                first_artifact_id.clone()
            } else {
                failing_artifact_id.clone()
            };
            value
        })
        .collect::<Vec<_>>();

    let error = DatabaseContextManifestStore::new(pool.clone())
        .save_manifest(manifest(&manifest_id, &user_id, &session_id, None), items)
        .await
        .expect_err("overflow in the second artifact update must fail the transaction");
    match error {
        ContextManifestError::Database {
            operation: "increment_manifest_artifact_ref",
            entity,
            ..
        } => assert_eq!(entity, failing_artifact_id),
        other => panic!("expected artifact-reference update failure, got {other:?}"),
    }

    for (artifact_id, expected) in [
        (&first_artifact_id, 9_i64),
        (&failing_artifact_id, i32::MAX as i64),
        (&unaffected_artifact_id, 12_i64),
    ] {
        let actual: i64 = sqlx::query_scalar(
            "SELECT referenced_by_manifest_count FROM session_artifacts
             WHERE user_id = ? AND session_id = ? AND artifact_id = ?",
        )
        .bind(&user_id)
        .bind(&session_id)
        .bind(artifact_id)
        .fetch_one(pool.get())
        .await
        .expect("load artifact reference count");
        assert_eq!(
            actual, expected,
            "failed transaction must restore {artifact_id}"
        );
    }
    let manifest_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM context_manifests WHERE manifest_id = ?")
            .bind(&manifest_id)
            .fetch_one(pool.get())
            .await
            .expect("count rolled-back manifest");
    let item_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM context_manifest_items WHERE manifest_id = ?")
            .bind(&manifest_id)
            .fetch_one(pool.get())
            .await
            .expect("count rolled-back items");
    assert_eq!((manifest_count, item_count), (0, 0));
}

#[tokio::test]
#[ignore = "requires live MatrixOne (ASTRA_TEST_DB_IT=1)"]
async fn one_connection_preserves_distinct_nullable_parameter_shapes() {
    let (_, mut settings) = common::setup_pool_and_settings().await;
    settings.db_pool_min_connections = 1;
    settings.db_pool_max_connections = 1;
    let pool = SharedPool::new(&settings)
        .await
        .expect("create one-connection MatrixOne pool");
    let user_id = id("nullable-user");
    let session_id = id("nullable-session");
    insert_session(&pool, &user_id, &session_id).await;
    let store = DatabaseContextManifestStore::new(pool.clone());

    let first_id = id("nullable-all-none");
    let mut first_items = vec![
        item(&session_id, 0),
        item(&session_id, 1),
        item(&session_id, 2),
    ];
    first_items[1].source_hash = Some("first-hash-only".to_string());
    first_items[2].raw_ref = Some("conversation_log://nullable/first-ref-only".to_string());
    store
        .save_manifest(
            manifest(&first_id, &user_id, &session_id, None),
            first_items,
        )
        .await
        .expect("save first mixed nullable shape");

    let second_id = id("nullable-raw-ref-change");
    let mut second = manifest(&second_id, &user_id, &session_id, Some("run-present"));
    second.tokenizer_id = Some("tokenizer-present".to_string());
    second.budget_template_id = Some("budget-present".to_string());
    second.turn_intent = Some("intent-present".to_string());
    let mut second_items = vec![
        item(&session_id, 0),
        item(&session_id, 1),
        item(&session_id, 2),
    ];
    second_items[1].source_hash = Some("first-hash-only".to_string());
    second_items[1].raw_ref = Some("conversation_log://nullable/second-ref-only".to_string());
    store
        .save_manifest(second, second_items)
        .await
        .expect("save raw-ref-only shape change on the same connection");

    let third_id = id("nullable-source-hash-change");
    let mut third = manifest(&third_id, &user_id, &session_id, Some("run-third"));
    third.tokenizer_id = Some("tokenizer-third".to_string());
    third.budget_template_id = Some("budget-third".to_string());
    third.turn_intent = Some("intent-third".to_string());
    let mut third_items = vec![
        item(&session_id, 0),
        item(&session_id, 1),
        item(&session_id, 2),
    ];
    third_items[1].raw_ref = Some("conversation_log://nullable/second-ref-only".to_string());
    third_items[2].source_hash = Some("third-hash-only".to_string());
    store
        .save_manifest(third, third_items)
        .await
        .expect("save source-hash-only shape change on the same connection");

    let first = sqlx::query(
        "SELECT run_id, tokenizer_id, budget_template_id, turn_intent
         FROM context_manifests WHERE manifest_id = ?",
    )
    .bind(&first_id)
    .fetch_one(pool.get())
    .await
    .expect("load all-NULL manifest");
    assert_eq!(first.try_get::<Option<String>, _>("run_id").unwrap(), None);
    assert_eq!(
        first.try_get::<Option<String>, _>("tokenizer_id").unwrap(),
        None
    );
    assert_eq!(
        first
            .try_get::<Option<String>, _>("budget_template_id")
            .unwrap(),
        None
    );
    assert_eq!(
        first.try_get::<Option<String>, _>("turn_intent").unwrap(),
        None
    );

    let second = sqlx::query(
        "SELECT run_id, tokenizer_id, budget_template_id, turn_intent
         FROM context_manifests WHERE manifest_id = ?",
    )
    .bind(&second_id)
    .fetch_one(pool.get())
    .await
    .expect("load all-non-NULL manifest");
    assert_eq!(
        second.try_get::<String, _>("run_id").unwrap(),
        "run-present"
    );
    assert_eq!(
        second.try_get::<String, _>("tokenizer_id").unwrap(),
        "tokenizer-present"
    );
    assert_eq!(
        second.try_get::<String, _>("budget_template_id").unwrap(),
        "budget-present"
    );
    assert_eq!(
        second.try_get::<String, _>("turn_intent").unwrap(),
        "intent-present"
    );

    for (manifest_id, expected) in [
        (
            &first_id,
            vec![
                (None, None),
                (Some("first-hash-only"), None),
                (None, Some("conversation_log://nullable/first-ref-only")),
            ],
        ),
        (
            &second_id,
            vec![
                (None, None),
                (
                    Some("first-hash-only"),
                    Some("conversation_log://nullable/second-ref-only"),
                ),
                (None, None),
            ],
        ),
        (
            &third_id,
            vec![
                (None, None),
                (None, Some("conversation_log://nullable/second-ref-only")),
                (Some("third-hash-only"), None),
            ],
        ),
    ] {
        let rows = sqlx::query(
            "SELECT source_hash, raw_ref FROM context_manifest_items
             WHERE manifest_id = ? ORDER BY item_order",
        )
        .bind(manifest_id)
        .fetch_all(pool.get())
        .await
        .expect("load mixed nullable item shapes");
        let actual = rows
            .iter()
            .map(|row| {
                (
                    row.try_get::<Option<String>, _>("source_hash").unwrap(),
                    row.try_get::<Option<String>, _>("raw_ref").unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let expected = expected
            .into_iter()
            .map(|(hash, raw_ref)| (hash.map(str::to_string), raw_ref.map(str::to_string)))
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }
}

#[tokio::test]
#[ignore = "requires live MatrixOne (ASTRA_TEST_DB_IT=1)"]
async fn pending_delete_fence_rejects_manifest_without_partial_rows() {
    let pool = common::setup_pool().await;
    let user_id = id("deleting-user");
    let session_id = id("deleting-session");
    let manifest_id = id("deleting-manifest");
    insert_session(&pool, &user_id, &session_id).await;
    let fence = sqlx::query(
        "INSERT INTO agent_session_lifecycle_fences
         (session_id, user_id, delete_requested_at, created_at, updated_at)
         VALUES (?, ?, NOW(6), NOW(6), NOW(6))",
    )
    .bind(&session_id)
    .bind(&user_id)
    .execute(pool.get())
    .await
    .expect("insert pending-delete session fence");
    assert_eq!(fence.rows_affected(), 1, "pending-delete fence must exist");

    let result = DatabaseContextManifestStore::new(pool.clone())
        .save_manifest(
            manifest(&manifest_id, &user_id, &session_id, None),
            vec![item(&session_id, 0)],
        )
        .await;
    assert!(
        result.is_err(),
        "pending delete must reject a manifest write"
    );

    let manifest_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM context_manifests WHERE manifest_id = ?")
            .bind(&manifest_id)
            .fetch_one(pool.get())
            .await
            .expect("count rejected manifest");
    let item_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM context_manifest_items WHERE manifest_id = ?")
            .bind(&manifest_id)
            .fetch_one(pool.get())
            .await
            .expect("count rejected manifest items");
    assert_eq!((manifest_count, item_count), (0, 0));
}

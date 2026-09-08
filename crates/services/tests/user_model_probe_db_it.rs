//! Real model service + isolated DB + strict, non-billable provider fixture.
//! Run with ASTRA_TEST_DB_IT=1, ASTRA_TEST_DATABASE=$ASTRA_DATABASE,
//! ASTRA_ALLOW_INSECURE_DEFAULTS=1 and ASTRA_BYOK_DEEPSEEK_BASE_URL set to
//! an unused loopback HTTP origin (e.g. http://127.0.0.1:18994).
//! Explicitly selected with --features external-contract-tests; ordinary
//! online lanes do not provide this operator-only endpoint override.
mod common;
#[path = "common/isolated_database.rs"]
mod isolated_database;

use astra_services::{
    DatabaseModelService, FernetTokenEncryptor, ModelService,
    models::{UserModelCreateRequestData, UserModelUpdateRequestData},
};
use axum::{Json, Router, http::StatusCode, routing::post};
use serde_json::{Value, json};
use std::sync::Arc;

#[tokio::test]
#[ignore = "requires isolated MatrixOne DB and loopback DeepSeek fixture override"]
async fn user_model_create_rotate_and_probe_enforce_provider_wire_contract() {
    let settings = astra_core::MatrixOneSettings::from_env();
    isolated_database::require_isolated_database(&settings.database);
    assert_eq!(
        std::env::var("ASTRA_ALLOW_INSECURE_DEFAULTS").as_deref(),
        Ok("1")
    );
    let base = std::env::var("ASTRA_BYOK_DEEPSEEK_BASE_URL").expect("loopback fixture origin");
    let url = reqwest::Url::parse(&base).unwrap();
    assert_eq!(url.scheme(), "http");
    assert_eq!(url.host_str(), Some("127.0.0.1"));
    let app = Router::new()
        .route(
            "/chat/completions",
            post(
                |headers: axum::http::HeaderMap, Json(body): Json<Value>| async move {
                    // Each simulated upstream enforces its own documented
                    // field; do not reuse the serializer under test here.
                    let (limit, forbidden) = match body["model"].as_str() {
                        Some("deepseek-chat") => ("max_tokens", "max_completion_tokens"),
                        _ => ("max_completion_tokens", "max_tokens"),
                    };
                    let status = if headers
                        .get("authorization")
                        .is_none_or(|v| v != "Bearer valid-key" && v != "Bearer rotated-key")
                    {
                        StatusCode::UNAUTHORIZED
                    } else if body["model"] != "deepseek-chat" && body["model"] != "o3" {
                        StatusCode::NOT_FOUND
                    } else if body[limit] != 32 || body.get(forbidden).is_some() {
                        StatusCode::BAD_REQUEST
                    } else {
                        StatusCode::OK
                    };
                    (
                        status,
                        Json(json!({"error":{"message":"strict fixture rejection"}})),
                    )
                },
            ),
        )
        .route(
            "/v1/messages",
            post(
                |headers: axum::http::HeaderMap, Json(body): Json<Value>| async move {
                    let status = if headers
                        .get("x-api-key")
                        .is_none_or(|v| v != "valid-key" && v != "rotated-key")
                    {
                        StatusCode::UNAUTHORIZED
                    } else if body["model"] != "claude-sonnet-4-5" {
                        StatusCode::NOT_FOUND
                    } else if headers
                        .get("anthropic-version")
                        .is_none_or(|v| v != "2023-06-01")
                        || body["max_tokens"] != 32
                        || body.get("max_completion_tokens").is_some()
                    {
                        StatusCode::BAD_REQUEST
                    } else {
                        StatusCode::OK
                    };
                    (
                        status,
                        Json(json!({"error":{"message":"strict fixture rejection"}})),
                    )
                },
            ),
        );
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", url.port().unwrap()))
        .await
        .unwrap();
    let fixture = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (pool, settings) = common::setup_pool_and_settings().await;
    let encryptor = FernetTokenEncryptor::new("provider-wire-test-only-key").unwrap();
    let service =
        DatabaseModelService::new(settings, Arc::new(encryptor.clone())).with_pool(pool.clone());
    let owner = uuid::Uuid::new_v4().to_string();
    let make_request = |name: &str, model: &str, key: &str| UserModelCreateRequestData {
        name: name.into(),
        provider: "deepseek".into(),
        model: model.into(),
        base_url: None,
        api_key: key.into(),
        context_window: 128000,
        is_default: true,
    };
    for (model, key) in [
        ("deepseek-chat", "invalid-key"),
        ("missing-model", "valid-key"),
    ] {
        let error = service
            .create_user_model(owner.clone(), make_request("rejected", model, key))
            .await
            .unwrap_err();
        assert_eq!(error.0, StatusCode::BAD_REQUEST);
        assert!(
            service
                .list_user_models(owner.clone())
                .await
                .unwrap()
                .is_empty(),
            "failed create persisted data"
        );
    }
    let created = service
        .create_user_model(
            owner.clone(),
            make_request("fixture", "deepseek-chat", "valid-key"),
        )
        .await
        .unwrap();
    // Fixed official endpoints are not user-overridable. Seed their *test row*
    // with this loopback fixture to exercise the real rotate/probe service paths
    // without adding a production transport bypass or spending a live API key.
    for (provider, model) in [
        ("deepseek", "deepseek-chat"),
        ("openai", "o3"),
        ("anthropic", "claude-sonnet-4-5"),
    ] {
        sqlx::query("UPDATE user_llm_models SET provider = ?, model_name = ?, api_key_encrypted = ? WHERE user_id = ? AND model_id = ?")
            .bind(provider).bind(model).bind(encryptor.encrypt("valid-key").unwrap()).bind(&owner).bind(&created.model_id).execute(pool.get()).await.unwrap();
        service
            .check_user_model(owner.clone(), created.model_id.clone())
            .await
            .unwrap();
        let before: String = sqlx::query_scalar(
            "SELECT api_key_encrypted FROM user_llm_models WHERE user_id = ? AND model_id = ?",
        )
        .bind(&owner)
        .bind(&created.model_id)
        .fetch_one(pool.get())
        .await
        .unwrap();
        let error = service
            .update_user_model(
                owner.clone(),
                created.model_id.clone(),
                UserModelUpdateRequestData {
                    api_key: Some("invalid-key".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.0, StatusCode::BAD_REQUEST, "{provider}: {error:?}");
        let after: String = sqlx::query_scalar(
            "SELECT api_key_encrypted FROM user_llm_models WHERE user_id = ? AND model_id = ?",
        )
        .bind(&owner)
        .bind(&created.model_id)
        .fetch_one(pool.get())
        .await
        .unwrap();
        assert_eq!(before, after, "failed rotation overwrote credential");
        service
            .update_user_model(
                owner.clone(),
                created.model_id.clone(),
                UserModelUpdateRequestData {
                    api_key: Some("rotated-key".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let after: String = sqlx::query_scalar(
            "SELECT api_key_encrypted FROM user_llm_models WHERE user_id = ? AND model_id = ?",
        )
        .bind(&owner)
        .bind(&created.model_id)
        .fetch_one(pool.get())
        .await
        .unwrap();
        assert_eq!(encryptor.decrypt(&after).unwrap(), "rotated-key");
        service
            .check_user_model(owner.clone(), created.model_id.clone())
            .await
            .unwrap();
    }
    service
        .delete_user_model(owner, created.model_id)
        .await
        .unwrap();
    fixture.abort();
}

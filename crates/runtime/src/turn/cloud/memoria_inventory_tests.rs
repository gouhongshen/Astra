//! Cursor protocol tests through the real HTTP transport, without a database.
use super::*;
use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy)]
enum Fault {
    None,
    MissingCursor,
    MalformedCursor,
    RepeatedCursor,
    DuplicateRow,
    ForeignOwner,
    SecondPageFailure,
    EmptyContinuation,
}

struct Backend {
    rows: Vec<Value>,
    queries: Vec<BTreeMap<String, String>>,
    fault: Fault,
}

async fn list(
    State(state): State<Arc<Mutex<Backend>>>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> (StatusCode, Json<Value>) {
    let mut backend = state.lock().unwrap();
    assert_eq!(headers["x-user-id"], "owner-1");
    assert_eq!(query["session_id"], "session-1");
    assert_eq!(query["memory_type"], "working");
    assert_eq!(query["branch"], "main");
    let limit = query["limit"].parse::<usize>().unwrap();
    assert!((1..=500).contains(&limit));
    let offset = query
        .get("cursor")
        .map(|cursor| {
            backend
                .rows
                .iter()
                .position(|row| row["memory_id"] == *cursor)
                .unwrap()
                + 1
        })
        .unwrap_or(0);
    backend.queries.push(query);
    let second = backend.queries.len() > 1;
    if second && matches!(backend.fault, Fault::SecondPageFailure) {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({})));
    }
    let mut rows: Vec<_> = backend
        .rows
        .iter()
        .skip(offset)
        .take(limit)
        .cloned()
        .collect();
    let mut next = if offset + rows.len() < backend.rows.len() {
        rows.last().unwrap()["memory_id"].clone()
    } else {
        Value::Null
    };
    match backend.fault {
        Fault::RepeatedCursor if second => {
            rows = backend.rows[..500].to_vec();
            next = rows.last().unwrap()["memory_id"].clone();
        }
        Fault::DuplicateRow if second => rows[0] = backend.rows[0].clone(),
        Fault::ForeignOwner if second => rows[0]["user_id"] = json!("foreign-owner"),
        Fault::EmptyContinuation => rows.clear(),
        Fault::MalformedCursor => next = json!({"invalid": true}),
        _ => {}
    }
    let mut body = json!({"items": rows, "next_cursor": next});
    if matches!(backend.fault, Fault::MissingCursor) {
        body.as_object_mut().unwrap().remove("next_cursor");
    }
    (StatusCode::OK, Json(body))
}

struct Fixture {
    client: HttpMemoriaPort,
    backend: Arc<Mutex<Backend>>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
async fn fixture(count: usize, fault: Fault) -> Fixture {
    let backend = Arc::new(Mutex::new(Backend {
        rows: (0..count).rev().map(|index| json!({
            "memory_id":format!("{index:032x}"), "user_id":"owner-1", "session_id":"session-1",
            "memory_type":"working", "content":"snapshot",
        })).collect(),
        queries: Vec::new(), fault,
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = HttpMemoriaPort::new(
        format!("http://{}", listener.local_addr().unwrap()),
        "test-key".into(),
    )
    .with_owner_user_id("owner-1")
    .for_durable_session();
    let app = Router::new()
        .route("/v1/memories", get(list))
        .with_state(backend.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Fixture {
        client,
        backend,
        server,
    }
}

#[tokio::test]
async fn complete_inventory_follows_cursor_past_500() {
    let f = fixture(501, Fault::None).await;
    let rows = f
        .client
        .retrieve_scoped_inventory("session-1", 4096, &["working"])
        .await
        .unwrap();
    assert_eq!(rows.len(), 501);
    assert_eq!(rows.last().unwrap().memory_id, format!("{:032x}", 0));
    let backend = f.backend.lock().unwrap();
    assert_eq!(backend.queries.len(), 2);
    assert!(!backend.queries[0].contains_key("cursor"));
    assert_eq!(
        backend.queries[1]["cursor"],
        backend.rows[499]["memory_id"].as_str().unwrap()
    );
}

#[tokio::test]
async fn complete_inventory_accepts_exact_bound_only_with_end_of_pagination() {
    for count in [0, 500, 4096] {
        let f = fixture(count, Fault::None).await;
        let rows = f
            .client
            .retrieve_scoped_inventory("session-1", 4096, &["working"])
            .await
            .unwrap();
        assert_eq!(rows.len(), count);
        assert!(f.backend.lock().unwrap().queries.len() <= 9);
    }
    let f = fixture(4097, Fault::None).await;
    let error = f
        .client
        .retrieve_scoped_inventory("session-1", 4096, &["working"])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("exceeds bounded inventory"));
    assert_eq!(f.backend.lock().unwrap().queries.len(), 9);
}

#[tokio::test]
async fn complete_inventory_rejects_incomplete_or_untrustworthy_pages() {
    for fault in [
        Fault::MissingCursor,
        Fault::MalformedCursor,
        Fault::RepeatedCursor,
        Fault::DuplicateRow,
        Fault::ForeignOwner,
        Fault::SecondPageFailure,
        Fault::EmptyContinuation,
    ] {
        let f = fixture(501, fault).await;
        assert!(
            f.client
                .retrieve_scoped_inventory("session-1", 4096, &["working"])
                .await
                .is_err()
        );
        assert!(f.backend.lock().unwrap().queries.len() <= 2);
    }
}

#[tokio::test]
async fn ordinary_typed_recall_preserves_top_k_limit() {
    let f = fixture(501, Fault::None).await;
    let rows = f
        .client
        .retrieve_scoped_typed("query", "session-1", 500, &["working"])
        .await
        .unwrap();
    assert_eq!(rows.len(), 500);
    assert_eq!(f.backend.lock().unwrap().queries.len(), 1);
}

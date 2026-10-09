//! Durable compressed history, readable through the selected Server introspect boundary.
use astra_services::session_artifact_store::{
    SessionArtifactJsonRecord, SessionArtifactJsonStore, SessionArtifactReference,
    SessionArtifactReferenceKind, artifact_window_arguments, read_artifact_window,
};
use astra_turn_types::{
    CONTEXT_HISTORY_ARTIFACT_KIND, CONTEXT_HISTORY_ARTIFACT_URI_PREFIX, ContextHistoryArtifactV1,
};
use serde_json::Value;

pub(crate) async fn persist(
    store: &dyn SessionArtifactJsonStore,
    user_id: &str,
    artifact_id: &str,
    history: ContextHistoryArtifactV1,
) -> Result<(), String> {
    history
        .validate(&history.session_id)
        .map_err(str::to_owned)?;
    let session_id = history.session_id.clone();
    let record = SessionArtifactJsonRecord {
        artifact_id: artifact_id.into(),
        session_id: session_id.clone(),
        user_id: user_id.into(),
        artifact_kind: CONTEXT_HISTORY_ARTIFACT_KIND.into(),
        source: Some("context_compaction".into()),
        turn: None,
        round: None,
        content: serde_json::to_value(history).map_err(|e| e.to_string())?,
        metadata: None,
        references: vec![SessionArtifactReference {
            kind: SessionArtifactReferenceKind::SessionTranscript,
            reference_id: session_id,
        }],
    };
    store
        .persist_json_artifact(record)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub(crate) async fn resolve_request(
    store: Option<&dyn SessionArtifactJsonStore>,
    user_id: &str,
    session_id: &str,
    args: &Value,
) -> Result<String, String> {
    let handle = args
        .get("artifact")
        .and_then(Value::as_str)
        .ok_or("context history artifact must be a string handle")?;
    let id = handle
        .strip_prefix(CONTEXT_HISTORY_ARTIFACT_URI_PREFIX)
        .filter(|id| uuid::Uuid::parse_str(id).is_ok())
        .ok_or("invalid context history artifact handle")?;
    let store = store.ok_or("context history artifact reader is unavailable")?;
    let (offset, max_bytes) = artifact_window_arguments(args)?;
    let record = store
        .load_json_artifact(user_id, session_id, id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("context history artifact was not found for this session")?;
    if record.user_id != user_id
        || record.session_id != session_id
        || record.artifact_id != id
        || record.artifact_kind != CONTEXT_HISTORY_ARTIFACT_KIND
        || record.status.as_deref() != Some("active")
    {
        return Err("context history artifact identity or status is invalid".into());
    }
    let history: ContextHistoryArtifactV1 = serde_json::from_value(record.content)
        .map_err(|e| format!("invalid context history artifact: {e}"))?;
    history.validate(session_id).map_err(str::to_owned)?;
    let (window, total, next) = read_artifact_window(&history.transcript_json, offset, max_bytes)?;
    let continuation = if next == total {
        "Transcript complete.".into()
    } else {
        format!(
            "Continue with introspect(artifact=\"{handle}\", offset={next}, max_bytes={max_bytes})."
        )
    };
    Ok(format!(
        "<context-history-artifact>\nArtifact: {handle}\nSource run: {}\nMessages: {}\nBytes: [{offset}..{next}) of {total}\n\n{window}\n\n{continuation}\n</context-history-artifact>",
        history.source_run_id, history.message_count
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::explain_analyze_artifact::tests::MemoryStore;
    use serde_json::json;

    #[tokio::test]
    async fn stored_history_reads_exact_bytes_across_runs_and_rejects_foreign_or_corrupt_handles() {
        let store = MemoryStore::default();
        let id = uuid::Uuid::new_v4().to_string();
        let transcript =
            serde_json::to_string(&json!([{"role":"user","content":"中文 history"}])).unwrap();
        let history =
            ContextHistoryArtifactV1::new("session", "earlier-run", 1, transcript.clone()).unwrap();
        persist(&store, "owner", &id, history).await.unwrap();
        let args = json!({"artifact":format!("{CONTEXT_HISTORY_ARTIFACT_URI_PREFIX}{id}")});
        let output = resolve_request(Some(&store), "owner", "session", &args)
            .await
            .unwrap();
        assert!(output.contains(&transcript));
        assert!(output.contains("Source run: earlier-run"));
        for (owner, session) in [("another-owner", "session"), ("owner", "another-session")] {
            assert!(
                resolve_request(Some(&store), owner, session, &args)
                    .await
                    .is_err()
            );
        }
        let prefix = json!({"artifact":args["artifact"],"max_bytes":10});
        let output = resolve_request(Some(&store), "owner", "session", &prefix)
            .await
            .unwrap();
        assert!(output.contains("offset=10"));
        assert!(!output.contains(&transcript));
        let key = ("owner".into(), "session".into(), id);
        store
            .artifacts
            .lock()
            .unwrap()
            .get_mut(&key)
            .unwrap()
            .content["transcript_json"] = json!("[]");
        assert!(
            resolve_request(Some(&store), "owner", "session", &args)
                .await
                .unwrap_err()
                .contains("integrity")
        );
        assert!(
            resolve_request(None, "owner", "session", &args)
                .await
                .is_err()
        );
    }
}

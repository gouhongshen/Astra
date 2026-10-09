use super::session_lessons::ensure_bootstrapped_lessons;
use crate::cli::session::session_state::SessionState;

pub(crate) async fn prepare_turn_adaptation(
    state: &mut SessionState,
    api: &astra_thin_client::ThinClient,
    token: &str,
    message: &str,
) {
    if state.drift_original_query.is_none() {
        state.drift_original_query = Some(message.to_string());
    }

    ensure_bootstrapped_lessons(state, api, token, message).await;
}

#[cfg(test)]
mod tests {
    use super::prepare_turn_adaptation;
    use crate::cli::session::session_state::SessionState;

    #[tokio::test]
    async fn prepare_turn_adaptation_records_original_query_without_classifying_text() {
        let api = astra_thin_client::ThinClient::new("http://127.0.0.1:9", None).unwrap();
        let mut state = SessionState {
            session_lessons_loaded: true,
            history: vec![("u1".into(), "a1".into()), ("u2".into(), "a2".into())],
            ..SessionState::default()
        };

        prepare_turn_adaptation(&mut state, &api, "token", "不对，修这里").await;

        assert_eq!(state.drift_original_query.as_deref(), Some("不对，修这里"));
    }
}

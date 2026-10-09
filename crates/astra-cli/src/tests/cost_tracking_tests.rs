use crate::cli::session::session_stats_scan;

#[test]
fn unavailable_cost_is_not_formatted_as_free() {
    for cost in [None, Some(f64::NAN), Some(f64::INFINITY), Some(-1.0)] {
        assert_eq!(
            session_stats_scan::format_optional_cost(cost),
            "unavailable"
        );
    }
    assert_eq!(
        session_stats_scan::format_optional_cost(Some(0.0)),
        "$0.0000"
    );
    assert_eq!(session_stats_scan::format_optional_cost(Some(1.5)), "$1.50");
}

#[test]
fn current_rate_scenario_preserves_unknown_prices_and_observed_counts() {
    let mut state = crate::cli::session::session_state::SessionState::default();
    state.total_prompt_tokens = 100;
    state.total_completion_tokens = 20;
    state.total_cache_read_tokens = 900;
    state.total_cache_creation_tokens = 30;
    state.turn = 2;
    let pricing = astra_services::models::PricingData {
        prompt: 0.01,
        completion: 0.02,
        cache_read: None,
        cache_write: None,
    };
    state.model = Some(
        crate::cli::session::session_state::SessionModelChoice::Selected(
            crate::cli::session::session_runtime::ServerModelSelection {
                name: "priced".into(),
                offering_id: "priced-offering".into(),
                context_window: None,
                pricing: Some(pricing),
            },
        ),
    );
    let rows = session_stats_scan::current_rate_cost_rows(&state);
    assert!(rows.contains(&("billing", "not a session bill".into())));
    assert!(rows.contains(&("coverage", "unknown".into())));
    assert!(rows.contains(&("attribution", "unknown".into())));
    assert!(rows.contains(&("cache read", "900 (unavailable)".into())));
    assert!(rows.contains(&("cache write", "30 (unavailable)".into())));
    assert!(rows.contains(&("scenario sum", "unavailable".into())));
    assert!(!rows.iter().any(|(label, value)| label.contains("avg")
        || value.contains("saved")
        || value.contains("%")));
    if let Some(crate::cli::session::session_state::SessionModelChoice::Selected(selection)) =
        state.model.as_mut()
    {
        selection.pricing.as_mut().unwrap().cache_read = Some(0.001);
    }
    if let Some(crate::cli::session::session_state::SessionModelChoice::Selected(selection)) =
        state.model.as_mut()
    {
        selection.pricing.as_mut().unwrap().cache_write = Some(0.01);
    }
    let rows = session_stats_scan::current_rate_cost_rows(&state);
    assert!(rows.contains(&("scenario sum", "$2.60".into())));
    if let Some(crate::cli::session::session_state::SessionModelChoice::Selected(selection)) =
        state.model.as_mut()
    {
        selection.pricing.as_mut().unwrap().cache_read = Some(0.0);
    }
    if let Some(crate::cli::session::session_state::SessionModelChoice::Selected(selection)) =
        state.model.as_mut()
    {
        selection.pricing.as_mut().unwrap().cache_write = Some(0.0);
    }
    let rows = session_stats_scan::current_rate_cost_rows(&state);
    assert!(rows.contains(&("cache read", "900 ($0.0000)".into())));
    assert!(rows.contains(&("scenario sum", "$1.40".into())));
    state.total_cache_read_tokens = 0;
    state.total_cache_creation_tokens = 0;
    if let Some(crate::cli::session::session_state::SessionModelChoice::Selected(selection)) =
        state.model.as_mut()
    {
        selection.pricing.as_mut().unwrap().cache_read = None;
    }
    if let Some(crate::cli::session::session_state::SessionModelChoice::Selected(selection)) =
        state.model.as_mut()
    {
        selection.pricing.as_mut().unwrap().cache_write = None;
    }
    let rows = session_stats_scan::current_rate_cost_rows(&state);
    assert!(rows.contains(&("scenario sum", "$1.40".into())));
}

// ── Explicit-rate scenarios ────────────────────────────────────────

#[test]
fn explicit_rate_scenarios() {
    let pricing = astra_services::models::PricingData {
        prompt: 0.000_003,
        completion: 0.000_015,
        cache_read: None,
        cache_write: None,
    };

    // basic: 1000 prompt + 500 completion → $0.0105
    let cost = pricing.estimated_cost_usd(1000, 500, 0, 0).unwrap();
    assert!((cost - 0.0105).abs() < 1e-10);

    // zero inputs
    assert_eq!(pricing.estimated_cost_usd(0, 0, 0, 0).unwrap(), 0.0);

    // zero pricing
    assert_eq!(
        astra_services::models::PricingData::default().estimated_cost_usd(10000, 5000, 0, 0,),
        Some(0.0)
    );

    // large values: 1M prompt + 500K completion → $10.50
    let cost = pricing
        .estimated_cost_usd(1_000_000, 500_000, 0, 0)
        .unwrap();
    assert!((cost - 10.5).abs() < 1e-6);

    // with explicit cache rates
    let cache_pricing = astra_services::models::PricingData {
        prompt: 0.000_003,
        completion: 0.000_015,
        cache_read: Some(0.000_000_3),
        cache_write: Some(0.000_003_75),
    };
    let cost = cache_pricing
        .estimated_cost_usd(500, 200, 1000, 100)
        .unwrap();
    let expected =
        (500.0 * 0.000_003) + (200.0 * 0.000_015) + (1000.0 * 0.000_000_3) + (100.0 * 0.000_003_75);
    assert!((cost - expected).abs() < 1e-10);

    // Missing cache pricing must not charge cached tokens at the prompt rate.
    assert_eq!(
        pricing.estimated_cost_usd(0, 0, 1000, 1000),
        None,
        "unknown cache rates are unpriced instead of guessed"
    );
}

// ── format_cost ─────────────────────────────────────────────────────

#[test]
fn format_cost() {
    for (input, expected) in [
        (0.0001, "$0.0001"),
        (0.0099, "$0.0099"),
        (0.01, "$0.010"),
        (0.123, "$0.123"),
        (0.999, "$0.999"),
        (1.0, "$1.00"),
        (12.345, "$12.35"),
        (100.0, "$100.00"),
        (0.0, "$0.0000"),
    ] {
        assert_eq!(session_stats_scan::format_cost(input), expected);
    }
}

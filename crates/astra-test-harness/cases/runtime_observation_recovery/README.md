# Ordinary runtime observation recovery

`ordinary_previous_run` disables Explain capture explicitly. The harness's
default `--explain=on` otherwise hides failures specific to normal conversations.
Its oracle checks the actual prior-run selector, persisted projection, and tool
outcome, not a successful final answer or a model's telemetry claims.

Run this case and `subagent_model_selection/flash_scoped_child_and_parent`
against each configured primary model with `astra-test --force-model <model>`.
The delegated-model fixture must authorize one GLM candidate; production code
does not branch on either the primary or delegated model's name. Retain failures,
including repeated rejected arguments, and inspect actual child inference and
both delivered values. Provider availability is not a test pass.

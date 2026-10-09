//! Atomic prompt-facing CSL snapshots.
use super::io::{csl_log_path_for, sync_parent_dir, write_bytes_atomic};
fn read_max_seq_from_log(path: &std::path::Path) -> u64 {
    let Ok(file) = std::fs::File::open(path) else {
        return 0;
    };
    use std::io::BufRead;
    let mut max_seq = 0u64;
    let measure_history_work = astra_core::history_work::instrumentation_enabled();
    let mut measured_bytes = 0_u64;
    let mut measured_rows = 0_u64;
    let mut deserialized_bytes = 0_u64;
    let mut deserialized_rows = 0_u64;
    for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
        if measure_history_work {
            measured_bytes =
                measured_bytes.saturating_add(line.len().try_into().unwrap_or(u64::MAX));
            measured_rows = measured_rows.saturating_add(1);
        }
        if line.trim().is_empty() {
            continue;
        }
        if measure_history_work {
            deserialized_bytes =
                deserialized_bytes.saturating_add(line.len().try_into().unwrap_or(u64::MAX));
            deserialized_rows = deserialized_rows.saturating_add(1);
        }
        if let Ok(entry) =
            serde_json::from_str::<astra_turn_core::conversation_log::CslEntry>(&line)
        {
            max_seq = max_seq.max(entry.seq());
        }
    }
    if measure_history_work {
        astra_core::history_work::record_operation(
            astra_core::history_work::HistoryWorkSite::CliRecoveryCslLogRead,
            measured_bytes,
            measured_rows,
            0,
        );
        if deserialized_rows > 0 {
            astra_core::history_work::record_operation(
                astra_core::history_work::HistoryWorkSite::CliRecoveryCslLogDeserialization,
                deserialized_bytes,
                deserialized_rows,
                0,
            );
        }
    }
    max_seq
}

pub(crate) fn write_full_csl_snapshot_atomic(
    sid: &str,
    turn: u32,
    messages: &[serde_json::Value],
    session_state: &astra_turn_core::conversation_log::SessionStateCompact,
) -> Result<(), String> {
    let path = csl_log_path_for(sid);
    if messages.is_empty() && turn == 0 {
        match std::fs::remove_file(&path) {
            Ok(()) => return sync_parent_dir(&path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(format!("remove stale CSL snapshot: {error}")),
        }
    }

    // The recovery snapshot replaces the file in its entirety, but the new
    // snapshot's seq must still strictly dominate anything previously written
    // so any out-of-band reader that observed older seqs (e.g. a cached
    // `last_seq` in a still-running CSL manager) does not regress and reject
    // subsequent appends as out-of-order.
    let next_seq = read_max_seq_from_log(&path).saturating_add(1);

    let snapshot = astra_turn_core::conversation_log::CslEntry::Snapshot {
        seq: next_seq,
        turn,
        messages: crate::cli::history_work::clone_json_history(
            astra_core::history_work::HistoryWorkSite::CliRecoveryCslSnapshotClone,
            messages,
        ),
        session_state: session_state.clone(),
    };
    let mut encoded =
        serde_json::to_string(&snapshot).map_err(|e| format!("serialize CSL snapshot: {e}"))?;
    crate::cli::history_work::record_existing_buffer(
        astra_core::history_work::HistoryWorkSite::CliRecoveryCslSnapshotSerialization,
        encoded.as_bytes(),
        messages.len(),
    );
    encoded.push('\n');
    write_bytes_atomic(&path, encoded.as_bytes(), "replace CSL snapshot")
}

//! Resolve Work workspaces through the lifecycle's selected local provider.
use std::path::PathBuf;

use astra_core::SharedPool;
use astra_runtime_env::WorkspaceSource;
use astra_services::{
    DatabaseWorkspaceRecordStore, WorkspaceRecordStore,
    runs::RunLifecycleService,
    work::{DatabaseWorkRepository, WorkBranchId, WorkId, WorkOwnerId, WorkRepository},
};

pub(crate) enum WorkspaceResolutionError {
    NotThisExecutor,
    UnverifiedUnavailable(String),
}

pub(crate) async fn resolve_workspace(
    pool: &SharedPool,
    lifecycle: &dyn RunLifecycleService,
    owner: &WorkOwnerId,
    work: &WorkId,
    branch: &WorkBranchId,
) -> Result<PathBuf, WorkspaceResolutionError> {
    let Some(executor_id) = lifecycle.workspace_executor_id() else {
        return Err(WorkspaceResolutionError::NotThisExecutor);
    };
    let binding = DatabaseWorkRepository::new(pool.clone())
        .load_branch_runtime_binding(owner, work, branch)
        .await
        .map_err(|error| WorkspaceResolutionError::UnverifiedUnavailable(error.to_string()))?;
    let entry = DatabaseWorkspaceRecordStore::new(pool.clone())
        .load_workspace_record(owner.as_str(), binding.session_id.as_str())
        .await
        .map_err(|error| WorkspaceResolutionError::UnverifiedUnavailable(error.to_string()))?
        .filter(|entry| entry.session_id.as_deref() == Some(binding.session_id.as_str()))
        .ok_or(WorkspaceResolutionError::NotThisExecutor)?;
    let WorkspaceSource::ServerSandbox {
        executor_id: owner_executor,
        session_id,
    } = &entry.record.source
    else {
        return Err(WorkspaceResolutionError::NotThisExecutor);
    };
    if owner_executor != executor_id || session_id != binding.session_id.as_str() {
        return Err(WorkspaceResolutionError::NotThisExecutor);
    }
    lifecycle
        .resolve_server_workspace(&entry.record)
        .map_err(|error| WorkspaceResolutionError::UnverifiedUnavailable(error.to_string()))
}

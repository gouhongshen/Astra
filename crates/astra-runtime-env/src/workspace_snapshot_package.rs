//! Verify portable workspace manifests and their content-addressed blobs.
//! Local capture and installation are not implemented here; this boundary is
//! shared by sealed Artifact publication and verified package reads.
use crate::{WorkspaceSnapshotChangeV1, WorkspaceSnapshotEntryKindV1, WorkspaceSnapshotManifestV1};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use thiserror::Error;
/// A verified package of manifest metadata and content-addressed file blobs.
/// The manifest is not considered complete until [`Self::verify`] succeeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSnapshotPackage {
    pub manifest: WorkspaceSnapshotManifestV1,
    pub blobs: BTreeMap<String, Vec<u8>>,
}

impl WorkspaceSnapshotPackage {
    pub fn verify(&self) -> Result<(), WorkspaceSnapshotPackageError> {
        Self::verify_blobs(
            &self.manifest,
            self.blobs
                .iter()
                .map(|(key, bytes)| (key.as_str(), bytes.as_slice()))
                .collect(),
        )
    }

    /// Verify borrowed content without copying the sealed artifact's payload.
    pub fn verify_blobs(
        manifest: &WorkspaceSnapshotManifestV1,
        blobs: BTreeMap<&str, &[u8]>,
    ) -> Result<(), WorkspaceSnapshotPackageError> {
        manifest
            .validate()
            .map_err(WorkspaceSnapshotPackageError::InvalidManifest)?;
        let mut digests = BTreeMap::new();
        for entry in &manifest.entries {
            if is_excluded_path(&entry.path) {
                return Err(WorkspaceSnapshotPackageError::UnsafePath(
                    entry.path.clone(),
                ));
            }
            if entry.change == WorkspaceSnapshotChangeV1::Deleted
                || entry.kind != WorkspaceSnapshotEntryKindV1::File
            {
                continue;
            }
            let blob_ref = entry
                .blob_ref
                .as_deref()
                .ok_or_else(|| WorkspaceSnapshotPackageError::MissingBlob(entry.path.clone()))?;
            let bytes = blobs
                .get(blob_ref)
                .ok_or_else(|| WorkspaceSnapshotPackageError::MissingBlob(entry.path.clone()))?;
            let digest = digests
                .entry(blob_ref)
                .or_insert_with(|| content_digest(bytes));
            if entry.digest.as_deref() != Some(digest.as_str()) {
                return Err(WorkspaceSnapshotPackageError::BlobDigestMismatch {
                    path: entry.path.clone(),
                });
            }
            if entry.size != bytes.len() as u64 {
                return Err(WorkspaceSnapshotPackageError::BlobSizeMismatch {
                    path: entry.path.clone(),
                });
            }
        }
        let referenced = manifest
            .entries
            .iter()
            .filter_map(|entry| entry.blob_ref.as_deref())
            .collect::<BTreeSet<_>>();
        if let Some(unexpected) = blobs
            .keys()
            .find(|blob_ref| !referenced.contains(**blob_ref))
        {
            return Err(WorkspaceSnapshotPackageError::UnexpectedBlob(
                (*unexpected).to_string(),
            ));
        }
        validate_package_layout(manifest)?;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum WorkspaceSnapshotPackageError {
    #[error("workspace path is unsafe: {0}")]
    UnsafePath(String),
    #[error("snapshot manifest is invalid: {0}")]
    InvalidManifest(#[source] crate::WorkspaceSnapshotValidationError),
    #[error("snapshot is missing the blob for {0}")]
    MissingBlob(String),
    #[error("snapshot blob digest does not match {path}")]
    BlobDigestMismatch { path: String },
    #[error("snapshot blob size does not match {path}")]
    BlobSizeMismatch { path: String },
    #[error("snapshot contains an unreferenced content blob: {0}")]
    UnexpectedBlob(String),
}
fn is_excluded_path(path: &str) -> bool {
    path == ".git" || path.starts_with(".git/") || path == ".astra" || path.starts_with(".astra/")
}
fn validate_package_layout(
    manifest: &WorkspaceSnapshotManifestV1,
) -> Result<(), WorkspaceSnapshotPackageError> {
    let mut kinds = BTreeMap::<String, WorkspaceSnapshotEntryKindV1>::new();
    let mut symlinks = BTreeMap::<String, String>::new();
    for entry in &manifest.entries {
        if entry.change == WorkspaceSnapshotChangeV1::Deleted {
            continue;
        }
        let mut ancestor = PathBuf::new();
        let components = entry.path.split('/').collect::<Vec<_>>();
        for component in &components[..components.len().saturating_sub(1)] {
            ancestor.push(component);
            if matches!(
                kinds.get(
                    &ancestor
                        .to_string_lossy()
                        .replace(std::path::MAIN_SEPARATOR, "/")
                ),
                Some(WorkspaceSnapshotEntryKindV1::File | WorkspaceSnapshotEntryKindV1::Symlink)
            ) {
                return Err(WorkspaceSnapshotPackageError::UnsafePath(
                    entry.path.clone(),
                ));
            }
        }
        kinds.insert(entry.path.clone(), entry.kind);
        if entry.kind == WorkspaceSnapshotEntryKindV1::Symlink
            && let Some(target) = &entry.symlink_target
        {
            symlinks.insert(entry.path.clone(), target.clone());
        }
    }
    let mut resolution_cache = BTreeMap::new();
    for path in symlinks.keys() {
        validate_symlink_resolution_with_cache(path, &symlinks, &mut resolution_cache)?;
    }
    Ok(())
}

#[cfg(test)]
fn validate_symlink_resolution(
    path: &str,
    symlinks: &BTreeMap<String, String>,
) -> Result<(), WorkspaceSnapshotPackageError> {
    let mut cache = BTreeMap::new();
    validate_symlink_resolution_with_cache(path, symlinks, &mut cache)
}

fn validate_symlink_resolution_with_cache(
    path: &str,
    symlinks: &BTreeMap<String, String>,
    cache: &mut BTreeMap<String, ResolvedSymlink>,
) -> Result<(), WorkspaceSnapshotPackageError> {
    let mut components = path.split('/').map(str::to_owned).collect::<Vec<_>>();
    let _ = components.pop();
    let Some(target) = symlinks.get(path) else {
        return Err(WorkspaceSnapshotPackageError::UnsafePath(path.to_string()));
    };
    components.extend(target.split('/').map(str::to_owned));
    let mut active = BTreeSet::new();
    let _ =
        resolve_symlink_components(&components, Vec::new(), symlinks, &mut active, cache, path)?;
    Ok(())
}

/// Resolve one component sequence while keeping the active expansion stack.
/// A symlink is removed from the stack after its own target has been resolved,
/// so a safe path may visit the same alias more than once while an actual
/// cycle is still rejected. Successful alias expansions are memoized by their
/// canonical path. Cache entries retain only their direct alias dependencies;
/// an iterative graph walk checks whether using one would intersect the
/// current active stack, without recursively expanding the alias or copying a
/// transitive dependency set.
#[derive(Clone)]
struct ResolvedSymlink {
    components: Vec<String>,
    dependencies: BTreeSet<String>,
}

fn resolve_symlink_components(
    components: &[String],
    initial: Vec<String>,
    symlinks: &BTreeMap<String, String>,
    active: &mut BTreeSet<String>,
    cache: &mut BTreeMap<String, ResolvedSymlink>,
    original: &str,
) -> Result<ResolvedSymlink, WorkspaceSnapshotPackageError> {
    struct Frame {
        components: Vec<String>,
        index: usize,
        active_symlink: Option<String>,
        direct_dependencies: BTreeSet<String>,
    }

    let mut resolved = initial;
    let mut frames = vec![Frame {
        components: components.to_vec(),
        index: 0,
        active_symlink: None,
        direct_dependencies: BTreeSet::new(),
    }];
    while let Some(frame) = frames.last_mut() {
        if frame.index >= frame.components.len() {
            let frame = frames.pop().expect("frame exists");
            if let Some(symlink) = frame.active_symlink {
                active.remove(&symlink);
                cache.insert(
                    symlink,
                    ResolvedSymlink {
                        components: resolved.clone(),
                        dependencies: frame.direct_dependencies,
                    },
                );
            } else {
                return Ok(ResolvedSymlink {
                    components: resolved,
                    dependencies: frame.direct_dependencies,
                });
            }
            continue;
        }
        let component = frame.components[frame.index].clone();
        frame.index += 1;
        match component.as_str() {
            "" | "." => {}
            ".." => {
                if resolved.pop().is_none() {
                    return Err(WorkspaceSnapshotPackageError::UnsafePath(
                        original.to_string(),
                    ));
                }
            }
            value => {
                resolved.push(value.to_string());
                let candidate = resolved.join("/");
                let Some(next_target) = symlinks.get(&candidate).cloned() else {
                    continue;
                };
                let _ = resolved.pop();
                if active.contains(&candidate) {
                    return Err(WorkspaceSnapshotPackageError::UnsafePath(
                        original.to_string(),
                    ));
                }
                if let Some(cached) = cache.get(&candidate)
                    && !cached_expansion_touches_active(&candidate, active, cache)
                {
                    frame.direct_dependencies.insert(candidate);
                    resolved = cached.components.clone();
                    continue;
                }
                frame.direct_dependencies.insert(candidate.clone());
                active.insert(candidate.clone());
                frames.push(Frame {
                    components: next_target.split('/').map(str::to_owned).collect(),
                    index: 0,
                    active_symlink: Some(candidate),
                    direct_dependencies: BTreeSet::new(),
                });
            }
        }
    }
    Err(WorkspaceSnapshotPackageError::UnsafePath(
        original.to_string(),
    ))
}

fn cached_expansion_touches_active(
    root: &str,
    active: &BTreeSet<String>,
    cache: &BTreeMap<String, ResolvedSymlink>,
) -> bool {
    // The common package-level validation path starts each root with no
    // active expansion. There is no cycle to detect in that state, so avoid
    // walking the cached dependency graph for every alias in a long chain.
    if active.is_empty() {
        return false;
    }
    let mut pending = vec![root.to_string()];
    let mut visited = BTreeSet::new();
    while let Some(candidate) = pending.pop() {
        if !visited.insert(candidate.clone()) {
            continue;
        }
        if active.contains(&candidate) {
            return true;
        }
        if let Some(entry) = cache.get(&candidate) {
            pending.extend(entry.dependencies.iter().cloned());
        }
    }
    false
}

fn content_digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        WORKSPACE_SNAPSHOT_MANIFEST_SCHEMA_VERSION, WorkspaceSnapshotCaptureV1,
        WorkspaceSnapshotContentV1, WorkspaceSnapshotEntryV1, WorkspaceSnapshotRepositoryV1,
    };

    fn shared_blob_package() -> WorkspaceSnapshotPackage {
        let bytes = b"before\n".to_vec();
        let digest = content_digest(&bytes);
        let entries = ["copy.txt", "src/main.txt"]
            .into_iter()
            .map(|path| WorkspaceSnapshotEntryV1 {
                path: path.into(),
                kind: WorkspaceSnapshotEntryKindV1::File,
                change: WorkspaceSnapshotChangeV1::Added,
                mode: 0o644,
                size: bytes.len() as u64,
                digest: Some(digest.clone()),
                blob_ref: Some(digest.clone()),
                symlink_target: None,
                renamed_from: None,
            })
            .collect();
        let mut package = WorkspaceSnapshotPackage {
            manifest: WorkspaceSnapshotManifestV1 {
                schema_version: WORKSPACE_SNAPSHOT_MANIFEST_SCHEMA_VERSION,
                snapshot_id: "snapshot-fixture".into(),
                logical_workspace_id: "workspace-fixture".into(),
                repository: WorkspaceSnapshotRepositoryV1 {
                    repository_id: "repository-fixture".into(),
                    base_commit: None,
                    base_tree: None,
                    submodules: Vec::new(),
                },
                capture: WorkspaceSnapshotCaptureV1 {
                    fingerprint_before: digest.clone(),
                    fingerprint_after: digest.clone(),
                    captured_at: "2026-09-17T00:00:00Z".into(),
                    consistent: true,
                },
                entries,
                exclusions: Vec::new(),
                data_sources: Vec::new(),
                content: WorkspaceSnapshotContentV1 {
                    content_root: String::new(),
                    total_bytes: (bytes.len() * 2) as u64,
                    blob_count: 1,
                    pack_ref: None,
                },
            },
            blobs: BTreeMap::from([(digest, bytes)]),
        };
        package.manifest.content.content_root = package.manifest.computed_content_root().unwrap();
        package.verify().expect("valid shared-blob package");
        package
    }

    #[test]
    fn shared_blob_references_are_checked_for_every_entry() {
        let package = shared_blob_package();
        let mut tampered_digest = package.clone();
        let second = tampered_digest
            .manifest
            .entries
            .iter_mut()
            .find(|entry| entry.path == "src/main.txt")
            .expect("second file");
        second.digest = Some(content_digest(b"tampered"));
        tampered_digest.manifest.content.content_root =
            tampered_digest.manifest.computed_content_root().unwrap();
        assert!(matches!(
            tampered_digest.verify(),
            Err(WorkspaceSnapshotPackageError::BlobDigestMismatch { path }) if path == "src/main.txt"
        ));
        let mut tampered_size = package;
        let second = tampered_size
            .manifest
            .entries
            .iter_mut()
            .find(|entry| entry.path == "src/main.txt")
            .expect("second file");
        second.size += 1;
        tampered_size.manifest.content.total_bytes += 1;
        tampered_size.manifest.content.content_root =
            tampered_size.manifest.computed_content_root().unwrap();
        assert!(matches!(
            tampered_size.verify(),
            Err(WorkspaceSnapshotPackageError::BlobSizeMismatch { path }) if path == "src/main.txt"
        ));
    }

    #[test]
    fn package_rejects_symlink_ancestors_in_manifest() {
        let entries = vec![
            WorkspaceSnapshotEntryV1 {
                path: "a".to_string(),
                kind: WorkspaceSnapshotEntryKindV1::Symlink,
                change: WorkspaceSnapshotChangeV1::Added,
                mode: 0o777,
                size: 0,
                digest: None,
                blob_ref: None,
                symlink_target: Some(".".to_string()),
                renamed_from: None,
            },
            WorkspaceSnapshotEntryV1 {
                path: "a/b".to_string(),
                kind: WorkspaceSnapshotEntryKindV1::File,
                change: WorkspaceSnapshotChangeV1::Added,
                mode: 0o644,
                size: 1,
                digest: Some(content_digest(b"x")),
                blob_ref: Some("blob-x".to_string()),
                symlink_target: None,
                renamed_from: None,
            },
        ];
        let mut package = WorkspaceSnapshotPackage {
            manifest: WorkspaceSnapshotManifestV1 {
                schema_version: WORKSPACE_SNAPSHOT_MANIFEST_SCHEMA_VERSION,
                snapshot_id: "snapshot-layout".to_string(),
                logical_workspace_id: "workspace-layout".to_string(),
                repository: WorkspaceSnapshotRepositoryV1 {
                    repository_id: "repo-layout".to_string(),
                    base_commit: None,
                    base_tree: None,
                    submodules: Vec::new(),
                },
                capture: WorkspaceSnapshotCaptureV1 {
                    fingerprint_before: content_digest(b"capture"),
                    fingerprint_after: content_digest(b"capture"),
                    captured_at: "2026-09-17T00:00:00Z".to_string(),
                    consistent: true,
                },
                entries,
                exclusions: Vec::new(),
                data_sources: Vec::new(),
                content: WorkspaceSnapshotContentV1 {
                    content_root: String::new(),
                    total_bytes: 1,
                    blob_count: 1,
                    pack_ref: None,
                },
            },
            blobs: BTreeMap::from([("blob-x".to_string(), b"x".to_vec())]),
        };
        package.manifest.content.content_root = package.manifest.computed_content_root().unwrap();
        assert!(matches!(
            package.verify(),
            Err(WorkspaceSnapshotPackageError::UnsafePath(path)) if path == "a/b"
        ));
    }

    #[test]
    fn package_verify_rejects_unreferenced_blob() {
        let mut package = shared_blob_package();
        package.blobs.insert(
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            b"extra".to_vec(),
        );
        assert!(matches!(
            package.verify(),
            Err(WorkspaceSnapshotPackageError::UnexpectedBlob(_))
        ));
    }

    #[test]
    fn symlink_resolution_allows_reusing_a_safe_alias() {
        let symlinks = BTreeMap::from([
            ("a".to_string(), ".".to_string()),
            ("dir/link".to_string(), "../a/a".to_string()),
        ]);
        assert!(validate_symlink_resolution("dir/link", &symlinks).is_ok());
    }

    #[test]
    fn symlink_resolution_rejects_a_chained_escape() {
        let symlinks = BTreeMap::from([
            ("a".to_string(), ".".to_string()),
            ("dir/link".to_string(), "../a/..".to_string()),
        ]);
        assert!(matches!(
            validate_symlink_resolution("dir/link", &symlinks),
            Err(WorkspaceSnapshotPackageError::UnsafePath(path)) if path == "dir/link"
        ));
    }

    #[test]
    fn symlink_resolution_memoizes_a_safe_diamond() {
        // Each alias references the next alias twice. Without memoization the
        // validator expands this DAG exponentially even though it has only a
        // linear number of distinct symlinks.
        let mut symlinks = BTreeMap::new();
        for index in 0..30 {
            symlinks.insert(
                format!("a{index}"),
                format!("a{next}/a{next}", next = index + 1),
            );
        }
        symlinks.insert("a30".to_string(), ".".to_string());
        assert!(validate_symlink_resolution("a0", &symlinks).is_ok());
    }

    #[test]
    fn symlink_resolution_rejects_a_cycle() {
        let symlinks = BTreeMap::from([
            ("a0".to_string(), "a1".to_string()),
            ("a1".to_string(), "a0".to_string()),
        ]);
        assert!(matches!(
            validate_symlink_resolution("a0", &symlinks),
            Err(WorkspaceSnapshotPackageError::UnsafePath(path)) if path == "a0"
        ));
    }

    #[test]
    fn symlink_resolution_handles_a_deep_chain_without_recursion() {
        let depth = 6_000;
        let mut symlinks = BTreeMap::new();
        for index in 0..depth {
            symlinks.insert(format!("a{index}"), format!("a{}", index + 1));
        }
        assert!(validate_symlink_resolution("a0", &symlinks).is_ok());
    }

    #[test]
    fn full_layout_validation_reuses_one_cache_for_a_deep_chain() {
        let depth = 6_000;
        let mut entries = Vec::with_capacity(depth + 1);
        for index in 0..depth {
            entries.push(WorkspaceSnapshotEntryV1 {
                path: format!("a{index}"),
                kind: WorkspaceSnapshotEntryKindV1::Symlink,
                change: WorkspaceSnapshotChangeV1::Added,
                mode: 0o777,
                size: 0,
                digest: None,
                blob_ref: None,
                symlink_target: Some(format!("a{}", index + 1)),
                renamed_from: None,
            });
        }
        entries.push(WorkspaceSnapshotEntryV1 {
            path: format!("a{depth}"),
            kind: WorkspaceSnapshotEntryKindV1::Directory,
            change: WorkspaceSnapshotChangeV1::Added,
            mode: 0o755,
            size: 0,
            digest: None,
            blob_ref: None,
            symlink_target: None,
            renamed_from: None,
        });
        entries.sort_by(|left, right| left.path.cmp(&right.path));
        let mut package = WorkspaceSnapshotPackage {
            manifest: WorkspaceSnapshotManifestV1 {
                schema_version: WORKSPACE_SNAPSHOT_MANIFEST_SCHEMA_VERSION,
                snapshot_id: "snapshot-deep-chain".to_string(),
                logical_workspace_id: "workspace-deep-chain".to_string(),
                repository: crate::WorkspaceSnapshotRepositoryV1 {
                    repository_id: "repo-deep-chain".to_string(),
                    base_commit: None,
                    base_tree: None,
                    submodules: Vec::new(),
                },
                capture: crate::WorkspaceSnapshotCaptureV1 {
                    fingerprint_before: "fingerprint".to_string(),
                    fingerprint_after: "fingerprint".to_string(),
                    captured_at: "2026-09-17T00:00:00Z".to_string(),
                    consistent: true,
                },
                entries,
                exclusions: Vec::new(),
                data_sources: Vec::new(),
                content: crate::WorkspaceSnapshotContentV1 {
                    content_root: format!("sha256:{}", "0".repeat(64)),
                    total_bytes: 0,
                    blob_count: 0,
                    pack_ref: None,
                },
            },
            blobs: BTreeMap::new(),
        };
        package.manifest.content.content_root = package.manifest.computed_content_root().unwrap();
        package
            .verify()
            .expect("validate deep chain through the production entrypoint");
    }

    #[test]
    fn package_rejects_missing_content_and_private_paths() {
        let mut missing = shared_blob_package();
        missing.blobs.clear();
        assert!(matches!(
            missing.verify(),
            Err(WorkspaceSnapshotPackageError::MissingBlob(_))
        ));
        let mut private = shared_blob_package();
        private.manifest.entries[0].path = ".astra/private".into();
        private.manifest.content.content_root = private.manifest.computed_content_root().unwrap();
        assert!(private.verify().is_err());
    }
}

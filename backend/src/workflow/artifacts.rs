use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::domain::{ArtifactMetadata, WorkflowError, WorkflowResult};
use super::journal::{Journal, now_ms};

#[derive(Debug, Clone)]
pub struct ArtifactStore {
    root: PathBuf,
}

impl ArtifactStore {
    pub fn open(root: impl AsRef<Path>) -> WorkflowResult<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    // Artifact identity, storage metadata and bytes are deliberately explicit
    // at this single transaction boundary.
    #[allow(clippy::too_many_arguments)]
    pub fn put(
        &self,
        journal: &Journal,
        run_id: &str,
        node_id: Option<&str>,
        name: impl Into<String>,
        mime_type: impl Into<String>,
        bytes: &[u8],
        metadata: Value,
    ) -> WorkflowResult<ArtifactMetadata> {
        let id = uuid::Uuid::new_v4().to_string();
        let sha256 = format!("{:x}", Sha256::digest(bytes));
        let storage_key = format!("{}/{id}.blob", &sha256[..2]);
        let path = self.resolve_key(&storage_key)?;
        let parent = path.parent().ok_or_else(|| {
            WorkflowError::Storage("artifact storage path has no parent".to_string())
        })?;
        fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(".{id}.tmp"));
        let write_result = (|| -> WorkflowResult<()> {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            fs::rename(&temporary, &path)?;
            if let Ok(directory) = OpenOptions::new().read(true).open(parent) {
                let _ = directory.sync_all();
            }
            Ok(())
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        write_result?;

        let artifact = ArtifactMetadata {
            id,
            run_id: run_id.to_string(),
            node_id: node_id.map(String::from),
            name: name.into(),
            mime_type: mime_type.into(),
            size: bytes.len() as u64,
            storage_key,
            sha256,
            metadata,
            created_at_ms: now_ms(),
        };
        if let Err(error) = journal.insert_artifact(&artifact) {
            // The blob has no durable metadata and is therefore unreachable.
            // Best-effort cleanup is safe because storage keys are immutable and unique.
            let _ = fs::remove_file(&path);
            return Err(error);
        }
        Ok(artifact)
    }

    pub fn read(&self, artifact: &ArtifactMetadata) -> WorkflowResult<Vec<u8>> {
        let path = self.resolve_key(&artifact.storage_key)?;
        let mut bytes = Vec::new();
        OpenOptions::new()
            .read(true)
            .open(path)?
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 != artifact.size {
            return Err(WorkflowError::Storage(format!(
                "artifact {} size does not match metadata",
                artifact.id
            )));
        }
        let actual_hash = format!("{:x}", Sha256::digest(&bytes));
        if actual_hash != artifact.sha256 {
            return Err(WorkflowError::Storage(format!(
                "artifact {} checksum does not match metadata",
                artifact.id
            )));
        }
        Ok(bytes)
    }

    fn resolve_key(&self, key: &str) -> WorkflowResult<PathBuf> {
        let relative = Path::new(key);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(WorkflowError::InvalidRequest(
                "artifact storage key is not a safe relative path".to_string(),
            ));
        }
        Ok(self.root.join(relative))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsafe_storage_keys_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(temp.path()).unwrap();
        assert!(store.resolve_key("../outside").is_err());
        assert!(store.resolve_key("/absolute").is_err());
    }
}

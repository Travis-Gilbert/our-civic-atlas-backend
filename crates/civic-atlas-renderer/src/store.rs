//! Asset storage.
//!
//! The renderer writes real bytes to storage and mints public URIs for
//! them. The store is content-addressed: the key embeds the sha256 of the
//! payload (matching the `sha256-<hex>` convention the Python Scene Foundry
//! lane already uses), so repeated renders of an identical spec are
//! idempotent writes and asset URLs never serve stale bytes.
//!
//! `LocalDirAssetStore` covers development (point the root at the frontend
//! `public/` tree for same-origin serving) and production (a service volume
//! served by the civic-atlas-server `/assets/scene-foundry/` route). The
//! GPU refinement lane writes its own assets to S3 from Python
//! (`civic_atlas_ingest.scene_foundry`) and reports URIs back through the
//! render-jobs table, so the Rust side never needs S3 credentials.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use async_trait::async_trait;
use sha2::{Digest, Sha256};

/// Compute the canonical content hash for asset bytes.
pub fn content_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("sha256-{digest:x}")
}

/// A stored asset: where it landed and how it is addressed.
#[derive(Debug, Clone)]
pub struct StoredAsset {
    /// Public URI the frontend can fetch (ends with the real filename, so
    /// extension-based renderer dispatch works).
    pub uri: String,
    /// Canonical `sha256-<hex>` content hash of the stored bytes.
    pub content_hash: String,
    /// Store-relative key.
    pub key: String,
}

#[async_trait]
pub trait AssetStore: Send + Sync {
    /// Write `bytes` under a content-addressed key derived from
    /// `spec_id`/`spec_version`/`file_stem`/`extension` and return the
    /// stored asset record.
    async fn put(
        &self,
        spec_id: &str,
        spec_version: u32,
        file_stem: &str,
        extension: &str,
        bytes: &[u8],
    ) -> Result<StoredAsset>;
}

fn sanitize_path_component(raw: &str) -> String {
    let mut sanitized: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        sanitized.push_str("unknown");
    }
    sanitized
}

/// Filesystem-backed store with a configurable public base URL.
#[derive(Debug, Clone)]
pub struct LocalDirAssetStore {
    root: PathBuf,
    public_base_url: String,
}

impl LocalDirAssetStore {
    pub fn new(root: impl Into<PathBuf>, public_base_url: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            public_base_url: public_base_url.into(),
        }
    }

    /// Build from the conventional environment variables.
    ///
    /// - `SCENE_FOUNDRY_ASSET_DIR`: filesystem root for written assets
    ///   (default `data/scene-foundry-assets`).
    /// - `SCENE_FOUNDRY_PUBLIC_BASE_URL`: URL prefix minted into asset URIs
    ///   (default `/assets/scene-foundry`, the civic-atlas-server route).
    pub fn from_env() -> Self {
        let root = std::env::var("SCENE_FOUNDRY_ASSET_DIR")
            .unwrap_or_else(|_| "data/scene-foundry-assets".to_string());
        let base = std::env::var("SCENE_FOUNDRY_PUBLIC_BASE_URL")
            .unwrap_or_else(|_| "/assets/scene-foundry".to_string());
        Self::new(root, base)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[async_trait]
impl AssetStore for LocalDirAssetStore {
    async fn put(
        &self,
        spec_id: &str,
        spec_version: u32,
        file_stem: &str,
        extension: &str,
        bytes: &[u8],
    ) -> Result<StoredAsset> {
        let hash = content_hash(bytes);
        let key = format!(
            "{}/v{}/{}.{}.{}",
            sanitize_path_component(spec_id),
            spec_version,
            sanitize_path_component(file_stem),
            hash,
            sanitize_path_component(extension),
        );
        let path = self.root.join(&key);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("creating asset dir {}", parent.display()))?;
        }
        // Content-addressed: an existing file with this key already holds
        // these exact bytes.
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            tracing::debug!(key = %key, "asset already stored (content-addressed hit)");
        } else {
            let tmp = path.with_extension("tmp-write");
            tokio::fs::write(&tmp, bytes)
                .await
                .with_context(|| format!("writing asset {}", path.display()))?;
            tokio::fs::rename(&tmp, &path)
                .await
                .with_context(|| format!("publishing asset {}", path.display()))?;
        }
        let uri = format!("{}/{}", self.public_base_url.trim_end_matches('/'), key);
        Ok(StoredAsset {
            uri,
            content_hash: hash,
            key,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn put_writes_content_addressed_file_and_mints_uri() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalDirAssetStore::new(dir.path(), "/assets/scene-foundry");
        let stored = store
            .put("recon-whaley-1900", 1, "massing", "glb", b"glTF-test-bytes")
            .await
            .unwrap();
        assert!(stored.content_hash.starts_with("sha256-"));
        assert!(stored.uri.starts_with("/assets/scene-foundry/recon-whaley-1900/v1/massing.sha256-"));
        assert!(stored.uri.ends_with(".glb"));
        let on_disk = std::fs::read(dir.path().join(&stored.key)).unwrap();
        assert_eq!(on_disk, b"glTF-test-bytes");

        // Idempotent re-put.
        let again = store
            .put("recon-whaley-1900", 1, "massing", "glb", b"glTF-test-bytes")
            .await
            .unwrap();
        assert_eq!(again.content_hash, stored.content_hash);
        assert_eq!(again.key, stored.key);
    }

    #[test]
    fn spec_ids_with_colons_are_sanitized() {
        assert_eq!(
            sanitize_path_component("spec:carriage-town:2"),
            "spec_carriage-town_2"
        );
    }
}

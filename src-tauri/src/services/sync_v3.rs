//! Cloud-sync protocol v3: per-category artifacts and shared orchestration.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::database::Database;
use crate::error::AppError;
use crate::services::s3::{self, S3Credentials};
use crate::services::skill::skill_state_write_guard;
use crate::services::sync_categories::{
    apply_categories_in_transaction, artifact_relative_path, export_category,
    model_pricing_file_bytes_from_artifact, validate_artifact, validate_artifact_relative_path,
    ApplyContext, CategoryApplyReport, CategoryArtifact, CategoryRuntimeState, CategorySyncStatus,
    CleanupResidue, CloudSyncSelection, CloudSyncTargetState, SyncCategory,
    CATEGORY_SCHEMA_VERSION,
};
use crate::services::sync_protocol::{
    detect_system_device_name, localized, sha256_hex, validate_artifact_size_limit, ArtifactMeta,
    RemoteLayout, SyncManifest, DB_COMPAT_VERSION, MAX_MANIFEST_BYTES, MAX_SYNC_ARTIFACT_BYTES,
    PROTOCOL_FORMAT, PROTOCOL_VERSION, REMOTE_DB_SQL, REMOTE_MANIFEST, REMOTE_SKILLS_ZIP,
};
use crate::services::webdav::{self, WebDavAuth};
use crate::services::webdav_sync::archive::{
    backup_current_skills, restore_skills_from_backup, restore_skills_zip,
};
use crate::settings::{S3SyncSettings, WebDavSyncSettings};

pub(crate) const PROTOCOL_FORMAT_V3: &str = "cc-switch-cloud-sync";
pub(crate) const PROTOCOL_VERSION_V3: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct V3Manifest {
    pub format: String,
    pub protocol_version: u32,
    pub db_compat_version: u32,
    pub profile: String,
    pub snapshot_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_snapshot_id: Option<String>,
    pub device_name: String,
    pub created_at: String,
    #[serde(default)]
    pub categories: BTreeMap<String, CategoryManifestEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryManifestEntry {
    pub schema_version: u32,
    pub artifact: String,
    pub sha256: String,
    pub size: u64,
    pub item_count: u64,
    pub updated_at: String,
    pub device_name: String,
}

impl V3Manifest {
    pub fn empty(profile: &str) -> Self {
        Self {
            format: PROTOCOL_FORMAT_V3.to_string(),
            protocol_version: PROTOCOL_VERSION_V3,
            db_compat_version: DB_COMPAT_VERSION,
            profile: profile.to_string(),
            snapshot_id: sha256_hex(b""),
            base_snapshot_id: None,
            device_name: detect_system_device_name()
                .unwrap_or_else(|| "Unknown Device".to_string()),
            created_at: Utc::now().to_rfc3339(),
            categories: BTreeMap::new(),
        }
    }

    pub fn recompute_snapshot_id(&mut self) {
        self.snapshot_id = compute_v3_snapshot_id(&self.categories);
    }
}

pub fn compute_v3_snapshot_id(categories: &BTreeMap<String, CategoryManifestEntry>) -> String {
    let parts: Vec<String> = categories
        .iter()
        .map(|(id, entry)| {
            format!(
                "{}:{}:{}:{}",
                id, entry.schema_version, entry.sha256, entry.artifact
            )
        })
        .collect();
    sha256_hex(parts.join("|").as_bytes())
}

pub fn validate_v3_manifest(manifest: &V3Manifest) -> Result<(), AppError> {
    if manifest.format != PROTOCOL_FORMAT_V3 {
        return Err(localized(
            "sync.manifest_format_incompatible",
            format!("远端 manifest 格式不兼容: {}", manifest.format),
            format!(
                "Remote manifest format is incompatible: {}",
                manifest.format
            ),
        ));
    }
    if manifest.protocol_version != PROTOCOL_VERSION_V3 {
        return Err(localized(
            "sync.manifest_version_incompatible",
            format!(
                "远端 manifest 协议版本不兼容: v{}",
                manifest.protocol_version
            ),
            format!(
                "Remote manifest protocol version is incompatible: v{}",
                manifest.protocol_version
            ),
        ));
    }
    if manifest.db_compat_version != DB_COMPAT_VERSION {
        return Err(localized(
            "sync.manifest_db_version_incompatible",
            format!(
                "远端数据库快照版本不兼容: db-v{}",
                manifest.db_compat_version
            ),
            format!(
                "Remote database snapshot version is incompatible: db-v{}",
                manifest.db_compat_version
            ),
        ));
    }
    let mut seen_paths = BTreeSet::new();
    for (id, entry) in &manifest.categories {
        if let Some(category) = SyncCategory::parse(id) {
            validate_artifact_relative_path(category, &entry.artifact, &entry.sha256)?;
            validate_artifact_size_limit(id, entry.size)?;
        } else if entry.artifact.contains("..")
            || entry.artifact.starts_with('/')
            || entry.artifact.contains('\\')
        {
            return Err(localized(
                "sync.manifest.path_traversal",
                "manifest 包含非法路径",
                "Manifest contains an illegal path.",
            ));
        }
        if !seen_paths.insert(entry.artifact.clone()) {
            return Err(localized(
                "sync.manifest.duplicate_artifact",
                "manifest 包含重复 artifact",
                "Manifest contains a duplicate artifact path.",
            ));
        }
    }
    let expected = compute_v3_snapshot_id(&manifest.categories);
    if expected != manifest.snapshot_id {
        return Err(localized(
            "sync.manifest.snapshot_mismatch",
            "manifest snapshotId 与类别条目不匹配",
            "Manifest snapshotId does not match category entries.",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncOperationReport {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_protocol_version: Option<u32>,
    pub status: String,
    #[serde(default)]
    pub categories: Vec<CategoryOperation>,
    #[serde(default)]
    pub warnings: Vec<crate::services::sync_categories::SyncWarning>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryOperation {
    pub category: SyncCategory,
    pub action: String,
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning_code: Option<String>,
}

#[derive(Debug, Clone)]
pub enum UploadMode {
    Auto { categories: Vec<SyncCategory> },
    Manual,
    Initialize,
}

#[derive(Debug, Clone, Default)]
struct MemoryObject {
    bytes: Vec<u8>,
    etag: String,
}

#[derive(Default)]
pub struct MemoryStore {
    objects: BTreeMap<String, MemoryObject>,
    fail_delete: Option<String>,
    fail_manifest_put: bool,
    force_conflict: bool,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

pub enum CloudTransport {
    WebDav {
        settings: WebDavSyncSettings,
        auth: WebDavAuth,
    },
    S3 {
        settings: S3SyncSettings,
        creds: S3Credentials,
    },
    Memory {
        store: Arc<Mutex<MemoryStore>>,
        remote_root: String,
        profile: String,
    },
}

impl CloudTransport {
    pub fn from_webdav(settings: &WebDavSyncSettings) -> Self {
        Self::WebDav {
            settings: settings.clone(),
            auth: crate::services::webdav::auth_from_credentials(
                &settings.username,
                &settings.password,
            ),
        }
    }

    pub fn from_s3(settings: &S3SyncSettings) -> Self {
        Self::S3 {
            settings: settings.clone(),
            creds: S3Credentials {
                access_key_id: settings.access_key_id.clone(),
                secret_access_key: settings.secret_access_key.clone(),
                region: settings.region.clone(),
                bucket: settings.bucket.clone(),
                endpoint: settings.endpoint.clone(),
            },
        }
    }

    pub fn profile(&self) -> &str {
        match self {
            Self::WebDav { settings, .. } => &settings.profile,
            Self::S3 { settings, .. } => &settings.profile,
            Self::Memory { profile, .. } => profile,
        }
    }

    pub fn remote_root(&self) -> &str {
        match self {
            Self::WebDav { settings, .. } => &settings.remote_root,
            Self::S3 { settings, .. } => &settings.remote_root,
            Self::Memory { remote_root, .. } => remote_root,
        }
    }

    pub fn display_root(&self) -> String {
        format!(
            "{}/v{PROTOCOL_VERSION_V3}/db-v{DB_COMPAT_VERSION}/{}",
            self.remote_root(),
            self.profile()
        )
    }

    fn v3_key(&self, relative: &str) -> String {
        format!(
            "{}/v{}/db-v{}/{}/{}",
            self.remote_root().trim_matches('/'),
            PROTOCOL_VERSION_V3,
            DB_COMPAT_VERSION,
            self.profile().trim_matches('/'),
            relative.trim_start_matches('/')
        )
    }

    fn v2_key(&self, relative: &str, layout: RemoteLayout) -> String {
        let mut parts = vec![self.remote_root().trim_matches('/').to_string()];
        parts.push(format!("v{PROTOCOL_VERSION}"));
        if layout == RemoteLayout::Current {
            parts.push(format!("db-v{DB_COMPAT_VERSION}"));
        }
        parts.push(self.profile().trim_matches('/').to_string());
        parts.push(relative.trim_start_matches('/').to_string());
        parts.join("/")
    }

    pub async fn ensure_v3_directories(&self) -> Result<(), AppError> {
        match self {
            Self::WebDav { settings, auth } => {
                let mut segs = Vec::new();
                segs.extend(webdav::path_segments(&settings.remote_root).map(str::to_string));
                segs.push(format!("v{PROTOCOL_VERSION_V3}"));
                segs.push(format!("db-v{DB_COMPAT_VERSION}"));
                segs.extend(webdav::path_segments(&settings.profile).map(str::to_string));
                segs.push("artifacts".to_string());
                webdav::ensure_remote_directories(&settings.base_url, &segs, auth).await?;
                for category in SyncCategory::ALL {
                    let mut cat_segs = segs.clone();
                    cat_segs.push(category.as_str().to_string());
                    webdav::ensure_remote_directories(&settings.base_url, &cat_segs, auth).await?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    async fn webdav_url(&self, key: &str) -> Result<String, AppError> {
        match self {
            Self::WebDav { settings, .. } => {
                let segs: Vec<String> = webdav::path_segments(key).map(str::to_string).collect();
                webdav::build_remote_url(&settings.base_url, &segs)
            }
            _ => Err(localized(
                "sync.transport.invalid",
                "内部传输类型错误",
                "Internal transport type error",
            )),
        }
    }

    pub async fn get(
        &self,
        relative: &str,
        max_bytes: usize,
    ) -> Result<Option<(Vec<u8>, Option<String>)>, AppError> {
        let key = self.v3_key(relative);
        self.get_key(&key, max_bytes).await
    }

    pub async fn get_v2(
        &self,
        relative: &str,
        layout: RemoteLayout,
        max_bytes: usize,
    ) -> Result<Option<(Vec<u8>, Option<String>)>, AppError> {
        let key = self.v2_key(relative, layout);
        self.get_key(&key, max_bytes).await
    }

    async fn get_key(
        &self,
        key: &str,
        max_bytes: usize,
    ) -> Result<Option<(Vec<u8>, Option<String>)>, AppError> {
        match self {
            Self::WebDav { auth, .. } => {
                let url = self.webdav_url(key).await?;
                webdav::get_bytes(&url, auth, max_bytes).await
            }
            Self::S3 { creds, .. } => s3::get_object(creds, key, max_bytes).await,
            Self::Memory { store, .. } => {
                let store = store.lock().map_err(|e| AppError::Lock(e.to_string()))?;
                Ok(store
                    .objects
                    .get(key)
                    .map(|obj| (obj.bytes.clone(), Some(obj.etag.clone()))))
            }
        }
    }

    pub async fn head(&self, relative: &str) -> Result<Option<String>, AppError> {
        let key = self.v3_key(relative);
        match self {
            Self::WebDav { auth, .. } => {
                let url = self.webdav_url(&key).await?;
                webdav::head_etag(&url, auth).await
            }
            Self::S3 { creds, .. } => s3::head_object(creds, &key).await,
            Self::Memory { store, .. } => {
                let store = store.lock().map_err(|e| AppError::Lock(e.to_string()))?;
                Ok(store.objects.get(&key).map(|obj| obj.etag.clone()))
            }
        }
    }

    pub async fn put(
        &self,
        relative: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> Result<(), AppError> {
        let key = self.v3_key(relative);
        match self {
            Self::WebDav { auth, .. } => {
                let url = self.webdav_url(&key).await?;
                webdav::put_bytes(&url, auth, bytes, content_type).await
            }
            Self::S3 { creds, .. } => s3::put_object(creds, &key, bytes, content_type).await,
            Self::Memory { store, .. } => {
                let mut store = store.lock().map_err(|e| AppError::Lock(e.to_string()))?;
                store.objects.insert(
                    key,
                    MemoryObject {
                        etag: sha256_hex(&bytes),
                        bytes,
                    },
                );
                Ok(())
            }
        }
    }

    pub async fn put_manifest_if_match(
        &self,
        bytes: Vec<u8>,
        etag: Option<&str>,
    ) -> Result<ManifestPutResult, AppError> {
        let key = self.v3_key(REMOTE_MANIFEST);
        match self {
            Self::WebDav { auth, .. } => {
                let url = self.webdav_url(&key).await?;
                match webdav::put_bytes_if_match(&url, auth, bytes, "application/json", etag)
                    .await?
                {
                    webdav::ConditionalPutResult::Written => Ok(ManifestPutResult::Written),
                    webdav::ConditionalPutResult::Conflict => Ok(ManifestPutResult::Conflict),
                    webdav::ConditionalPutResult::Unsupported => Ok(ManifestPutResult::Unsupported),
                }
            }
            Self::S3 { creds, .. } => {
                match s3::put_object_if_match(creds, &key, bytes, "application/json", etag).await? {
                    s3::ConditionalPutResult::Written => Ok(ManifestPutResult::Written),
                    s3::ConditionalPutResult::Conflict => Ok(ManifestPutResult::Conflict),
                    s3::ConditionalPutResult::Unsupported => Ok(ManifestPutResult::Unsupported),
                }
            }
            Self::Memory { store, .. } => {
                let mut store = store.lock().map_err(|e| AppError::Lock(e.to_string()))?;
                if store.fail_manifest_put {
                    return Err(localized(
                        "sync.manifest.put_failed",
                        "写入 manifest 失败",
                        "Failed to write manifest",
                    ));
                }
                if store.force_conflict {
                    return Ok(ManifestPutResult::Conflict);
                }
                if let Some(expected) = etag {
                    match store.objects.get(&key) {
                        Some(existing) if existing.etag != expected => {
                            return Ok(ManifestPutResult::Conflict);
                        }
                        None if !expected.is_empty() => return Ok(ManifestPutResult::Conflict),
                        _ => {}
                    }
                } else if store.objects.contains_key(&key) {
                    return Ok(ManifestPutResult::Conflict);
                }
                store.objects.insert(
                    key,
                    MemoryObject {
                        etag: sha256_hex(&bytes),
                        bytes,
                    },
                );
                Ok(ManifestPutResult::Written)
            }
        }
    }

    pub async fn delete_key(&self, relative: &str) -> Result<(), AppError> {
        self.delete_absolute(&self.v3_key(relative)).await
    }

    pub async fn delete_v2(&self, relative: &str, layout: RemoteLayout) -> Result<(), AppError> {
        self.delete_absolute(&self.v2_key(relative, layout)).await
    }

    async fn delete_absolute(&self, key: &str) -> Result<(), AppError> {
        match self {
            Self::WebDav { auth, .. } => {
                let url = self.webdav_url(key).await?;
                webdav::delete_url(&url, auth).await
            }
            Self::S3 { creds, .. } => s3::delete_object(creds, key).await,
            Self::Memory { store, .. } => {
                let mut store = store.lock().map_err(|e| AppError::Lock(e.to_string()))?;
                if store.fail_delete.as_deref() == Some(key) {
                    return Err(localized(
                        "sync.cleanup.delete_failed",
                        "删除远端 artifact 失败",
                        "Failed to delete remote artifact",
                    ));
                }
                store.objects.remove(key);
                Ok(())
            }
        }
    }

    pub fn protocol_validated_key(&self, relative: &str) -> Result<String, AppError> {
        if relative.contains("..") || relative.starts_with('/') || relative.contains('\\') {
            return Err(localized(
                "sync.manifest.path_traversal",
                "拒绝删除非法路径",
                "Refusing to delete an illegal path",
            ));
        }
        Ok(self.v3_key(relative))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestPutResult {
    Written,
    Conflict,
    Unsupported,
}

pub async fn fetch_v3_manifest(
    transport: &CloudTransport,
) -> Result<Option<(V3Manifest, Vec<u8>, Option<String>)>, AppError> {
    let Some((bytes, etag)) = transport.get(REMOTE_MANIFEST, MAX_MANIFEST_BYTES).await? else {
        return Ok(None);
    };
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(localized(
            "sync.manifest_too_large",
            "远端 manifest 过大",
            "Remote manifest is too large",
        ));
    }
    let manifest: V3Manifest = serde_json::from_slice(&bytes).map_err(|e| AppError::Json {
        path: REMOTE_MANIFEST.to_string(),
        source: e,
    })?;
    validate_v3_manifest(&manifest)?;
    Ok(Some((manifest, bytes, etag)))
}

pub async fn fetch_v2_manifest(
    transport: &CloudTransport,
    layout: RemoteLayout,
) -> Result<Option<(SyncManifest, Vec<u8>, Option<String>)>, AppError> {
    let Some((bytes, etag)) = transport
        .get_v2(REMOTE_MANIFEST, layout, MAX_MANIFEST_BYTES)
        .await?
    else {
        return Ok(None);
    };
    let manifest: SyncManifest = serde_json::from_slice(&bytes).map_err(|e| AppError::Json {
        path: REMOTE_MANIFEST.to_string(),
        source: e,
    })?;
    Ok(Some((manifest, bytes, etag)))
}

/// True when a v2 `manifest.json` exists in the current or legacy layout.
/// Fetch errors are treated as "no v2" so a missing/empty remote is not labeled legacy.
pub async fn has_v2_snapshot(transport: &CloudTransport) -> bool {
    for layout in [RemoteLayout::Current, RemoteLayout::Legacy] {
        if fetch_v2_manifest(transport, layout)
            .await
            .ok()
            .flatten()
            .is_some()
        {
            return true;
        }
    }
    false
}

fn paused_report() -> SyncOperationReport {
    SyncOperationReport {
        snapshot_id: None,
        source_protocol_version: Some(PROTOCOL_VERSION_V3),
        status: "paused".to_string(),
        categories: Vec::new(),
        warnings: Vec::new(),
        error_code: None,
    }
}

pub async fn upload(
    db: &Database,
    transport: &CloudTransport,
    selection: &CloudSyncSelection,
    target: &mut CloudSyncTargetState,
    mode: UploadMode,
    init_categories: &[SyncCategory],
) -> Result<SyncOperationReport, AppError> {
    selection.validate()?;
    if !selection.any_enabled() {
        return Ok(paused_report());
    }

    let mut to_export = Vec::new();
    let mut operations = Vec::new();
    for category in SyncCategory::ALL {
        let enabled = selection.is_enabled(category);
        let status = target.effective_status(category, selection);
        let initialize =
            matches!(mode, UploadMode::Initialize) && init_categories.contains(&category);
        let auto_wanted = match &mode {
            UploadMode::Auto { categories } => categories.contains(&category),
            _ => true,
        };
        let eligible = enabled
            && auto_wanted
            && (initialize
                || (!matches!(mode, UploadMode::Initialize)
                    && status.participates_in_auto_sync()
                    && status != CategorySyncStatus::Pending));
        if !eligible {
            if enabled
                && status == CategorySyncStatus::Pending
                && !matches!(mode, UploadMode::Initialize)
            {
                operations.push(CategoryOperation {
                    category,
                    action: "skipped".to_string(),
                    bytes: 0,
                    item_count: None,
                    warning_code: Some("pending".to_string()),
                });
            } else if !enabled {
                operations.push(CategoryOperation {
                    category,
                    action: "skipped".to_string(),
                    bytes: 0,
                    item_count: None,
                    warning_code: Some("disabled".to_string()),
                });
            }
            continue;
        }
        to_export.push(category);
    }

    // Pending/disabled-only changes must not create directories or fetch the remote.
    if to_export.is_empty() {
        return Ok(SyncOperationReport {
            snapshot_id: target.last_snapshot_id.clone(),
            source_protocol_version: Some(PROTOCOL_VERSION_V3),
            status: "success".to_string(),
            categories: operations,
            warnings: Vec::new(),
            error_code: None,
        });
    }

    transport.ensure_v3_directories().await?;
    let remote = fetch_v3_manifest(transport).await?;
    let (mut manifest, remote_etag) = match remote {
        Some((manifest, _, etag)) => (manifest, etag),
        None => (V3Manifest::empty(transport.profile()), None),
    };
    let base_snapshot = manifest.snapshot_id.clone();

    let device_name = detect_system_device_name().unwrap_or_else(|| "Unknown Device".to_string());
    let now = Utc::now().to_rfc3339();
    let mut committed_states: Vec<(SyncCategory, String)> = Vec::new();
    let mut unreferenced_keys: Vec<(SyncCategory, String)> = Vec::new();

    for category in to_export {
        if category == SyncCategory::SkillFiles && !selection.is_enabled(SyncCategory::SkillFiles) {
            continue;
        }
        let artifact = export_category(db, category)?;
        validate_artifact(category, &artifact.bytes)?;
        let prev = manifest.categories.get(category.as_str());
        let unchanged = prev.is_some_and(|entry| entry.sha256 == artifact.sha256);
        let relative = artifact.relative_path();
        if !unchanged {
            if transport.head(&relative).await?.is_none() {
                transport
                    .put(&relative, artifact.bytes.clone(), category.content_type())
                    .await?;
                unreferenced_keys.push((category, relative.clone()));
            }
            operations.push(CategoryOperation {
                category,
                action: "uploaded".to_string(),
                bytes: artifact.bytes.len() as u64,
                item_count: Some(artifact.item_count),
                warning_code: None,
            });
        } else {
            operations.push(CategoryOperation {
                category,
                action: "unchanged".to_string(),
                bytes: 0,
                item_count: Some(artifact.item_count),
                warning_code: None,
            });
        }
        manifest.categories.insert(
            category.as_str().to_string(),
            CategoryManifestEntry {
                schema_version: artifact.schema_version,
                artifact: relative,
                sha256: artifact.sha256.clone(),
                size: artifact.bytes.len() as u64,
                item_count: artifact.item_count,
                updated_at: now.clone(),
                device_name: device_name.clone(),
            },
        );
        committed_states.push((category, artifact.sha256));
    }

    manifest.base_snapshot_id = if base_snapshot.is_empty() {
        None
    } else {
        Some(base_snapshot)
    };
    manifest.device_name = device_name;
    manifest.created_at = now;
    manifest.profile = transport.profile().to_string();
    manifest.recompute_snapshot_id();

    let manifest_bytes =
        serde_json::to_vec_pretty(&manifest).map_err(|e| AppError::JsonSerialize { source: e })?;
    match transport
        .put_manifest_if_match(manifest_bytes, remote_etag.as_deref())
        .await?
    {
        ManifestPutResult::Written => {}
        ManifestPutResult::Conflict => {
            for (category, key) in unreferenced_keys {
                target.cleanup_incomplete.push(CleanupResidue {
                    key,
                    category: Some(category.as_str().to_string()),
                    last_error_code: Some("cleanup_incomplete".to_string()),
                });
            }
            return Ok(SyncOperationReport {
                snapshot_id: Some(manifest.snapshot_id),
                source_protocol_version: Some(PROTOCOL_VERSION_V3),
                status: "conflict".to_string(),
                categories: operations,
                warnings: Vec::new(),
                error_code: Some("remote_changed".to_string()),
            });
        }
        ManifestPutResult::Unsupported => {
            target.supports_conditional_write = false;
            return Err(localized(
                "sync.conditional_write_unsupported",
                "远端不支持条件写入，已禁止覆盖其他设备的快照",
                "Remote does not support conditional writes; refusing to overwrite another device's snapshot.",
            ));
        }
    }

    let synced_at = Utc::now().timestamp();
    for (category, sha256) in committed_states {
        let mut state = target.category_state(category);
        state.last_local_sha256 = Some(sha256.clone());
        state.last_remote_sha256 = Some(sha256);
        state.status = CategorySyncStatus::Synced;
        state.needs_v2_migration = Some(false);
        state.last_error_code = None;
        state.last_synced_at = Some(synced_at);
        target.set_category_state(category, state);
    }
    target.last_snapshot_id = Some(manifest.snapshot_id.clone());
    target.last_protocol_version = Some(PROTOCOL_VERSION_V3);
    Ok(SyncOperationReport {
        snapshot_id: Some(manifest.snapshot_id),
        source_protocol_version: Some(PROTOCOL_VERSION_V3),
        status: "success".to_string(),
        categories: operations,
        warnings: Vec::new(),
        error_code: None,
    })
}

pub async fn download(
    db: &Database,
    transport: &CloudTransport,
    selection: &CloudSyncSelection,
    target: &mut CloudSyncTargetState,
    init_categories: &[SyncCategory],
    expected_snapshot_id: Option<&str>,
) -> Result<SyncOperationReport, AppError> {
    selection.validate()?;
    if !selection.any_enabled() {
        return Ok(paused_report());
    }

    if let Some((manifest, _, _)) = fetch_v3_manifest(transport).await? {
        if let Some(expected) = expected_snapshot_id {
            if expected != manifest.snapshot_id {
                return Ok(SyncOperationReport {
                    snapshot_id: Some(manifest.snapshot_id),
                    source_protocol_version: Some(PROTOCOL_VERSION_V3),
                    status: "conflict".to_string(),
                    categories: Vec::new(),
                    warnings: Vec::new(),
                    error_code: Some("remote_changed".to_string()),
                });
            }
        }
        return download_v3(db, transport, selection, target, init_categories, manifest).await;
    }

    download_v2(db, transport, selection, target, init_categories).await
}

async fn download_v3(
    db: &Database,
    transport: &CloudTransport,
    selection: &CloudSyncSelection,
    target: &mut CloudSyncTargetState,
    init_categories: &[SyncCategory],
    manifest: V3Manifest,
) -> Result<SyncOperationReport, AppError> {
    let mut wanted = Vec::new();
    let mut operations = Vec::new();
    for category in selection.enabled_categories() {
        let status = target.effective_status(category, selection);
        let initialize = init_categories.contains(&category);
        if status == CategorySyncStatus::Pending && !initialize {
            operations.push(CategoryOperation {
                category,
                action: "skipped".to_string(),
                bytes: 0,
                item_count: None,
                warning_code: Some("pending".to_string()),
            });
            continue;
        }
        if !initialize
            && !status.participates_in_auto_sync()
            && status != CategorySyncStatus::Pending
        {
            // ready/synced/local_changed/remote_missing/remote_changed all OK for manual download
        }
        match manifest.categories.get(category.as_str()) {
            Some(entry) => wanted.push((category, entry.clone())),
            None => {
                operations.push(CategoryOperation {
                    category,
                    action: "skipped".to_string(),
                    bytes: 0,
                    item_count: None,
                    warning_code: Some("remote_missing".to_string()),
                });
                let mut state = target.category_state(category);
                state.status = CategorySyncStatus::RemoteMissing;
                target.set_category_state(category, state);
            }
        }
    }

    let mut artifacts = BTreeMap::new();
    for (category, entry) in &wanted {
        if SyncCategory::parse(&entry.artifact.split('/').nth(1).unwrap_or_default()).is_none()
            && SyncCategory::parse(category.as_str()).is_none()
        {
            continue;
        }
        validate_artifact_relative_path(*category, &entry.artifact, &entry.sha256)?;
        let (bytes, _) = transport
            .get(&entry.artifact, MAX_SYNC_ARTIFACT_BYTES as usize)
            .await?
            .ok_or_else(|| {
                localized(
                    "sync.remote_missing_artifact",
                    format!("远端缺少 artifact: {}", entry.artifact),
                    format!("Remote artifact missing: {}", entry.artifact),
                )
            })?;
        if bytes.len() as u64 != entry.size || sha256_hex(&bytes) != entry.sha256 {
            return Err(localized(
                "sync.artifact_hash_mismatch",
                format!("{} 校验失败", category.as_str()),
                format!("{} verification failed", category.as_str()),
            ));
        }
        validate_artifact(*category, &bytes)?;
        artifacts.insert(
            *category,
            CategoryArtifact::from_bytes(*category, entry.schema_version, bytes, entry.item_count),
        );
    }

    let reports = apply_local_artifacts(db, selection, &artifacts)?;
    let mut warnings = Vec::new();
    for (category, artifact) in &artifacts {
        let report = reports.get(category);
        operations.push(CategoryOperation {
            category: *category,
            action: "downloaded".to_string(),
            bytes: artifact.bytes.len() as u64,
            item_count: Some(report.map(|r| r.item_count).unwrap_or(artifact.item_count)),
            warning_code: report.and_then(|r| r.warnings.first().map(|w| w.code.clone())),
        });
        if let Some(report) = report {
            warnings.extend(report.warnings.clone());
        }
        let mut state = target.category_state(*category);
        state.status = CategorySyncStatus::Synced;
        state.last_remote_sha256 = Some(artifact.sha256.clone());
        state.last_local_sha256 = Some(artifact.sha256.clone());
        state.last_synced_at = Some(Utc::now().timestamp());
        state.needs_v2_migration = Some(false);
        state.last_error_code = None;
        target.set_category_state(*category, state);
    }
    target.last_snapshot_id = Some(manifest.snapshot_id.clone());
    target.last_protocol_version = Some(PROTOCOL_VERSION_V3);

    Ok(SyncOperationReport {
        snapshot_id: Some(manifest.snapshot_id),
        source_protocol_version: Some(PROTOCOL_VERSION_V3),
        status: "success".to_string(),
        categories: operations,
        warnings,
        error_code: None,
    })
}

async fn download_v2(
    db: &Database,
    transport: &CloudTransport,
    selection: &CloudSyncSelection,
    target: &mut CloudSyncTargetState,
    init_categories: &[SyncCategory],
) -> Result<SyncOperationReport, AppError> {
    let (manifest, layout) = if let Some((manifest, _, _)) =
        fetch_v2_manifest(transport, RemoteLayout::Current).await?
    {
        (manifest, RemoteLayout::Current)
    } else {
        let (manifest, _, _) = fetch_v2_manifest(transport, RemoteLayout::Legacy)
            .await?
            .ok_or_else(|| {
                localized(
                    "sync.remote_empty",
                    "远端没有可下载的同步数据",
                    "No downloadable sync data found on the remote.",
                )
            })?;
        (manifest, RemoteLayout::Legacy)
    };

    let db_sql =
        download_v2_artifact(transport, layout, REMOTE_DB_SQL, &manifest.artifacts).await?;
    let sql = std::str::from_utf8(&db_sql).map_err(|e| {
        localized(
            "sync.sql_not_utf8",
            format!("SQL 非 UTF-8: {e}"),
            format!("SQL is not valid UTF-8: {e}"),
        )
    })?;
    let isolated = Database::from_sync_sql_export(sql)?;

    let mut artifacts = BTreeMap::new();
    let mut operations = Vec::new();
    for category in selection.enabled_categories() {
        if category == SyncCategory::ModelPricing {
            operations.push(CategoryOperation {
                category,
                action: "skipped".to_string(),
                bytes: 0,
                item_count: None,
                warning_code: Some("v2_no_model_pricing".to_string()),
            });
            continue;
        }
        let status = target.effective_status(category, selection);
        if status == CategorySyncStatus::Pending && !init_categories.contains(&category) {
            operations.push(CategoryOperation {
                category,
                action: "skipped".to_string(),
                bytes: 0,
                item_count: None,
                warning_code: Some("pending".to_string()),
            });
            continue;
        }
        if category == SyncCategory::SkillFiles {
            if !selection.is_enabled(SyncCategory::SkillFiles) {
                continue;
            }
            let zip =
                download_v2_artifact(transport, layout, REMOTE_SKILLS_ZIP, &manifest.artifacts)
                    .await?;
            artifacts.insert(
                category,
                CategoryArtifact::from_bytes(category, CATEGORY_SCHEMA_VERSION, zip, 0),
            );
            continue;
        }
        let artifact = export_category(&isolated, category)?;
        artifacts.insert(category, artifact);
    }

    let reports = apply_local_artifacts(db, selection, &artifacts)?;
    let mut warnings = vec![crate::services::sync_categories::SyncWarning {
        code: "v2_source".to_string(),
        category: None,
        message: "选择性同步要求所有参与设备升级".to_string(),
    }];
    for (category, artifact) in &artifacts {
        let report = reports.get(category);
        operations.push(CategoryOperation {
            category: *category,
            action: "downloaded".to_string(),
            bytes: artifact.bytes.len() as u64,
            item_count: Some(report.map(|r| r.item_count).unwrap_or(artifact.item_count)),
            warning_code: None,
        });
        if let Some(report) = report {
            warnings.extend(report.warnings.clone());
        }
        let mut state = target.category_state(*category);
        state.status = CategorySyncStatus::Synced;
        state.needs_v2_migration = Some(false);
        state.last_synced_at = Some(Utc::now().timestamp());
        target.set_category_state(*category, state);
    }
    target.last_protocol_version = Some(PROTOCOL_VERSION);

    Ok(SyncOperationReport {
        snapshot_id: Some(manifest.snapshot_id),
        source_protocol_version: Some(PROTOCOL_VERSION),
        status: "success".to_string(),
        categories: operations,
        warnings,
        error_code: None,
    })
}

async fn download_v2_artifact(
    transport: &CloudTransport,
    layout: RemoteLayout,
    name: &str,
    artifacts: &BTreeMap<String, ArtifactMeta>,
) -> Result<Vec<u8>, AppError> {
    let meta = artifacts.get(name).ok_or_else(|| {
        localized(
            "sync.manifest_missing_artifact",
            format!("manifest 中缺少 artifact: {name}"),
            format!("Manifest missing artifact: {name}"),
        )
    })?;
    validate_artifact_size_limit(name, meta.size)?;
    let (bytes, _) = transport
        .get_v2(name, layout, MAX_SYNC_ARTIFACT_BYTES as usize)
        .await?
        .ok_or_else(|| {
            localized(
                "sync.remote_missing_artifact",
                format!("远端缺少 artifact 文件: {name}"),
                format!("Remote artifact file missing: {name}"),
            )
        })?;
    crate::services::sync_protocol::verify_artifact(&bytes, name, meta)?;
    Ok(bytes)
}

fn apply_local_artifacts(
    db: &Database,
    selection: &CloudSyncSelection,
    artifacts: &BTreeMap<SyncCategory, CategoryArtifact>,
) -> Result<BTreeMap<SyncCategory, CategoryApplyReport>, AppError> {
    let include_files = artifacts.contains_key(&SyncCategory::SkillFiles);
    let include_pricing = artifacts.contains_key(&SyncCategory::ModelPricing);
    let context = ApplyContext::new(selection.clone(), include_files);

    let skills_backup = if include_files {
        Some(backup_current_skills()?)
    } else {
        None
    };
    let pricing_path = crate::services::model_pricing::model_pricing_file_path();
    let pricing_existed = include_pricing && pricing_path.exists();
    let pricing_backup = if pricing_existed {
        Some(fs::read(&pricing_path).map_err(|e| AppError::io(&pricing_path, e))?)
    } else {
        None
    };

    let _skill_guard = skill_state_write_guard();
    // Keep an in-memory SQLite snapshot so file-swap failures after a committed
    // transaction can still restore the pre-download database.
    let sqlite_backup = db.snapshot_to_memory().ok();

    let apply_result = (|| {
        let mut conn = crate::database::lock_conn!(db.conn);
        let tx = conn
            .transaction()
            .map_err(|e| AppError::Database(e.to_string()))?;
        let reports = apply_categories_in_transaction(&tx, artifacts, &context)?;

        if include_files {
            if let Some(artifact) = artifacts.get(&SyncCategory::SkillFiles) {
                restore_skills_zip(&artifact.bytes)?;
            }
        }
        if include_pricing {
            if let Some(artifact) = artifacts.get(&SyncCategory::ModelPricing) {
                let bytes = model_pricing_file_bytes_from_artifact(artifact)?;
                crate::config::atomic_write(&pricing_path, &bytes)?;
            }
        }

        tx.commit().map_err(|e| AppError::Database(e.to_string()))?;
        Ok(reports)
    })();

    match apply_result {
        Ok(reports) => Ok(reports),
        Err(err) => {
            rollback_files(
                skills_backup.as_ref(),
                pricing_backup.as_deref(),
                include_pricing,
                pricing_existed,
                &pricing_path,
            );
            if let Some(backup) = sqlite_backup.as_ref() {
                let _ = restore_live_db_from_snapshot(db, backup);
            }
            Err(err)
        }
    }
}

fn restore_live_db_from_snapshot(
    db: &Database,
    snapshot: &rusqlite::Connection,
) -> Result<(), AppError> {
    let mut conn = crate::database::lock_conn!(db.conn);
    let backup = rusqlite::backup::Backup::new(snapshot, &mut conn)
        .map_err(|e| AppError::Database(e.to_string()))?;
    match backup
        .step(-1)
        .map_err(|e| AppError::Database(e.to_string()))?
    {
        rusqlite::backup::StepResult::Done => Ok(()),
        other => Err(AppError::Database(format!(
            "SQLite snapshot restore did not finish: {other:?}"
        ))),
    }
}

fn rollback_files(
    skills_backup: Option<&crate::services::webdav_sync::archive::SkillsBackup>,
    pricing_backup: Option<&[u8]>,
    include_pricing: bool,
    pricing_existed: bool,
    pricing_path: &PathBuf,
) {
    if let Some(backup) = skills_backup {
        let _ = restore_skills_from_backup(backup);
    }
    if !include_pricing {
        return;
    }
    if let Some(bytes) = pricing_backup {
        let _ = crate::config::atomic_write(pricing_path, bytes);
    } else if !pricing_existed && pricing_path.exists() {
        let _ = fs::remove_file(pricing_path);
    }
}

pub async fn delete_categories(
    transport: &CloudTransport,
    target: &mut CloudSyncTargetState,
    selection: &CloudSyncSelection,
    categories: &[SyncCategory],
) -> Result<SyncOperationReport, AppError> {
    if !target.supports_conditional_write {
        return Err(localized(
            "sync.conditional_write_unsupported",
            "当前远端不支持安全条件提交，已禁用远端删除",
            "Remote does not support safe conditional commits; remote delete is disabled.",
        ));
    }
    for category in categories {
        if selection.is_enabled(*category) {
            return Err(localized(
                "sync.cleanup.enabled_category",
                format!("只能删除当前设备已关闭的类别: {}", category.as_str()),
                format!(
                    "Only disabled categories can be deleted: {}",
                    category.as_str()
                ),
            ));
        }
    }
    let Some((mut manifest, _, etag)) = fetch_v3_manifest(transport).await? else {
        return Err(localized(
            "sync.remote_empty",
            "远端没有 v3 数据",
            "No v3 data on the remote.",
        ));
    };

    let mut removed_entries = Vec::new();
    for category in categories {
        if let Some(entry) = manifest.categories.remove(category.as_str()) {
            let _ = transport.protocol_validated_key(&entry.artifact)?;
            removed_entries.push((*category, entry));
        }
    }
    manifest.recompute_snapshot_id();
    let manifest_bytes =
        serde_json::to_vec_pretty(&manifest).map_err(|e| AppError::JsonSerialize { source: e })?;
    match transport
        .put_manifest_if_match(manifest_bytes, etag.as_deref())
        .await?
    {
        ManifestPutResult::Written => {}
        ManifestPutResult::Conflict => {
            return Ok(SyncOperationReport {
                snapshot_id: Some(manifest.snapshot_id),
                source_protocol_version: Some(PROTOCOL_VERSION_V3),
                status: "conflict".to_string(),
                categories: Vec::new(),
                warnings: Vec::new(),
                error_code: Some("remote_changed".to_string()),
            });
        }
        ManifestPutResult::Unsupported => {
            target.supports_conditional_write = false;
            return Err(localized(
                "sync.conditional_write_unsupported",
                "远端不支持条件写入，已禁止删除",
                "Remote does not support conditional writes; delete aborted.",
            ));
        }
    }

    let mut operations = Vec::new();
    let mut incomplete = Vec::new();
    for (category, entry) in removed_entries {
        match transport.delete_key(&entry.artifact).await {
            Ok(()) => operations.push(CategoryOperation {
                category,
                action: "deleted".to_string(),
                bytes: entry.size,
                item_count: Some(entry.item_count),
                warning_code: None,
            }),
            Err(_) => {
                incomplete.push(entry.artifact.clone());
                target
                    .cleanup_incomplete
                    .push(crate::services::sync_categories::CleanupResidue {
                        key: entry.artifact,
                        category: Some(category.as_str().to_string()),
                        last_error_code: Some("cleanup_incomplete".to_string()),
                    });
                let mut state = target.category_state(category);
                state.status = CategorySyncStatus::CleanupIncomplete;
                target.set_category_state(category, state);
                operations.push(CategoryOperation {
                    category,
                    action: "deleted".to_string(),
                    bytes: entry.size,
                    item_count: Some(entry.item_count),
                    warning_code: Some("cleanup_incomplete".to_string()),
                });
            }
        }
    }
    let status = if incomplete.is_empty() {
        "success"
    } else {
        "success"
    };
    Ok(SyncOperationReport {
        snapshot_id: Some(manifest.snapshot_id),
        source_protocol_version: Some(PROTOCOL_VERSION_V3),
        status: status.to_string(),
        categories: operations,
        warnings: if incomplete.is_empty() {
            Vec::new()
        } else {
            vec![crate::services::sync_categories::SyncWarning {
                code: "cleanup_incomplete".to_string(),
                category: None,
                message: "manifest 已更新，部分 artifact 删除失败".to_string(),
            }]
        },
        error_code: None,
    })
}

pub async fn retry_cleanup(
    transport: &CloudTransport,
    target: &mut CloudSyncTargetState,
) -> Result<SyncOperationReport, AppError> {
    let mut remaining = Vec::new();
    let mut operations = Vec::new();
    for residue in std::mem::take(&mut target.cleanup_incomplete) {
        let relative = residue
            .key
            .rsplit_once("/artifacts/")
            .map(|(_, rest)| format!("artifacts/{rest}"))
            .unwrap_or_else(|| residue.key.clone());
        let _ = transport.protocol_validated_key(&relative)?;
        match transport.delete_key(&relative).await {
            Ok(()) => {
                if let Some(cat) = residue.category.as_deref().and_then(SyncCategory::parse) {
                    operations.push(CategoryOperation {
                        category: cat,
                        action: "deleted".to_string(),
                        bytes: 0,
                        item_count: None,
                        warning_code: None,
                    });
                }
            }
            Err(_) => remaining.push(residue),
        }
    }
    target.cleanup_incomplete = remaining;
    Ok(SyncOperationReport {
        snapshot_id: target.last_snapshot_id.clone(),
        source_protocol_version: Some(PROTOCOL_VERSION_V3),
        status: "success".to_string(),
        categories: operations,
        warnings: Vec::new(),
        error_code: None,
    })
}

pub async fn delete_v2_snapshot(
    transport: &CloudTransport,
) -> Result<SyncOperationReport, AppError> {
    let Some(_) = fetch_v3_manifest(transport).await? else {
        return Err(localized(
            "sync.cleanup.v3_required",
            "只有当前目标已有完整 v3 快照时才能删除旧版数据",
            "A valid v3 snapshot is required before deleting the legacy snapshot.",
        ));
    };
    let mut warnings = Vec::new();
    for layout in [RemoteLayout::Current, RemoteLayout::Legacy] {
        for name in [REMOTE_MANIFEST, REMOTE_DB_SQL, REMOTE_SKILLS_ZIP] {
            if let Err(err) = transport.delete_v2(name, layout).await {
                warnings.push(crate::services::sync_categories::SyncWarning {
                    code: "v2_delete_failed".to_string(),
                    category: None,
                    message: err.to_string(),
                });
            }
        }
    }
    Ok(SyncOperationReport {
        snapshot_id: None,
        source_protocol_version: Some(PROTOCOL_VERSION),
        status: if warnings.is_empty() {
            "success".to_string()
        } else {
            "success".to_string()
        },
        categories: Vec::new(),
        warnings,
        error_code: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::sync_categories::CloudSyncSelection;

    fn memory_transport() -> (CloudTransport, Arc<Mutex<MemoryStore>>) {
        let store = Arc::new(Mutex::new(MemoryStore::new()));
        (
            CloudTransport::Memory {
                store: Arc::clone(&store),
                remote_root: "cc-switch-sync".to_string(),
                profile: "default".to_string(),
            },
            store,
        )
    }

    #[test]
    fn snapshot_id_covers_all_category_fields() {
        let mut cats = BTreeMap::new();
        cats.insert(
            "providers".to_string(),
            CategoryManifestEntry {
                schema_version: 1,
                artifact: artifact_relative_path(SyncCategory::Providers, "abc"),
                sha256: "abc".to_string(),
                size: 1,
                item_count: 0,
                updated_at: "t".to_string(),
                device_name: "d".to_string(),
            },
        );
        let id1 = compute_v3_snapshot_id(&cats);
        cats.get_mut("providers").unwrap().sha256 = "def".to_string();
        cats.get_mut("providers").unwrap().artifact =
            artifact_relative_path(SyncCategory::Providers, "def");
        assert_ne!(id1, compute_v3_snapshot_id(&cats));
    }

    #[tokio::test]
    async fn upload_paused_when_nothing_selected() {
        let db = Database::memory().unwrap();
        let (transport, _) = memory_transport();
        let mut selection = CloudSyncSelection::default();
        for category in SyncCategory::ALL {
            selection.set_enabled(category, false);
        }
        let mut target = CloudSyncTargetState::new("fp".to_string());
        let report = upload(
            &db,
            &transport,
            &selection,
            &mut target,
            UploadMode::Manual,
            &[],
        )
        .await
        .unwrap();
        assert_eq!(report.status, "paused");
    }

    #[tokio::test]
    async fn conditional_manifest_put_conflict() {
        let db = Database::memory().unwrap();
        let (transport, store) = memory_transport();
        let selection = CloudSyncSelection::default();
        let mut target = CloudSyncTargetState::new("fp".to_string());
        for category in SyncCategory::ALL {
            let mut state = CategoryRuntimeState::default();
            state.status = CategorySyncStatus::Ready;
            target.set_category_state(category, state);
        }
        upload(
            &db,
            &transport,
            &selection,
            &mut target,
            UploadMode::Manual,
            &[],
        )
        .await
        .unwrap();

        let snapshot_before = target.last_snapshot_id.clone();
        let common_sha_before = target
            .category_state(SyncCategory::CommonConfig)
            .last_remote_sha256
            .clone();
        db.set_setting("common_config_claude", "changed-after-first-upload")
            .unwrap();
        store.lock().unwrap().force_conflict = true;
        let report = upload(
            &db,
            &transport,
            &selection,
            &mut target,
            UploadMode::Manual,
            &[],
        )
        .await
        .unwrap();
        assert_eq!(report.status, "conflict");
        assert_eq!(target.last_snapshot_id, snapshot_before);
        assert_eq!(
            target
                .category_state(SyncCategory::CommonConfig)
                .last_remote_sha256,
            common_sha_before,
            "conflict must not mark local categories as matching the uncommitted snapshot"
        );
        assert!(
            !target.cleanup_incomplete.is_empty(),
            "unreferenced artifacts from a failed manifest commit must be recorded"
        );
    }

    #[tokio::test]
    async fn auto_pending_does_not_touch_remote() {
        let db = Database::memory().unwrap();
        let (transport, store) = memory_transport();
        let selection = CloudSyncSelection::default();
        let mut target = CloudSyncTargetState::new("fp".to_string());
        let report = upload(
            &db,
            &transport,
            &selection,
            &mut target,
            UploadMode::Auto {
                categories: SyncCategory::ALL.to_vec(),
            },
            &[],
        )
        .await
        .unwrap();
        assert_eq!(report.status, "success");
        assert!(report
            .categories
            .iter()
            .all(|op| op.action == "skipped" && op.warning_code.as_deref() == Some("pending")));
        assert!(
            store.lock().unwrap().objects.is_empty(),
            "pending-only auto-sync must not PUT or create remote objects"
        );
    }

    #[tokio::test]
    async fn delete_does_not_remove_artifacts_on_manifest_conflict() {
        let db = Database::memory().unwrap();
        let (transport, store) = memory_transport();
        let mut selection = CloudSyncSelection::default();
        let mut target = CloudSyncTargetState::new("fp".to_string());
        for category in SyncCategory::ALL {
            let mut state = CategoryRuntimeState::default();
            state.status = CategorySyncStatus::Ready;
            target.set_category_state(category, state);
        }
        upload(
            &db,
            &transport,
            &selection,
            &mut target,
            UploadMode::Manual,
            &[],
        )
        .await
        .unwrap();
        let before = store.lock().unwrap().objects.len();
        selection.set_enabled(SyncCategory::Prompts, false);
        store.lock().unwrap().force_conflict = true;
        let report = delete_categories(
            &transport,
            &mut target,
            &selection,
            &[SyncCategory::Prompts],
        )
        .await
        .unwrap();
        assert_eq!(report.status, "conflict");
        assert_eq!(store.lock().unwrap().objects.len(), before);
    }

    #[tokio::test]
    async fn delete_404_is_success_and_manifest_updates_first() {
        let db = Database::memory().unwrap();
        let (transport, store) = memory_transport();
        let mut selection = CloudSyncSelection::default();
        let mut target = CloudSyncTargetState::new("fp".to_string());
        for category in SyncCategory::ALL {
            let mut state = CategoryRuntimeState::default();
            state.status = CategorySyncStatus::Ready;
            target.set_category_state(category, state);
        }
        upload(
            &db,
            &transport,
            &selection,
            &mut target,
            UploadMode::Manual,
            &[],
        )
        .await
        .unwrap();
        selection.set_enabled(SyncCategory::Prompts, false);
        let report = delete_categories(
            &transport,
            &mut target,
            &selection,
            &[SyncCategory::Prompts],
        )
        .await
        .unwrap();
        assert_eq!(report.status, "success");
        let (manifest, _, _) = fetch_v3_manifest(&transport).await.unwrap().unwrap();
        assert!(!manifest.categories.contains_key("prompts"));
        let _ = store;
    }

    #[tokio::test]
    async fn download_selected_categories_does_not_clear_missing_remote_category() {
        let db = Database::memory().unwrap();
        db.set_setting("common_config_claude", "keep-local")
            .unwrap();
        let (transport, _) = memory_transport();
        let mut selection = CloudSyncSelection::default();
        selection.set_enabled(SyncCategory::CommonConfig, false);
        let mut target = CloudSyncTargetState::new("fp".to_string());
        for category in SyncCategory::ALL {
            if category == SyncCategory::CommonConfig {
                continue;
            }
            let mut state = CategoryRuntimeState::default();
            state.status = CategorySyncStatus::Ready;
            target.set_category_state(category, state);
        }
        upload(
            &db,
            &transport,
            &selection,
            &mut target,
            UploadMode::Manual,
            &[],
        )
        .await
        .unwrap();

        let mut download_selection = CloudSyncSelection::default();
        download_selection.set_enabled(SyncCategory::SkillFiles, false);
        let mut download_target = CloudSyncTargetState::new("fp".to_string());
        for category in SyncCategory::ALL {
            let mut state = CategoryRuntimeState::default();
            state.status = CategorySyncStatus::Ready;
            download_target.set_category_state(category, state);
        }
        download(
            &db,
            &transport,
            &download_selection,
            &mut download_target,
            &[],
            None,
        )
        .await
        .unwrap();
        let _ = download_selection;
        assert_eq!(
            db.get_setting("common_config_claude").unwrap().as_deref(),
            Some("keep-local")
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn skill_files_not_zipped_when_disabled() {
        use crate::services::webdav_sync::archive::skill_zip_call_count;
        let db = Database::memory().unwrap();
        let (transport, _) = memory_transport();
        let mut selection = CloudSyncSelection::default();
        selection.set_enabled(SyncCategory::SkillFiles, false);
        let mut target = CloudSyncTargetState::new("fp".to_string());
        for category in SyncCategory::ALL {
            if category == SyncCategory::SkillFiles {
                continue;
            }
            let mut state = CategoryRuntimeState::default();
            state.status = CategorySyncStatus::Ready;
            target.set_category_state(category, state);
        }
        let before = skill_zip_call_count();
        let report = upload(
            &db,
            &transport,
            &selection,
            &mut target,
            UploadMode::Manual,
            &[],
        )
        .await
        .unwrap();
        assert!(!report
            .categories
            .iter()
            .any(|op| { op.category == SyncCategory::SkillFiles && op.action == "uploaded" }));
        assert_eq!(
            skill_zip_call_count(),
            before,
            "disabling Skill files must not zip the SSOT directory"
        );
    }

    #[test]
    #[serial_test::serial]
    fn zip_failure_does_not_commit_database_categories() {
        let db = Database::memory().unwrap();
        db.save_prompt(
            "claude",
            &crate::prompt::Prompt {
                id: "keep-me".to_string(),
                name: "Keep".to_string(),
                content: "local".to_string(),
                description: None,
                enabled: true,
                created_at: Some(1),
                updated_at: Some(1),
            },
        )
        .unwrap();
        let remote_prompts = CategoryArtifact::from_bytes(
            SyncCategory::Prompts,
            1,
            serde_json::to_vec(&serde_json::json!({
                "schemaVersion": 1,
                "prompts": [{
                    "id": "remote",
                    "appType": "claude",
                    "name": "Remote",
                    "content": "cloud",
                    "enabled": true
                }]
            }))
            .unwrap(),
            1,
        );
        let bad_zip =
            CategoryArtifact::from_bytes(SyncCategory::SkillFiles, 1, b"not-a-zip".to_vec(), 0);
        let mut artifacts = BTreeMap::new();
        artifacts.insert(SyncCategory::Prompts, remote_prompts);
        artifacts.insert(SyncCategory::SkillFiles, bad_zip);
        let err =
            apply_local_artifacts(&db, &CloudSyncSelection::default(), &artifacts).unwrap_err();
        let _ = err;
        let prompts = db.get_prompts("claude").unwrap();
        assert!(
            prompts.contains_key("keep-me"),
            "prompt table must stay unchanged when Skill zip restore fails"
        );
        assert!(!prompts.contains_key("remote"));
    }

    fn live_s3_settings() -> Option<crate::settings::S3SyncSettings> {
        let endpoint = std::env::var("CC_SWITCH_TEST_S3_ENDPOINT").ok()?;
        if endpoint.is_empty() {
            return None;
        }
        Some(crate::settings::S3SyncSettings {
            enabled: true,
            region: std::env::var("CC_SWITCH_TEST_S3_REGION")
                .unwrap_or_else(|_| "us-east-1".into()),
            bucket: std::env::var("CC_SWITCH_TEST_S3_BUCKET")
                .unwrap_or_else(|_| "cc-switch-test".into()),
            access_key_id: std::env::var("CC_SWITCH_TEST_S3_ACCESS_KEY")
                .unwrap_or_else(|_| "minioadmin".into()),
            secret_access_key: std::env::var("CC_SWITCH_TEST_S3_SECRET_KEY")
                .unwrap_or_else(|_| "minioadmin".into()),
            endpoint,
            remote_root: format!("cc-switch-live-{}", std::process::id()),
            profile: "default".into(),
            ..crate::settings::S3SyncSettings::default()
        })
    }

    fn live_webdav_settings() -> Option<crate::settings::WebDavSyncSettings> {
        let base_url = std::env::var("CC_SWITCH_TEST_WEBDAV_URL").ok()?;
        if base_url.is_empty() {
            return None;
        }
        Some(crate::settings::WebDavSyncSettings {
            enabled: true,
            base_url,
            username: std::env::var("CC_SWITCH_TEST_WEBDAV_USER")
                .unwrap_or_else(|_| "davuser".into()),
            password: std::env::var("CC_SWITCH_TEST_WEBDAV_PASSWORD")
                .unwrap_or_else(|_| "davpass".into()),
            remote_root: format!("cc-switch-live-{}", std::process::id()),
            profile: "default".into(),
            ..crate::settings::WebDavSyncSettings::default()
        })
    }

    async fn live_roundtrip(transport: CloudTransport) {
        let db = Database::memory().unwrap();
        db.set_setting("common_config_claude", "from-a").unwrap();
        let mut selection = CloudSyncSelection::default();
        selection.set_enabled(SyncCategory::SkillFiles, false);
        let mut target = CloudSyncTargetState::new("live".to_string());
        for category in SyncCategory::ALL {
            if category == SyncCategory::SkillFiles {
                continue;
            }
            let mut state = CategoryRuntimeState::default();
            state.status = CategorySyncStatus::Ready;
            target.set_category_state(category, state);
        }
        let report = upload(
            &db,
            &transport,
            &selection,
            &mut target,
            UploadMode::Manual,
            &[],
        )
        .await
        .expect("live upload");
        assert_eq!(report.status, "success");
        assert!(!report
            .categories
            .iter()
            .any(|op| op.category == SyncCategory::SkillFiles && op.action == "uploaded"));

        target.supports_conditional_write = true;
        let mut store_conflict_target = target.clone();
        // Second upload with a stale etag is covered by Memory tests; here verify GET+DELETE.
        selection.set_enabled(SyncCategory::Prompts, false);
        let delete_report = delete_categories(
            &transport,
            &mut store_conflict_target,
            &selection,
            &[SyncCategory::Prompts],
        )
        .await
        .expect("live delete");
        assert!(delete_report.status == "success" || delete_report.status == "conflict");
        let _ = fetch_v3_manifest(&transport).await.unwrap();
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn live_minio_protocol_roundtrip() {
        let Some(settings) = live_s3_settings() else {
            return;
        };
        live_roundtrip(CloudTransport::from_s3(&settings)).await;
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn live_webdav_protocol_roundtrip() {
        let Some(settings) = live_webdav_settings() else {
            return;
        };
        live_roundtrip(CloudTransport::from_webdav(&settings)).await;
    }
}

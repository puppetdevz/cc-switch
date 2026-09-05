//! Central registry for selective cloud-sync categories A–K.
//!
//! Category IDs are protocol fields and must not change after release.

mod adapters;
mod settings_keys;
mod stats;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::AppError;

pub(crate) use adapters::{
    apply_categories_in_transaction, export_category, export_skill_files,
    model_pricing_file_bytes_from_artifact, validate_artifact, ApplyContext, CategoryApplyReport,
    CategoryArtifact, SyncWarning, FILE_CATEGORIES,
};
pub(crate) use settings_keys::{
    classify_settings_key, known_settings_keys, SettingsKeyClass, ALL_KNOWN_SETTINGS_KEYS,
};
pub(crate) use stats::{
    collect_local_category_stats, skill_files_listing_stats, LocalCategoryStats, SkillFilesListing,
};

/// Protocol schema version written into every v3 category artifact.
pub(crate) const CATEGORY_SCHEMA_VERSION: u32 = 1;

pub(crate) const MAX_CATEGORY_JSON_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const MAX_JSON_ARRAY_LEN: usize = 50_000;
pub(crate) const MAX_JSON_STRING_LEN: usize = 2 * 1024 * 1024;
pub(crate) const MAX_JSON_OBJECT_KEYS: usize = 50_000;

/// Stable A–K category identifiers. Serde names are the protocol IDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncCategory {
    Providers,
    Mcp,
    Prompts,
    SkillRepos,
    SkillMetadata,
    SkillFiles,
    Profiles,
    CommonConfig,
    ProxySettings,
    DiagnosticsSettings,
    ModelPricing,
}

impl SyncCategory {
    pub const ALL: [SyncCategory; 11] = [
        Self::Providers,
        Self::Mcp,
        Self::Prompts,
        Self::SkillRepos,
        Self::SkillMetadata,
        Self::SkillFiles,
        Self::Profiles,
        Self::CommonConfig,
        Self::ProxySettings,
        Self::DiagnosticsSettings,
        Self::ModelPricing,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Providers => "providers",
            Self::Mcp => "mcp",
            Self::Prompts => "prompts",
            Self::SkillRepos => "skill_repos",
            Self::SkillMetadata => "skill_metadata",
            Self::SkillFiles => "skill_files",
            Self::Profiles => "profiles",
            Self::CommonConfig => "common_config",
            Self::ProxySettings => "proxy_settings",
            Self::DiagnosticsSettings => "diagnostics_settings",
            Self::ModelPricing => "model_pricing",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "providers" => Some(Self::Providers),
            "mcp" => Some(Self::Mcp),
            "prompts" => Some(Self::Prompts),
            "skill_repos" => Some(Self::SkillRepos),
            "skill_metadata" => Some(Self::SkillMetadata),
            "skill_files" => Some(Self::SkillFiles),
            "profiles" => Some(Self::Profiles),
            "common_config" => Some(Self::CommonConfig),
            "proxy_settings" => Some(Self::ProxySettings),
            "diagnostics_settings" => Some(Self::DiagnosticsSettings),
            "model_pricing" => Some(Self::ModelPricing),
            _ => None,
        }
    }

    pub fn artifact_extension(self) -> &'static str {
        if self == Self::SkillFiles {
            "zip"
        } else {
            "json"
        }
    }

    pub fn content_type(self) -> &'static str {
        if self == Self::SkillFiles {
            "application/zip"
        } else {
            "application/json"
        }
    }

    pub fn is_sensitive(self) -> bool {
        matches!(
            self,
            Self::Providers | Self::Mcp | Self::CommonConfig | Self::ProxySettings
        )
    }

    pub fn is_file_backed(self) -> bool {
        matches!(self, Self::SkillFiles | Self::ModelPricing)
    }
}

fn default_true() -> bool {
    true
}

/// Device-local A–K selection. Missing fields default to selected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudSyncSelection {
    #[serde(default = "default_true")]
    pub providers: bool,
    #[serde(default = "default_true")]
    pub mcp: bool,
    #[serde(default = "default_true")]
    pub prompts: bool,
    #[serde(default = "default_true")]
    pub skill_repos: bool,
    #[serde(default = "default_true")]
    pub skill_metadata: bool,
    #[serde(default = "default_true")]
    pub skill_files: bool,
    #[serde(default = "default_true")]
    pub profiles: bool,
    #[serde(default = "default_true")]
    pub common_config: bool,
    #[serde(default = "default_true")]
    pub proxy_settings: bool,
    #[serde(default = "default_true")]
    pub diagnostics_settings: bool,
    #[serde(default = "default_true")]
    pub model_pricing: bool,
    /// Unknown future category IDs are retained but not enabled by this client.
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl Default for CloudSyncSelection {
    fn default() -> Self {
        Self {
            providers: true,
            mcp: true,
            prompts: true,
            skill_repos: true,
            skill_metadata: true,
            skill_files: true,
            profiles: true,
            common_config: true,
            proxy_settings: true,
            diagnostics_settings: true,
            model_pricing: true,
            extra: BTreeMap::new(),
        }
    }
}

impl CloudSyncSelection {
    pub fn is_enabled(&self, category: SyncCategory) -> bool {
        match category {
            SyncCategory::Providers => self.providers,
            SyncCategory::Mcp => self.mcp,
            SyncCategory::Prompts => self.prompts,
            SyncCategory::SkillRepos => self.skill_repos,
            SyncCategory::SkillMetadata => self.skill_metadata,
            SyncCategory::SkillFiles => self.skill_files,
            SyncCategory::Profiles => self.profiles,
            SyncCategory::CommonConfig => self.common_config,
            SyncCategory::ProxySettings => self.proxy_settings,
            SyncCategory::DiagnosticsSettings => self.diagnostics_settings,
            SyncCategory::ModelPricing => self.model_pricing,
        }
    }

    pub fn set_enabled(&mut self, category: SyncCategory, enabled: bool) {
        match category {
            SyncCategory::Providers => self.providers = enabled,
            SyncCategory::Mcp => self.mcp = enabled,
            SyncCategory::Prompts => self.prompts = enabled,
            SyncCategory::SkillRepos => self.skill_repos = enabled,
            SyncCategory::SkillMetadata => {
                self.skill_metadata = enabled;
                if !enabled {
                    self.skill_files = false;
                }
            }
            SyncCategory::SkillFiles => {
                self.skill_files = enabled;
                if enabled {
                    self.skill_metadata = true;
                }
            }
            SyncCategory::Profiles => self.profiles = enabled,
            SyncCategory::CommonConfig => self.common_config = enabled,
            SyncCategory::ProxySettings => self.proxy_settings = enabled,
            SyncCategory::DiagnosticsSettings => self.diagnostics_settings = enabled,
            SyncCategory::ModelPricing => self.model_pricing = enabled,
        }
        self.normalize_skill_dependency();
    }

    pub fn normalize_skill_dependency(&mut self) {
        if self.skill_files {
            self.skill_metadata = true;
        }
        if !self.skill_metadata {
            self.skill_files = false;
        }
    }

    pub fn validate(&self) -> Result<(), AppError> {
        if self.skill_files && !self.skill_metadata {
            return Err(AppError::localized(
                "sync.selection.skill_files_requires_metadata",
                "同步 Skill 文件需要同时同步安装清单",
                "Syncing Skill files requires Skill metadata to be enabled.",
            ));
        }
        Ok(())
    }

    pub fn any_enabled(&self) -> bool {
        SyncCategory::ALL
            .iter()
            .any(|category| self.is_enabled(*category))
    }

    pub fn enabled_categories(&self) -> Vec<SyncCategory> {
        SyncCategory::ALL
            .iter()
            .copied()
            .filter(|category| self.is_enabled(*category))
            .collect()
    }
}

/// Per-category runtime status stored per remote target fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CategorySyncStatus {
    Disabled,
    Pending,
    Ready,
    Syncing,
    Synced,
    LocalChanged,
    RemoteChanged,
    RemoteMissing,
    Error,
    CleanupIncomplete,
}

impl CategorySyncStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Pending => "pending",
            Self::Ready => "ready",
            Self::Syncing => "syncing",
            Self::Synced => "synced",
            Self::LocalChanged => "local_changed",
            Self::RemoteChanged => "remote_changed",
            Self::RemoteMissing => "remote_missing",
            Self::Error => "error",
            Self::CleanupIncomplete => "cleanup_incomplete",
        }
    }

    pub fn participates_in_auto_sync(self) -> bool {
        matches!(
            self,
            Self::Ready | Self::Synced | Self::LocalChanged | Self::RemoteMissing
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CategoryRuntimeState {
    #[serde(default)]
    pub status: CategorySyncStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_local_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_remote_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_synced_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs_v2_migration: Option<bool>,
}

impl Default for CategorySyncStatus {
    fn default() -> Self {
        Self::Pending
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CleanupResidue {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error_code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudSyncTargetState {
    #[serde(default)]
    pub fingerprint: String,
    #[serde(default)]
    pub categories: BTreeMap<String, CategoryRuntimeState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_snapshot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_protocol_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_remote_etag: Option<String>,
    #[serde(default)]
    pub cleanup_incomplete: Vec<CleanupResidue>,
    #[serde(default = "default_true")]
    pub supports_conditional_write: bool,
}

impl Default for CloudSyncTargetState {
    fn default() -> Self {
        Self::new(String::new())
    }
}

impl CloudSyncTargetState {
    pub fn new(fingerprint: String) -> Self {
        Self {
            fingerprint,
            categories: BTreeMap::new(),
            last_snapshot_id: None,
            last_protocol_version: None,
            last_remote_etag: None,
            cleanup_incomplete: Vec::new(),
            supports_conditional_write: true,
        }
    }

    pub fn category_state(&self, category: SyncCategory) -> CategoryRuntimeState {
        self.categories
            .get(category.as_str())
            .cloned()
            .unwrap_or_default()
    }

    pub fn set_category_state(&mut self, category: SyncCategory, state: CategoryRuntimeState) {
        self.categories
            .insert(category.as_str().to_string(), state);
    }

    pub fn effective_status(
        &self,
        category: SyncCategory,
        selection: &CloudSyncSelection,
    ) -> CategorySyncStatus {
        if !selection.is_enabled(category) {
            return CategorySyncStatus::Disabled;
        }
        self.category_state(category).status
    }
}

/// Map a SQLite table name to the categories that may have changed.
pub fn categories_for_table(table: &str) -> &'static [SyncCategory] {
    match table.trim().to_ascii_lowercase().as_str() {
        "providers" | "provider_endpoints" => &[SyncCategory::Providers],
        "mcp_servers" => &[SyncCategory::Mcp],
        "prompts" => &[SyncCategory::Prompts],
        "skill_repos" => &[SyncCategory::SkillRepos],
        "skills" => &[SyncCategory::SkillMetadata],
        "profiles" => &[SyncCategory::Profiles],
        "proxy_config" => &[SyncCategory::ProxySettings],
        "model_pricing_file" => &[SyncCategory::ModelPricing],
        "settings" => &[
            SyncCategory::Providers,
            SyncCategory::SkillRepos,
            SyncCategory::Profiles,
            SyncCategory::CommonConfig,
            SyncCategory::ProxySettings,
            SyncCategory::DiagnosticsSettings,
        ],
        _ => &[],
    }
}

pub fn table_maps_to_sync_category(table: &str) -> bool {
    !categories_for_table(table).is_empty()
}

pub fn webdav_target_fingerprint(
    base_url: &str,
    username: &str,
    remote_root: &str,
    profile: &str,
) -> String {
    target_fingerprint(&[
        "webdav",
        base_url.trim(),
        username.trim(),
        remote_root.trim(),
        profile.trim(),
    ])
}

pub fn s3_target_fingerprint(
    region: &str,
    bucket: &str,
    access_key_id: &str,
    endpoint: &str,
    remote_root: &str,
    profile: &str,
) -> String {
    target_fingerprint(&[
        "s3",
        region.trim(),
        bucket.trim(),
        access_key_id.trim(),
        endpoint.trim(),
        remote_root.trim(),
        profile.trim(),
    ])
}

fn target_fingerprint(parts: &[&str]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            hasher.update(b"\n");
        }
        hasher.update(part.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

pub fn artifact_relative_path(category: SyncCategory, sha256: &str) -> String {
    format!(
        "artifacts/{}/{}.{}",
        category.as_str(),
        sha256,
        category.artifact_extension()
    )
}

pub fn validate_artifact_relative_path(
    category: SyncCategory,
    path: &str,
    sha256: &str,
) -> Result<(), AppError> {
    let expected = artifact_relative_path(category, sha256);
    if path != expected {
        return Err(AppError::localized(
            "sync.manifest.artifact_path_invalid",
            format!("artifact 路径不合法: {path}"),
            format!("Invalid artifact path: {path}"),
        ));
    }
    if path.contains("..") || path.starts_with('/') || path.contains('\\') {
        return Err(AppError::localized(
            "sync.manifest.path_traversal",
            "manifest 包含非法路径",
            "Manifest contains an illegal path.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_categories_have_stable_ids() {
        let ids: Vec<_> = SyncCategory::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(
            ids,
            [
                "providers",
                "mcp",
                "prompts",
                "skill_repos",
                "skill_metadata",
                "skill_files",
                "profiles",
                "common_config",
                "proxy_settings",
                "diagnostics_settings",
                "model_pricing",
            ]
        );
        for category in SyncCategory::ALL {
            assert_eq!(SyncCategory::parse(category.as_str()), Some(category));
        }
        assert!(SyncCategory::parse("unknown_future").is_none());
    }

    #[test]
    fn selection_defaults_to_all_enabled() {
        let selection = CloudSyncSelection::default();
        assert!(selection.any_enabled());
        for category in SyncCategory::ALL {
            assert!(selection.is_enabled(category));
        }
    }

    #[test]
    fn missing_selection_fields_default_to_enabled() {
        let selection: CloudSyncSelection = serde_json::from_str("{}").unwrap();
        for category in SyncCategory::ALL {
            assert!(
                selection.is_enabled(category),
                "{:?} should default to enabled",
                category
            );
        }
    }

    #[test]
    fn unknown_selection_fields_are_preserved_but_not_enabled() {
        let selection: CloudSyncSelection =
            serde_json::from_str(r#"{"providers":true,"future_category":true}"#).unwrap();
        assert!(selection.extra.contains_key("future_category"));
        assert!(SyncCategory::parse("future_category").is_none());
        let encoded = serde_json::to_value(&selection).unwrap();
        assert_eq!(encoded.get("future_category"), Some(&serde_json::json!(true)));
    }

    #[test]
    fn enabling_skill_files_forces_metadata() {
        let mut selection = CloudSyncSelection::default();
        selection.skill_metadata = false;
        selection.skill_files = false;
        selection.set_enabled(SyncCategory::SkillFiles, true);
        assert!(selection.skill_metadata);
        assert!(selection.skill_files);
        assert!(selection.validate().is_ok());
    }

    #[test]
    fn disabling_metadata_disables_skill_files() {
        let mut selection = CloudSyncSelection::default();
        selection.set_enabled(SyncCategory::SkillMetadata, false);
        assert!(!selection.skill_metadata);
        assert!(!selection.skill_files);
    }

    #[test]
    fn invalid_skill_files_without_metadata_is_rejected() {
        let selection = CloudSyncSelection {
            skill_metadata: false,
            skill_files: true,
            ..CloudSyncSelection::default()
        };
        assert!(selection.validate().is_err());
    }

    #[test]
    fn all_disabled_is_allowed() {
        let mut selection = CloudSyncSelection::default();
        for category in SyncCategory::ALL {
            selection.set_enabled(category, false);
        }
        assert!(!selection.any_enabled());
        assert!(selection.validate().is_ok());
    }

    #[test]
    fn fingerprint_excludes_secrets_by_construction() {
        let a = webdav_target_fingerprint(
            "https://dav.example.com/dav",
            "alice",
            "cc-switch-sync",
            "default",
        );
        let b = webdav_target_fingerprint(
            "https://dav.example.com/dav",
            "alice",
            "cc-switch-sync",
            "work",
        );
        assert_ne!(a, b);
        assert!(!a.contains("alice"));
        let s3 = s3_target_fingerprint(
            "us-east-1",
            "bucket",
            "AKID",
            "http://minio:9000",
            "cc-switch-sync",
            "default",
        );
        assert_eq!(s3.len(), 64);
    }

    #[test]
    fn table_mapping_covers_sync_tables() {
        assert_eq!(
            categories_for_table("providers"),
            &[SyncCategory::Providers]
        );
        assert_eq!(categories_for_table("mcp_servers"), &[SyncCategory::Mcp]);
        assert!(categories_for_table("proxy_request_logs").is_empty());
        assert!(categories_for_table("model_pricing").is_empty());
        assert!(table_maps_to_sync_category("settings"));
    }
}

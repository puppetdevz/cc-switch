#![allow(non_snake_case)]

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::State;

use crate::commands::sync_support::run_post_import_sync_for_categories;
use crate::error::AppError;
use crate::services::s3_sync as s3_sync_service;
use crate::services::sync_categories::{
    collect_local_category_stats, s3_target_fingerprint, webdav_target_fingerprint,
    CategorySyncStatus, CloudSyncSelection, CloudSyncTargetState, SyncCategory,
};
use crate::services::sync_v3::{self, CloudTransport, UploadMode};
use crate::services::webdav_sync as webdav_sync_service;
use crate::settings::{self, S3SyncSettings, WebDavSyncSettings};
use crate::store::AppState;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitCategoryRequest {
    pub category: SyncCategory,
    pub direction: String,
    #[serde(default)]
    pub expected_snapshot_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteCategoriesRequest {
    pub categories: Vec<SyncCategory>,
}

fn active_transport() -> Result<(CloudTransport, String, bool), AppError> {
    if let Some(s3) = settings::get_s3_sync_settings().filter(|s| s.enabled) {
        s3.validate()?;
        let fingerprint = s3_target_fingerprint(
            &s3.region,
            &s3.bucket,
            &s3.access_key_id,
            &s3.endpoint,
            &s3.remote_root,
            &s3.profile,
        );
        return Ok((CloudTransport::from_s3(&s3), fingerprint, true));
    }
    if let Some(webdav) = settings::get_webdav_sync_settings().filter(|s| s.enabled) {
        webdav.validate()?;
        let fingerprint = webdav_target_fingerprint(
            &webdav.base_url,
            &webdav.username,
            &webdav.remote_root,
            &webdav.profile,
        );
        return Ok((CloudTransport::from_webdav(&webdav), fingerprint, false));
    }
    Err(AppError::localized(
        "sync.not_configured",
        "未配置云同步",
        "Cloud sync is not configured.",
    ))
}

fn enabled_webdav() -> Result<WebDavSyncSettings, AppError> {
    settings::get_webdav_sync_settings()
        .filter(|s| s.enabled)
        .ok_or_else(|| {
            AppError::localized(
                "webdav.sync.not_configured",
                "未配置 WebDAV 同步",
                "WebDAV sync is not configured.",
            )
        })
}

fn enabled_s3() -> Result<S3SyncSettings, AppError> {
    settings::get_s3_sync_settings()
        .filter(|s| s.enabled)
        .ok_or_else(|| {
            AppError::localized(
                "s3.sync.not_configured",
                "未配置 S3 同步",
                "S3 sync is not configured.",
            )
        })
}

#[tauri::command]
pub fn cloud_sync_get_selection() -> Result<CloudSyncSelection, String> {
    Ok(settings::get_cloud_sync_selection())
}

#[tauri::command]
pub fn cloud_sync_set_selection(
    selection: CloudSyncSelection,
) -> Result<CloudSyncSelection, String> {
    let mut next = selection;
    next.normalize_skill_dependency();
    next.validate().map_err(|e| e.to_string())?;
    let stored = settings::set_cloud_sync_selection(next).map_err(|e| e.to_string())?;
    if let Ok((_, fingerprint, _)) = active_transport() {
        let mut target = settings::get_cloud_sync_target(&fingerprint);
        for category in SyncCategory::ALL {
            let mut state = target.category_state(category);
            if stored.is_enabled(category) {
                if state.status == CategorySyncStatus::Disabled || state.last_synced_at.is_none() {
                    state.status = CategorySyncStatus::Pending;
                }
            } else {
                state.status = CategorySyncStatus::Disabled;
            }
            target.set_category_state(category, state);
        }
        let _ = settings::put_cloud_sync_target(target);
    }
    Ok(stored)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryUiState {
    pub category: SyncCategory,
    pub enabled: bool,
    pub status: CategorySyncStatus,
    pub local_item_count: u64,
    pub local_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_file_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_uncompressed_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_item_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_synced_at: Option<i64>,
    pub sensitive: bool,
    pub legacy_combined: bool,
}

#[tauri::command]
pub async fn cloud_sync_get_category_stats(
    state: State<'_, AppState>,
) -> Result<Value, String> {
    let db = state.db.clone();
    let local = tauri::async_runtime::spawn_blocking(move || collect_local_category_stats(&db))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let selection = settings::get_cloud_sync_selection();
    let (remote_categories, legacy_combined, has_v3, snapshot_id) = match active_transport() {
        Ok((transport, _, _)) => match sync_v3::fetch_v3_manifest(&transport).await {
            Ok(Some((manifest, _, _))) => (
                Some(manifest.categories),
                false,
                true,
                Some(manifest.snapshot_id),
            ),
            Ok(None) => (None, true, false, None),
            Err(err) => return Err(err.to_string()),
        },
        Err(_) => (None, false, false, None),
    };
    let target = active_transport()
        .ok()
        .map(|(_, fingerprint, _)| settings::get_cloud_sync_target(&fingerprint))
        .unwrap_or_else(|| CloudSyncTargetState::new(String::new()));

    let mut categories = Vec::new();
    for stat in local {
        let enabled = selection.is_enabled(stat.category);
        let mut status = target.effective_status(stat.category, &selection);
        if enabled && status == CategorySyncStatus::Pending && !has_v3 && legacy_combined {
            status = CategorySyncStatus::Pending;
        }
        let remote = remote_categories
            .as_ref()
            .and_then(|map| map.get(stat.category.as_str()));
        categories.push(CategoryUiState {
            category: stat.category,
            enabled,
            status,
            local_item_count: stat.item_count,
            local_bytes: stat.bytes,
            local_file_count: stat.file_count,
            local_uncompressed_bytes: stat.uncompressed_bytes,
            remote_bytes: remote.map(|e| e.size),
            remote_item_count: remote.map(|e| e.item_count),
            last_synced_at: target.category_state(stat.category).last_synced_at,
            sensitive: stat.category.is_sensitive(),
            legacy_combined: !has_v3 && legacy_combined,
        });
    }
    Ok(json!({
        "selection": selection,
        "categories": categories,
        "paused": !selection.any_enabled(),
        "hasV3": has_v3,
        "legacyCombined": !has_v3 && legacy_combined,
        "snapshotId": snapshot_id,
        "supportsConditionalWrite": target.supports_conditional_write,
        "cleanupIncomplete": target.cleanup_incomplete,
    }))
}

#[tauri::command]
pub async fn cloud_sync_init_category(
    state: State<'_, AppState>,
    request: InitCategoryRequest,
) -> Result<Value, String> {
    let db = state.db.clone();
    let app_state = state.inner().clone();
    let selection = settings::get_cloud_sync_selection();
    if !selection.is_enabled(request.category) {
        return Err(AppError::localized(
            "sync.init.category_disabled",
            "未勾选的类别不能初始化",
            "Disabled categories cannot be initialized.",
        )
        .to_string());
    }
    if request.category == SyncCategory::SkillFiles && !selection.is_enabled(SyncCategory::SkillMetadata)
    {
        return Err(AppError::localized(
            "sync.selection.skill_files_requires_metadata",
            "同步 Skill 文件需要同时同步安装清单",
            "Syncing Skill files requires Skill metadata to be enabled.",
        )
        .to_string());
    }

    let is_s3 = settings::get_s3_sync_settings().is_some_and(|s| s.enabled);
    let init = [request.category];
    let result = if request.direction == "download" {
        if is_s3 {
            let mut settings = enabled_s3().map_err(|e| e.to_string())?;
            s3_sync_service::run_with_sync_lock(s3_sync_service::download_with_init(
                &db,
                &mut settings,
                &init,
                request.expected_snapshot_id.as_deref(),
            ))
            .await
        } else {
            let mut settings = enabled_webdav().map_err(|e| e.to_string())?;
            webdav_sync_service::run_with_sync_lock(webdav_sync_service::download_with_init(
                &db,
                &mut settings,
                &init,
                request.expected_snapshot_id.as_deref(),
            ))
            .await
        }
    } else {
        if is_s3 {
            let mut settings = enabled_s3().map_err(|e| e.to_string())?;
            s3_sync_service::run_with_sync_lock(s3_sync_service::upload_with_mode(
                &db,
                &mut settings,
                UploadMode::Initialize,
                &init,
            ))
            .await
        } else {
            let mut settings = enabled_webdav().map_err(|e| e.to_string())?;
            webdav_sync_service::run_with_sync_lock(webdav_sync_service::upload_with_mode(
                &db,
                &mut settings,
                UploadMode::Initialize,
                &init,
            ))
            .await
        }
    };
    let value = result.map_err(|e| e.to_string())?;
    if request.direction == "download" {
        let restored = vec![request.category];
        let _ = tauri::async_runtime::spawn_blocking(move || {
            run_post_import_sync_for_categories(&app_state, &restored)
        })
        .await;
    }
    Ok(value)
}

#[tauri::command]
pub async fn cloud_sync_delete_categories(
    request: DeleteCategoriesRequest,
) -> Result<Value, String> {
    let (transport, fingerprint, _) = active_transport().map_err(|e| e.to_string())?;
    let selection = settings::get_cloud_sync_selection();
    let mut target = settings::get_cloud_sync_target(&fingerprint);
    let report = crate::services::sync_protocol::run_with_sync_lock(async {
        sync_v3::delete_categories(&transport, &mut target, &selection, &request.categories).await
    })
    .await
    .map_err(|e| e.to_string())?;
    let _ = settings::put_cloud_sync_target(target);
    serde_json::to_value(report).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cloud_sync_delete_v2_snapshot() -> Result<Value, String> {
    let (transport, _, _) = active_transport().map_err(|e| e.to_string())?;
    let report = crate::services::sync_protocol::run_with_sync_lock(async {
        sync_v3::delete_v2_snapshot(&transport).await
    })
    .await
    .map_err(|e| e.to_string())?;
    serde_json::to_value(report).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cloud_sync_retry_cleanup() -> Result<Value, String> {
    let (transport, fingerprint, _) = active_transport().map_err(|e| e.to_string())?;
    let mut target = settings::get_cloud_sync_target(&fingerprint);
    let report = crate::services::sync_protocol::run_with_sync_lock(async {
        sync_v3::retry_cleanup(&transport, &mut target).await
    })
    .await
    .map_err(|e| e.to_string())?;
    let _ = settings::put_cloud_sync_target(target);
    serde_json::to_value(report).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cloud_sync_remote_inventory() -> Result<Value, String> {
    let (transport, fingerprint, _) = active_transport().map_err(|e| e.to_string())?;
    let selection = settings::get_cloud_sync_selection();
    let target = settings::get_cloud_sync_target(&fingerprint);
    let v3 = sync_v3::fetch_v3_manifest(&transport)
        .await
        .map_err(|e| e.to_string())?;
    let v2_current = sync_v3::fetch_v2_manifest(&transport, crate::services::sync_protocol::RemoteLayout::Current)
        .await
        .ok()
        .flatten();
    let v2_legacy = sync_v3::fetch_v2_manifest(&transport, crate::services::sync_protocol::RemoteLayout::Legacy)
        .await
        .ok()
        .flatten();
    Ok(json!({
        "selection": selection,
        "target": target,
        "v3": v3.map(|(m, _, _)| m),
        "v2Current": v2_current.map(|(m, _, _)| m),
        "v2Legacy": v2_legacy.map(|(m, _, _)| m),
        "displayRoot": transport.display_root(),
        "profile": transport.profile(),
        "remoteRoot": transport.remote_root(),
        "supportsConditionalWrite": target.supports_conditional_write,
    }))
}

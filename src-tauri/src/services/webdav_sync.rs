//! WebDAV v2 sync protocol layer with DB compatibility subdirectories.
//!
//! Implements manifest-based synchronization on top of the HTTP transport
//! primitives in [`super::webdav`]. Artifact set: `db.sql` + `skills.zip`.

use std::collections::BTreeMap;

use chrono::Utc;
use serde_json::Value;

use crate::error::AppError;
use crate::services::sync_categories::webdav_target_fingerprint;
use crate::services::sync_v3::{self, CloudTransport, UploadMode};
use crate::services::webdav::{
    auth_from_credentials, build_remote_url, ensure_remote_directories, get_bytes, path_segments,
    test_connection, WebDavAuth,
};
use crate::settings::{self, update_webdav_sync_status, WebDavSyncSettings, WebDavSyncStatus};

pub(crate) use super::sync_protocol::run_with_sync_lock;
use super::sync_protocol::{
    effective_db_compat_version, persist_sync_success_best_effort, validate_manifest_compat,
    RemoteLayout, SyncManifest, DB_COMPAT_VERSION, MAX_MANIFEST_BYTES, PROTOCOL_VERSION,
    REMOTE_MANIFEST,
};

#[cfg(test)]
pub(crate) fn sync_mutex() -> &'static tokio::sync::Mutex<()> {
    super::sync_protocol::sync_mutex()
}

pub(crate) mod archive;

struct RemoteSnapshot {
    layout: RemoteLayout,
    manifest: SyncManifest,
    manifest_bytes: Vec<u8>,
    manifest_etag: Option<String>,
}
// ─── Public API ──────────────────────────────────────────────

/// Check WebDAV connectivity and ensure remote directory structure.
pub async fn check_connection(settings: &WebDavSyncSettings) -> Result<(), AppError> {
    settings.validate()?;
    let auth = auth_for(settings);
    test_connection(&settings.base_url, &auth).await?;
    let dir_segs = remote_dir_segments(settings, RemoteLayout::Current);
    ensure_remote_directories(&settings.base_url, &dir_segs, &auth).await?;
    Ok(())
}

/// Upload selected v3 categories to remote.
pub async fn upload(
    db: &crate::database::Database,
    settings: &mut WebDavSyncSettings,
) -> Result<Value, AppError> {
    upload_with_mode(db, settings, UploadMode::Manual, &[]).await
}

pub async fn upload_with_mode(
    db: &crate::database::Database,
    settings: &mut WebDavSyncSettings,
    mode: UploadMode,
    init_categories: &[crate::services::sync_categories::SyncCategory],
) -> Result<Value, AppError> {
    settings.validate()?;
    let transport = CloudTransport::from_webdav(settings);
    let selection = settings::get_cloud_sync_selection();
    let fingerprint = webdav_target_fingerprint(
        &settings.base_url,
        &settings.username,
        &settings.remote_root,
        &settings.profile,
    );
    let mut target = settings::get_cloud_sync_target(&fingerprint);
    target.fingerprint = fingerprint.clone();
    let report =
        sync_v3::upload(db, &transport, &selection, &mut target, mode, init_categories).await?;
    let _ = settings::put_cloud_sync_target(target);
    persist_operation_status(settings, &report);
    serde_json::to_value(&report).map_err(|e| AppError::JsonSerialize { source: e })
}

/// Download selected v3 (or v2-extracted) categories.
pub async fn download(
    db: &crate::database::Database,
    settings: &mut WebDavSyncSettings,
) -> Result<Value, AppError> {
    download_with_init(db, settings, &[], None).await
}

pub async fn download_with_init(
    db: &crate::database::Database,
    settings: &mut WebDavSyncSettings,
    init_categories: &[crate::services::sync_categories::SyncCategory],
    expected_snapshot_id: Option<&str>,
) -> Result<Value, AppError> {
    settings.validate()?;
    let transport = CloudTransport::from_webdav(settings);
    let selection = settings::get_cloud_sync_selection();
    let fingerprint = webdav_target_fingerprint(
        &settings.base_url,
        &settings.username,
        &settings.remote_root,
        &settings.profile,
    );
    let mut target = settings::get_cloud_sync_target(&fingerprint);
    target.fingerprint = fingerprint;
    let report = sync_v3::download(
        db,
        &transport,
        &selection,
        &mut target,
        init_categories,
        expected_snapshot_id,
    )
    .await?;
    let _ = settings::put_cloud_sync_target(target);
    persist_operation_status(settings, &report);
    serde_json::to_value(&report).map_err(|e| AppError::JsonSerialize { source: e })
}

/// Fetch remote manifest info without downloading artifacts.
pub async fn fetch_remote_info(settings: &WebDavSyncSettings) -> Result<Option<Value>, AppError> {
    settings.validate()?;
    let transport = CloudTransport::from_webdav(settings);
    if let Some((manifest, _, _)) = sync_v3::fetch_v3_manifest(&transport).await? {
        return Ok(Some(serde_json::json!({
            "deviceName": manifest.device_name,
            "createdAt": manifest.created_at,
            "snapshotId": manifest.snapshot_id,
            "version": manifest.protocol_version,
            "protocolVersion": manifest.protocol_version,
            "dbCompatVersion": manifest.db_compat_version,
            "compatible": true,
            "categories": manifest.categories,
            "artifacts": manifest.categories.keys().collect::<Vec<_>>(),
            "layout": "current",
            "remotePath": transport.display_root(),
            "hasV3": true,
            "legacyCombined": false,
        })));
    }
    let auth = auth_for(settings);
    let Some(snapshot) = find_remote_snapshot(settings, &auth).await? else {
        return Ok(None);
    };
    let compatible = validate_manifest_compat(&snapshot.manifest, snapshot.layout).is_ok();
    let db_compat_version = effective_db_compat_version(&snapshot.manifest, snapshot.layout);

    let payload = serde_json::json!({
        "deviceName": snapshot.manifest.device_name,
        "createdAt": snapshot.manifest.created_at,
        "snapshotId": snapshot.manifest.snapshot_id,
        "version": snapshot.manifest.version,
        "protocolVersion": snapshot.manifest.version,
        "dbCompatVersion": db_compat_version,
        "compatible": compatible,
        "artifacts": snapshot.manifest.artifacts.keys().collect::<Vec<_>>(),
        "layout": snapshot.layout.as_str(),
        "remotePath": remote_dir_display(settings, snapshot.layout),
        "hasV3": false,
        "legacyCombined": true,
    });

    Ok(Some(payload))
}

// ─── Sync status persistence ─────────────────────────────────

fn persist_operation_status(
    settings: &mut WebDavSyncSettings,
    report: &sync_v3::SyncOperationReport,
) {
    if report.status == "paused" {
        return;
    }
    if report.status == "success" {
        let _ = persist_sync_success_best_effort(
            settings,
            report.snapshot_id.clone().unwrap_or_default(),
            None,
            persist_sync_success,
        );
    }
}

fn persist_sync_success(
    settings: &mut WebDavSyncSettings,
    manifest_hash: String,
    etag: Option<String>,
) -> Result<(), AppError> {
    let status = WebDavSyncStatus {
        last_sync_at: Some(Utc::now().timestamp()),
        last_error: None,
        last_error_source: None,
        last_local_manifest_hash: Some(manifest_hash.clone()),
        last_remote_manifest_hash: Some(manifest_hash),
        last_remote_etag: etag,
    };
    settings.status = status.clone();
    update_webdav_sync_status(status)
}

async fn find_remote_snapshot(
    settings: &WebDavSyncSettings,
    auth: &WebDavAuth,
) -> Result<Option<RemoteSnapshot>, AppError> {
    if let Some(snapshot) = fetch_remote_snapshot(settings, auth, RemoteLayout::Current).await? {
        return Ok(Some(snapshot));
    }
    fetch_remote_snapshot(settings, auth, RemoteLayout::Legacy).await
}

async fn fetch_remote_snapshot(
    settings: &WebDavSyncSettings,
    auth: &WebDavAuth,
    layout: RemoteLayout,
) -> Result<Option<RemoteSnapshot>, AppError> {
    let manifest_url = remote_file_url(settings, layout, REMOTE_MANIFEST)?;
    let Some((manifest_bytes, manifest_etag)) =
        get_bytes(&manifest_url, auth, MAX_MANIFEST_BYTES).await?
    else {
        return Ok(None);
    };

    let manifest: SyncManifest =
        serde_json::from_slice(&manifest_bytes).map_err(|e| AppError::Json {
            path: REMOTE_MANIFEST.to_string(),
            source: e,
        })?;

    Ok(Some(RemoteSnapshot {
        layout,
        manifest,
        manifest_bytes,
        manifest_etag,
    }))
}
// ─── Remote path helpers ─────────────────────────────────────

fn remote_dir_segments(settings: &WebDavSyncSettings, layout: RemoteLayout) -> Vec<String> {
    let mut segs = Vec::new();
    segs.extend(path_segments(&settings.remote_root).map(str::to_string));
    segs.push(format!("v{PROTOCOL_VERSION}"));
    if layout == RemoteLayout::Current {
        segs.push(format!("db-v{DB_COMPAT_VERSION}"));
    }
    segs.extend(path_segments(&settings.profile).map(str::to_string));
    segs
}

fn remote_file_url(
    settings: &WebDavSyncSettings,
    layout: RemoteLayout,
    file_name: &str,
) -> Result<String, AppError> {
    let mut segs = remote_dir_segments(settings, layout);
    segs.extend(path_segments(file_name).map(str::to_string));
    build_remote_url(&settings.base_url, &segs)
}

fn remote_dir_display(settings: &WebDavSyncSettings, layout: RemoteLayout) -> String {
    let segs = remote_dir_segments(settings, layout);
    format!("/{}", segs.join("/"))
}

fn auth_for(settings: &WebDavSyncSettings) -> WebDavAuth {
    auth_from_credentials(&settings.username, &settings.password)
}

// ─── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_dir_segments_uses_current_layout() {
        let settings = WebDavSyncSettings {
            remote_root: "cc-switch-sync".to_string(),
            profile: "default".to_string(),
            ..WebDavSyncSettings::default()
        };
        let segs = remote_dir_segments(&settings, RemoteLayout::Current);
        assert_eq!(segs, vec!["cc-switch-sync", "v2", "db-v6", "default"]);
    }

    #[test]
    fn remote_dir_segments_uses_legacy_layout() {
        let settings = WebDavSyncSettings {
            remote_root: "cc-switch-sync".to_string(),
            profile: "default".to_string(),
            ..WebDavSyncSettings::default()
        };
        let segs = remote_dir_segments(&settings, RemoteLayout::Legacy);
        assert_eq!(segs, vec!["cc-switch-sync", "v2", "default"]);
    }
}

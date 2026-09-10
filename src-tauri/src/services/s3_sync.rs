//! S3 v2 sync protocol layer.
//!
//! Implements manifest-based synchronization on top of the S3 transport
//! primitives in [`super::s3`]. Artifact set: `db.sql` + `skills.zip`.

use std::collections::BTreeMap;

use chrono::Utc;
use serde_json::Value;

use crate::error::AppError;
use crate::services::s3::{self, S3Credentials};
use crate::services::sync_categories::s3_target_fingerprint;
use crate::services::sync_v3::{self, CloudTransport, UploadMode};
use crate::settings::{self, update_s3_sync_status, S3SyncSettings, WebDavSyncStatus};

pub(crate) use super::sync_protocol::run_with_sync_lock;
use super::sync_protocol::{
    persist_sync_success_best_effort, validate_manifest_compat, RemoteLayout, SyncManifest,
    DB_COMPAT_VERSION, MAX_MANIFEST_BYTES, PROTOCOL_VERSION, REMOTE_MANIFEST,
};

#[cfg(test)]
pub(crate) fn sync_mutex() -> &'static tokio::sync::Mutex<()> {
    super::sync_protocol::sync_mutex()
}

// ─── Public API ──────────────────────────────────────────────

/// Check S3 connectivity by issuing a HEAD request against the bucket.
pub async fn check_connection(settings: &S3SyncSettings) -> Result<(), AppError> {
    settings.validate()?;
    let creds = creds_for(settings);
    s3::test_connection(&creds).await
}

/// Upload selected v3 categories to remote S3.
pub async fn upload(
    db: &crate::database::Database,
    settings: &mut S3SyncSettings,
) -> Result<Value, AppError> {
    upload_with_mode(db, settings, UploadMode::Manual, &[]).await
}

pub async fn upload_with_mode(
    db: &crate::database::Database,
    settings: &mut S3SyncSettings,
    mode: UploadMode,
    init_categories: &[crate::services::sync_categories::SyncCategory],
) -> Result<Value, AppError> {
    settings.validate()?;
    let transport = CloudTransport::from_s3(settings);
    let selection = settings::get_cloud_sync_selection();
    let fingerprint = s3_target_fingerprint(
        &settings.region,
        &settings.bucket,
        &settings.access_key_id,
        &settings.endpoint,
        &settings.remote_root,
        &settings.profile,
    );
    let mut target = settings::get_cloud_sync_target(&fingerprint);
    target.fingerprint = fingerprint;
    let report = sync_v3::upload(
        db,
        &transport,
        &selection,
        &mut target,
        mode,
        init_categories,
    )
    .await?;
    let _ = settings::put_cloud_sync_target(target);
    persist_operation_status(settings, &report);
    serde_json::to_value(&report).map_err(|e| AppError::JsonSerialize { source: e })
}

/// Download selected v3 (or v2-extracted) categories.
pub async fn download(
    db: &crate::database::Database,
    settings: &mut S3SyncSettings,
) -> Result<Value, AppError> {
    download_with_init(db, settings, &[], None).await
}

pub async fn download_with_init(
    db: &crate::database::Database,
    settings: &mut S3SyncSettings,
    init_categories: &[crate::services::sync_categories::SyncCategory],
    expected_snapshot_id: Option<&str>,
) -> Result<Value, AppError> {
    settings.validate()?;
    let transport = CloudTransport::from_s3(settings);
    let selection = settings::get_cloud_sync_selection();
    let fingerprint = s3_target_fingerprint(
        &settings.region,
        &settings.bucket,
        &settings.access_key_id,
        &settings.endpoint,
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
pub async fn fetch_remote_info(settings: &S3SyncSettings) -> Result<Option<Value>, AppError> {
    settings.validate()?;
    let transport = CloudTransport::from_s3(settings);
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
    let creds = creds_for(settings);
    let manifest_key = s3_key(settings, REMOTE_MANIFEST);

    let Some((bytes, _)) = s3::get_object(&creds, &manifest_key, MAX_MANIFEST_BYTES).await? else {
        return Ok(None);
    };

    let manifest: SyncManifest = serde_json::from_slice(&bytes).map_err(|e| AppError::Json {
        path: REMOTE_MANIFEST.to_string(),
        source: e,
    })?;

    let compatible = validate_manifest_compat(&manifest, RemoteLayout::Current).is_ok();

    let payload = serde_json::json!({
        "deviceName": manifest.device_name,
        "createdAt": manifest.created_at,
        "snapshotId": manifest.snapshot_id,
        "version": manifest.version,
        "protocolVersion": manifest.version,
        "dbCompatVersion": manifest.db_compat_version,
        "compatible": compatible,
        "artifacts": manifest.artifacts.keys().collect::<Vec<_>>(),
        "layout": RemoteLayout::Current.as_str(),
        "remotePath": s3_dir_display(settings),
        "hasV3": false,
        "legacyCombined": true,
    });

    Ok(Some(payload))
}

// ─── Sync status persistence ─────────────────────────────────

fn persist_operation_status(settings: &mut S3SyncSettings, report: &sync_v3::SyncOperationReport) {
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
    settings: &mut S3SyncSettings,
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
    update_s3_sync_status(status)
}

// ─── S3 key helpers ──────────────────────────────────────────

/// Build the S3 object key for a given artifact.
///
/// Format: `{remote_root}/v{PROTOCOL_VERSION}/db-v{DB_COMPAT_VERSION}/{profile}/{artifact}`
/// Example: `cc-switch-sync/v2/db-v6/default/manifest.json`
fn s3_key(settings: &S3SyncSettings, artifact: &str) -> String {
    format!(
        "{}/v{}/db-v{}/{}/{}",
        settings.remote_root, PROTOCOL_VERSION, DB_COMPAT_VERSION, settings.profile, artifact
    )
}

fn s3_dir_display(settings: &S3SyncSettings) -> String {
    format!(
        "{}/v{}/db-v{}/{}",
        settings.remote_root, PROTOCOL_VERSION, DB_COMPAT_VERSION, settings.profile
    )
}

fn creds_for(settings: &S3SyncSettings) -> S3Credentials {
    S3Credentials {
        access_key_id: settings.access_key_id.clone(),
        secret_access_key: settings.secret_access_key.clone(),
        region: settings.region.clone(),
        bucket: settings.bucket.clone(),
        endpoint: settings.endpoint.clone(),
    }
}

// ─── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_settings() -> S3SyncSettings {
        S3SyncSettings {
            remote_root: "cc-switch-sync".to_string(),
            profile: "default".to_string(),
            ..S3SyncSettings::default()
        }
    }

    #[test]
    fn s3_key_uses_v2_and_correct_format() {
        let settings = test_settings();
        let key = s3_key(&settings, "manifest.json");
        assert_eq!(key, "cc-switch-sync/v2/db-v6/default/manifest.json");
    }

    #[test]
    fn s3_key_with_custom_profile() {
        let settings = S3SyncSettings {
            remote_root: "my-root".to_string(),
            profile: "work".to_string(),
            ..S3SyncSettings::default()
        };
        assert_eq!(s3_key(&settings, "db.sql"), "my-root/v2/db-v6/work/db.sql");
    }

    #[test]
    fn s3_key_matches_expected_pattern() {
        let settings = test_settings();
        let key = s3_key(&settings, "skills.zip");
        // Should follow {remote_root}/v{version}/db-v{db}/{profile}/{artifact}
        let parts: Vec<&str> = key.splitn(5, '/').collect();
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[0], "cc-switch-sync");
        assert_eq!(parts[1], "v2");
        assert_eq!(parts[2], "db-v6");
        assert_eq!(parts[3], "default");
        assert_eq!(parts[4], "skills.zip");
    }

    #[test]
    fn sync_mutex_is_singleton() {
        let m1 = sync_mutex();
        let m2 = sync_mutex();
        assert!(
            std::ptr::eq(m1, m2),
            "sync_mutex must return the same instance"
        );
    }

    #[test]
    fn creds_for_maps_all_fields() {
        let settings = S3SyncSettings {
            access_key_id: "AKID".to_string(),
            secret_access_key: "SECRET".to_string(),
            region: "us-west-2".to_string(),
            bucket: "my-bucket".to_string(),
            endpoint: "minio.local:9000".to_string(),
            ..S3SyncSettings::default()
        };
        let creds = creds_for(&settings);
        assert_eq!(creds.access_key_id, "AKID");
        assert_eq!(creds.secret_access_key, "SECRET");
        assert_eq!(creds.region, "us-west-2");
        assert_eq!(creds.bucket, "my-bucket");
        assert_eq!(creds.endpoint, "minio.local:9000");
    }
}

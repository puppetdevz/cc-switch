use serde_json::{json, Value};

use crate::app_config::AppType;
use crate::error::AppError;
use crate::services::sync_categories::SyncCategory;
use crate::services::{model_pricing, McpService, PromptService, ProviderService, SkillService};
use crate::settings;
use crate::store::AppState;

pub(crate) fn run_post_import_sync(app_state: &AppState) -> Result<(), AppError> {
    run_post_import_sync_for_categories(app_state, &SyncCategory::ALL)
}

pub(crate) fn run_post_import_sync_for_categories(
    app_state: &AppState,
    categories: &[SyncCategory],
) -> Result<(), AppError> {
    let mut failures = Vec::new();
    let has = |category: SyncCategory| categories.contains(&category);

    if has(SyncCategory::Providers) {
        if let Err(error) = ProviderService::sync_current_to_live(app_state) {
            failures.push(format!("live configuration: {error}"));
        }
    }
    if has(SyncCategory::Mcp) {
        if let Err(error) = McpService::sync_all_enabled(app_state) {
            failures.push(format!("mcp: {error}"));
        }
    }
    if has(SyncCategory::Prompts) {
        if let Err(error) = PromptService::sync_all_to_live(app_state) {
            failures.push(format!("prompts: {error}"));
        }
    }
    if has(SyncCategory::SkillMetadata) || has(SyncCategory::SkillFiles) {
        for app in AppType::all() {
            if let Err(error) = SkillService::sync_to_app(&app_state.db, &app) {
                failures.push(format!("skills: {error}"));
            }
        }
    }
    if has(SyncCategory::ModelPricing) {
        if let Err(error) = model_pricing::sync_local_model_pricing(&app_state.db) {
            failures.push(format!("model pricing: {error}"));
        }
    }
    if has(SyncCategory::CommonConfig) || has(SyncCategory::ProxySettings) {
        if let Err(error) = settings::reload_settings() {
            failures.push(format!("settings cache: {error}"));
        }
    }
    if has(SyncCategory::DiagnosticsSettings) {
        match app_state.db.get_log_config() {
            Ok(log_config) => log::set_max_level(log_config.to_level_filter()),
            Err(error) => {
                log::set_max_level(log::LevelFilter::Info);
                failures.push(format!("runtime log level: {error}"));
            }
        }
    }
    if has(SyncCategory::Providers) || has(SyncCategory::ModelPricing) {
        app_state.usage_cache.invalidate_all();
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(AppError::localized(
            "sync.post_operation_sync_failed",
            format!("数据已恢复，部分运行配置刷新失败: {}", failures.join("; ")),
            format!(
                "Data restored, but some live projections failed: {}",
                failures.join("; ")
            ),
        ))
    }
}

fn post_sync_warning<E: std::fmt::Display>(err: E) -> String {
    AppError::localized(
        "sync.post_operation_sync_failed",
        format!("后置同步状态失败: {err}"),
        format!("Post-operation synchronization failed: {err}"),
    )
    .to_string()
}

pub(crate) fn post_sync_warning_from_result(
    result: Result<Result<(), AppError>, String>,
) -> Option<String> {
    match result {
        Ok(Ok(())) => None,
        Ok(Err(err)) => Some(post_sync_warning(err)),
        Err(err) => Some(post_sync_warning(err)),
    }
}

pub(crate) fn downloaded_categories_from_report(value: &Value) -> Vec<SyncCategory> {
    value
        .get("categories")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let action = item.get("action").and_then(|a| a.as_str())?;
                    if action != "downloaded" {
                        return None;
                    }
                    item.get("category")
                        .and_then(|c| c.as_str())
                        .and_then(SyncCategory::parse)
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn attach_warning(mut value: Value, warning: Option<String>) -> Value {
    if let Some(message) = warning {
        if let Some(obj) = value.as_object_mut() {
            obj.insert("warning".to_string(), Value::String(message));
        }
    }
    value
}

pub(crate) fn success_payload_with_warning(backup_id: String, warning: Option<String>) -> Value {
    attach_warning(
        json!({
            "success": true,
            "message": "SQL imported successfully",
            "backupId": backup_id
        }),
        warning,
    )
}

#[cfg(test)]
mod tests {
    use super::{attach_warning, post_sync_warning_from_result};
    use serde_json::json;

    #[test]
    fn post_sync_warning_from_result_returns_none_on_success() {
        let warning = post_sync_warning_from_result(Ok(Ok(())));
        assert!(warning.is_none());
    }

    #[test]
    fn post_sync_warning_from_result_returns_some_on_sync_error() {
        let warning =
            post_sync_warning_from_result(Ok(Err(crate::error::AppError::Config("boom".into()))));
        assert!(warning.is_some());
    }

    #[tokio::test]
    async fn post_sync_warning_from_result_returns_some_on_join_error() {
        let handle = tokio::spawn(async move {
            panic!("forced join error");
        });
        let join_err = handle.await.expect_err("task should panic");
        let warning = post_sync_warning_from_result(Err(join_err.to_string()));
        assert!(warning.is_some());
    }

    #[test]
    fn attach_warning_adds_warning_without_dropping_existing_fields() {
        let payload = json!({ "status": "downloaded" });
        let updated = attach_warning(payload, Some("post sync warning".to_string()));
        assert_eq!(
            updated.get("status").and_then(|v| v.as_str()),
            Some("downloaded")
        );
        assert_eq!(
            updated.get("warning").and_then(|v| v.as_str()),
            Some("post sync warning")
        );
    }
}

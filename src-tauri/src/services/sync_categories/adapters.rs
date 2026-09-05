//! Category adapters: deterministic export, validation, and transactional replace.

use std::collections::{BTreeMap, HashSet};

use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::database::Database;
use crate::error::AppError;
use crate::services::sync_categories::{
    artifact_relative_path, classify_settings_key, CloudSyncSelection, SettingsKeyClass,
    SyncCategory, CATEGORY_SCHEMA_VERSION, MAX_CATEGORY_JSON_BYTES, MAX_JSON_ARRAY_LEN,
    MAX_JSON_OBJECT_KEYS, MAX_JSON_STRING_LEN,
};
use crate::services::sync_protocol::{localized, sha256_hex, MAX_SYNC_ARTIFACT_BYTES};
use crate::services::webdav_sync::archive;

pub(crate) const FILE_CATEGORIES: [SyncCategory; 2] =
    [SyncCategory::SkillFiles, SyncCategory::ModelPricing];

#[derive(Debug, Clone)]
pub struct CategoryArtifact {
    pub category: SyncCategory,
    pub schema_version: u32,
    pub bytes: Vec<u8>,
    pub sha256: String,
    pub item_count: u64,
}

impl CategoryArtifact {
    pub fn from_bytes(category: SyncCategory, schema_version: u32, bytes: Vec<u8>, item_count: u64) -> Self {
        let sha256 = sha256_hex(&bytes);
        Self {
            category,
            schema_version,
            bytes,
            sha256,
            item_count,
        }
    }

    pub fn relative_path(&self) -> String {
        artifact_relative_path(self.category, &self.sha256)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryApplyReport {
    pub item_count: u64,
    #[serde(default)]
    pub skipped: u64,
    #[serde(default)]
    pub warnings: Vec<SyncWarning>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncWarning {
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<SyncCategory>,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct ApplyContext {
    pub selected: CloudSyncSelection,
    pub skill_files_included: bool,
}

impl ApplyContext {
    pub fn new(selected: CloudSyncSelection, skill_files_included: bool) -> Self {
        Self {
            selected,
            skill_files_included,
        }
    }
}

pub fn export_category(db: &Database, category: SyncCategory) -> Result<CategoryArtifact, AppError> {
    if category == SyncCategory::SkillFiles {
        return export_skill_files();
    }
    let conn = crate::database::lock_conn!(db.conn);
    export_category_on(&conn, category)
}

pub fn export_skill_files() -> Result<CategoryArtifact, AppError> {
    let tmp = tempfile::tempdir().map_err(|e| crate::services::sync_protocol::io_context_localized(
        "sync.snapshot_tmpdir_failed",
        "创建快照临时目录失败",
        "Failed to create temporary directory for snapshot",
        e,
    ))?;
    let zip_path = tmp.path().join("skills.zip");
    archive::zip_skills_ssot(&zip_path)?;
    let bytes = std::fs::read(&zip_path).map_err(|e| AppError::io(&zip_path, e))?;
    crate::services::sync_protocol::validate_artifact_size_limit("skill_files.zip", bytes.len() as u64)?;
    let listing = crate::services::sync_categories::skill_files_listing_stats()?;
    Ok(CategoryArtifact::from_bytes(
        SyncCategory::SkillFiles,
        CATEGORY_SCHEMA_VERSION,
        bytes,
        listing.file_count,
    ))
}

fn export_category_on(conn: &Connection, category: SyncCategory) -> Result<CategoryArtifact, AppError> {
    let (payload, item_count) = match category {
        SyncCategory::Providers => export_providers(conn)?,
        SyncCategory::Mcp => export_mcp(conn)?,
        SyncCategory::Prompts => export_prompts(conn)?,
        SyncCategory::SkillRepos => export_skill_repos(conn)?,
        SyncCategory::SkillMetadata => export_skill_metadata(conn)?,
        SyncCategory::SkillFiles => unreachable!("skill files use zip export"),
        SyncCategory::Profiles => export_profiles(conn)?,
        SyncCategory::CommonConfig => export_settings_category(conn, SyncCategory::CommonConfig)?,
        SyncCategory::ProxySettings => export_proxy_settings(conn)?,
        SyncCategory::DiagnosticsSettings => {
            export_settings_category(conn, SyncCategory::DiagnosticsSettings)?
        }
        SyncCategory::ModelPricing => export_model_pricing()?,
    };
    let wrapped = wrap_schema(payload);
    let bytes = serde_json::to_vec(&wrapped).map_err(|e| AppError::JsonSerialize { source: e })?;
    if bytes.len() > MAX_CATEGORY_JSON_BYTES {
        return Err(localized(
            "sync.artifact_too_large",
            format!("{} 类别数据超过上限", category.as_str()),
            format!("{} category payload exceeds size limit", category.as_str()),
        ));
    }
    Ok(CategoryArtifact::from_bytes(
        category,
        CATEGORY_SCHEMA_VERSION,
        bytes,
        item_count,
    ))
}

pub fn validate_artifact(category: SyncCategory, bytes: &[u8]) -> Result<(), AppError> {
    crate::services::sync_protocol::validate_artifact_size_limit(category.as_str(), bytes.len() as u64)?;
    if category == SyncCategory::SkillFiles {
        if bytes.len() as u64 > MAX_SYNC_ARTIFACT_BYTES {
            return Err(localized(
                "sync.artifact_too_large",
                "Skill 文件压缩包过大",
                "Skill files archive is too large",
            ));
        }
        return Ok(());
    }
    if bytes.len() > MAX_CATEGORY_JSON_BYTES {
        return Err(localized(
            "sync.artifact_too_large",
            format!("{} 类别 JSON 过大", category.as_str()),
            format!("{} category JSON is too large", category.as_str()),
        ));
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|e| AppError::Json {
        path: format!("artifacts/{}.json", category.as_str()),
        source: e,
    })?;
    validate_json_limits(&value, 0)?;
    let schema_version = value
        .get("schemaVersion")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if schema_version == 0 || schema_version > CATEGORY_SCHEMA_VERSION as u64 {
        return Err(localized(
            "sync.artifact.schema_unsupported",
            format!(
                "{} schemaVersion {schema_version} 不受支持",
                category.as_str()
            ),
            format!(
                "{} schemaVersion {schema_version} is not supported",
                category.as_str()
            ),
        ));
    }
    if category == SyncCategory::ModelPricing {
        crate::services::model_pricing::validate_pricing_file_value(
            value.get("file").cloned().unwrap_or(Value::Null),
        )?;
    }
    Ok(())
}

pub fn apply_categories_in_transaction(
    tx: &Transaction<'_>,
    artifacts: &BTreeMap<SyncCategory, CategoryArtifact>,
    context: &ApplyContext,
) -> Result<BTreeMap<SyncCategory, CategoryApplyReport>, AppError> {
    let mut reports = BTreeMap::new();
    let order = [
        SyncCategory::Providers,
        SyncCategory::Mcp,
        SyncCategory::Prompts,
        SyncCategory::SkillRepos,
        SyncCategory::SkillMetadata,
        SyncCategory::Profiles,
        SyncCategory::CommonConfig,
        SyncCategory::ProxySettings,
        SyncCategory::DiagnosticsSettings,
    ];
    for category in order {
        let Some(artifact) = artifacts.get(&category) else {
            continue;
        };
        if category == SyncCategory::SkillFiles || category == SyncCategory::ModelPricing {
            continue;
        }
        let report = replace_db_category(tx, artifact, context)?;
        reports.insert(category, report);
    }
    if let Some(artifact) = artifacts.get(&SyncCategory::ModelPricing) {
        let report = apply_model_pricing_in_transaction(tx, artifact)?;
        reports.insert(SyncCategory::ModelPricing, report);
    }
    Ok(reports)
}

fn replace_db_category(
    tx: &Transaction<'_>,
    artifact: &CategoryArtifact,
    context: &ApplyContext,
) -> Result<CategoryApplyReport, AppError> {
    validate_artifact(artifact.category, &artifact.bytes)?;
    let value: Value = serde_json::from_slice(&artifact.bytes).map_err(|e| AppError::Json {
        path: artifact.category.as_str().to_string(),
        source: e,
    })?;
    match artifact.category {
        SyncCategory::Providers => replace_providers(tx, &value),
        SyncCategory::Mcp => replace_mcp(tx, &value),
        SyncCategory::Prompts => replace_prompts(tx, &value),
        SyncCategory::SkillRepos => replace_skill_repos(tx, &value),
        SyncCategory::SkillMetadata => replace_skill_metadata(tx, &value, context),
        SyncCategory::Profiles => replace_profiles(tx, &value, context),
        SyncCategory::CommonConfig => {
            replace_settings_category(tx, SyncCategory::CommonConfig, &value)
        }
        SyncCategory::ProxySettings => replace_proxy_settings(tx, &value),
        SyncCategory::DiagnosticsSettings => {
            replace_settings_category(tx, SyncCategory::DiagnosticsSettings, &value)
        }
        SyncCategory::SkillFiles | SyncCategory::ModelPricing => Ok(CategoryApplyReport::default()),
    }
}

fn wrap_schema(mut payload: Value) -> Value {
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("schemaVersion".to_string(), json!(CATEGORY_SCHEMA_VERSION));
        return payload;
    }
    json!({
        "schemaVersion": CATEGORY_SCHEMA_VERSION,
        "payload": payload,
    })
}

fn validate_json_limits(value: &Value, depth: usize) -> Result<(), AppError> {
    if depth > 32 {
        return Err(localized(
            "sync.artifact.json_too_deep",
            "类别 JSON 嵌套过深",
            "Category JSON is nested too deeply",
        ));
    }
    match value {
        Value::String(s) if s.len() > MAX_JSON_STRING_LEN => Err(localized(
            "sync.artifact.string_too_long",
            "类别 JSON 字符串过长",
            "Category JSON string is too long",
        )),
        Value::Array(items) => {
            if items.len() > MAX_JSON_ARRAY_LEN {
                return Err(localized(
                    "sync.artifact.array_too_long",
                    "类别 JSON 数组过长",
                    "Category JSON array is too long",
                ));
            }
            for item in items {
                validate_json_limits(item, depth + 1)?;
            }
            Ok(())
        }
        Value::Object(map) => {
            if map.len() > MAX_JSON_OBJECT_KEYS {
                return Err(localized(
                    "sync.artifact.object_too_large",
                    "类别 JSON 对象键过多",
                    "Category JSON object has too many keys",
                ));
            }
            for (key, child) in map {
                if key.len() > MAX_JSON_STRING_LEN {
                    return Err(localized(
                        "sync.artifact.string_too_long",
                        "类别 JSON 键过长",
                        "Category JSON key is too long",
                    ));
                }
                validate_json_limits(child, depth + 1)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

// ─── Providers ───────────────────────────────────────────────

fn export_providers(conn: &Connection) -> Result<(Value, u64), AppError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, app_type, name, settings_config, website_url, category, created_at,
                    sort_index, notes, icon, icon_color, meta, is_current, in_failover_queue
             FROM providers
             ORDER BY app_type, id",
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(json!({
                "id": row.get::<_, String>(0)?,
                "appType": row.get::<_, String>(1)?,
                "name": row.get::<_, String>(2)?,
                "settingsConfig": parse_json_or_null(&row.get::<_, String>(3)?),
                "websiteUrl": row.get::<_, Option<String>>(4)?,
                "category": row.get::<_, Option<String>>(5)?,
                "createdAt": row.get::<_, Option<i64>>(6)?,
                "sortIndex": row.get::<_, Option<i64>>(7)?,
                "notes": row.get::<_, Option<String>>(8)?,
                "icon": row.get::<_, Option<String>>(9)?,
                "iconColor": row.get::<_, Option<String>>(10)?,
                "meta": parse_json_or_null(&row.get::<_, String>(11)?),
                "isCurrent": row.get::<_, bool>(12)?,
                "inFailoverQueue": row.get::<_, bool>(13)?,
            }))
        })
        .map_err(|e| AppError::Database(e.to_string()))?;

    let mut providers = Vec::new();
    for row in rows {
        let mut provider = row.map_err(|e| AppError::Database(e.to_string()))?;
        let id = provider.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let app_type = provider
            .get("appType")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        provider.as_object_mut().unwrap().insert(
            "endpoints".to_string(),
            export_provider_endpoints(conn, &id, &app_type)?,
        );
        providers.push(provider);
    }

    let universal = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'universal_providers'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| AppError::Database(e.to_string()))?
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or_else(|| json!({}));

    let seeded = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'official_providers_seeded'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| AppError::Database(e.to_string()))?
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);

    let item_count = providers.len() as u64;
    Ok((
        json!({
            "providers": providers,
            "universalProviders": universal,
            "officialProvidersSeeded": seeded,
        }),
        item_count,
    ))
}

fn export_provider_endpoints(conn: &Connection, id: &str, app_type: &str) -> Result<Value, AppError> {
    let mut stmt = conn
        .prepare(
            "SELECT url, added_at FROM provider_endpoints
             WHERE provider_id = ?1 AND app_type = ?2
             ORDER BY added_at ASC, url ASC",
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
    let rows = stmt
        .query_map(params![id, app_type], |row| {
            Ok(json!({
                "url": row.get::<_, String>(0)?,
                "addedAt": row.get::<_, Option<i64>>(1)?,
            }))
        })
        .map_err(|e| AppError::Database(e.to_string()))?;
    let mut endpoints = Vec::new();
    for row in rows {
        endpoints.push(row.map_err(|e| AppError::Database(e.to_string()))?);
    }
    Ok(Value::Array(endpoints))
}

fn replace_providers(tx: &Transaction<'_>, value: &Value) -> Result<CategoryApplyReport, AppError> {
    tx.execute("DELETE FROM provider_endpoints", [])
        .map_err(|e| AppError::Database(e.to_string()))?;
    tx.execute("DELETE FROM providers", [])
        .map_err(|e| AppError::Database(e.to_string()))?;

    let providers = value
        .get("providers")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    for provider in &providers {
        let id = required_str(provider, "id")?;
        let app_type = required_str(provider, "appType")?;
        tx.execute(
            "INSERT INTO providers (
                id, app_type, name, settings_config, website_url, category, created_at,
                sort_index, notes, icon, icon_color, meta, is_current, in_failover_queue
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                id,
                app_type,
                opt_str(provider, "name").unwrap_or_else(|| id.to_string()),
                json_field_as_text(provider, "settingsConfig"),
                opt_str(provider, "websiteUrl"),
                opt_str(provider, "category"),
                opt_i64(provider, "createdAt"),
                opt_i64(provider, "sortIndex"),
                opt_str(provider, "notes"),
                opt_str(provider, "icon"),
                opt_str(provider, "iconColor"),
                json_field_as_text(provider, "meta"),
                opt_bool(provider, "isCurrent"),
                opt_bool(provider, "inFailoverQueue"),
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        if let Some(endpoints) = provider.get("endpoints").and_then(|v| v.as_array()) {
            for endpoint in endpoints {
                tx.execute(
                    "INSERT INTO provider_endpoints (provider_id, app_type, url, added_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        id,
                        app_type,
                        required_str(endpoint, "url")?,
                        opt_i64(endpoint, "addedAt"),
                    ],
                )
                .map_err(|e| AppError::Database(e.to_string()))?;
            }
        }
    }

    let universal = value.get("universalProviders").cloned().unwrap_or(json!({}));
    upsert_setting(
        tx,
        "universal_providers",
        &serde_json::to_string(&universal).map_err(|e| AppError::JsonSerialize { source: e })?,
    )?;
    let seeded = value
        .get("officialProvidersSeeded")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    upsert_setting(tx, "official_providers_seeded", if seeded { "true" } else { "false" })?;

    Ok(CategoryApplyReport {
        item_count: providers.len() as u64,
        ..Default::default()
    })
}

// ─── MCP ─────────────────────────────────────────────────────

fn export_mcp(conn: &Connection) -> Result<(Value, u64), AppError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, name, server_config, description, homepage, docs, tags,
                    enabled_claude, enabled_codex, enabled_gemini, enabled_grokbuild,
                    enabled_opencode, enabled_hermes
             FROM mcp_servers
             ORDER BY id",
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(json!({
                "id": row.get::<_, String>(0)?,
                "name": row.get::<_, String>(1)?,
                "serverConfig": parse_json_or_null(&row.get::<_, String>(2)?),
                "description": row.get::<_, Option<String>>(3)?,
                "homepage": row.get::<_, Option<String>>(4)?,
                "docs": row.get::<_, Option<String>>(5)?,
                "tags": parse_json_or_null(&row.get::<_, String>(6)?),
                "enabledClaude": row.get::<_, bool>(7)?,
                "enabledCodex": row.get::<_, bool>(8)?,
                "enabledGemini": row.get::<_, bool>(9)?,
                "enabledGrokbuild": row.get::<_, bool>(10)?,
                "enabledOpencode": row.get::<_, bool>(11)?,
                "enabledHermes": row.get::<_, bool>(12)?,
            }))
        })
        .map_err(|e| AppError::Database(e.to_string()))?;
    let mut servers = Vec::new();
    for row in rows {
        servers.push(row.map_err(|e| AppError::Database(e.to_string()))?);
    }
    let item_count = servers.len() as u64;
    Ok((json!({ "servers": servers }), item_count))
}

fn replace_mcp(tx: &Transaction<'_>, value: &Value) -> Result<CategoryApplyReport, AppError> {
    tx.execute("DELETE FROM mcp_servers", [])
        .map_err(|e| AppError::Database(e.to_string()))?;
    let servers = value
        .get("servers")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    for server in &servers {
        tx.execute(
            "INSERT INTO mcp_servers (
                id, name, server_config, description, homepage, docs, tags,
                enabled_claude, enabled_codex, enabled_gemini, enabled_grokbuild,
                enabled_opencode, enabled_hermes
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                required_str(server, "id")?,
                opt_str(server, "name").unwrap_or_else(|| required_str(server, "id").unwrap_or("").to_string()),
                json_field_as_text(server, "serverConfig"),
                opt_str(server, "description"),
                opt_str(server, "homepage"),
                opt_str(server, "docs"),
                json_field_as_text(server, "tags"),
                opt_bool(server, "enabledClaude"),
                opt_bool(server, "enabledCodex"),
                opt_bool(server, "enabledGemini"),
                opt_bool(server, "enabledGrokbuild"),
                opt_bool(server, "enabledOpencode"),
                opt_bool(server, "enabledHermes"),
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
    }
    Ok(CategoryApplyReport {
        item_count: servers.len() as u64,
        ..Default::default()
    })
}

// ─── Prompts ─────────────────────────────────────────────────

fn export_prompts(conn: &Connection) -> Result<(Value, u64), AppError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, app_type, name, content, description, enabled, created_at, updated_at
             FROM prompts
             ORDER BY app_type, id",
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(json!({
                "id": row.get::<_, String>(0)?,
                "appType": row.get::<_, String>(1)?,
                "name": row.get::<_, String>(2)?,
                "content": row.get::<_, String>(3)?,
                "description": row.get::<_, Option<String>>(4)?,
                "enabled": row.get::<_, bool>(5)?,
                "createdAt": row.get::<_, Option<i64>>(6)?,
                "updatedAt": row.get::<_, Option<i64>>(7)?,
            }))
        })
        .map_err(|e| AppError::Database(e.to_string()))?;
    let mut prompts = Vec::new();
    for row in rows {
        prompts.push(row.map_err(|e| AppError::Database(e.to_string()))?);
    }
    let item_count = prompts.len() as u64;
    Ok((json!({ "prompts": prompts }), item_count))
}

fn replace_prompts(tx: &Transaction<'_>, value: &Value) -> Result<CategoryApplyReport, AppError> {
    tx.execute("DELETE FROM prompts", [])
        .map_err(|e| AppError::Database(e.to_string()))?;
    let prompts = value
        .get("prompts")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    for prompt in &prompts {
        tx.execute(
            "INSERT INTO prompts (
                id, app_type, name, content, description, enabled, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                required_str(prompt, "id")?,
                required_str(prompt, "appType")?,
                opt_str(prompt, "name").unwrap_or_default(),
                opt_str(prompt, "content").unwrap_or_default(),
                opt_str(prompt, "description"),
                opt_bool(prompt, "enabled"),
                opt_i64(prompt, "createdAt"),
                opt_i64(prompt, "updatedAt"),
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
    }
    Ok(CategoryApplyReport {
        item_count: prompts.len() as u64,
        ..Default::default()
    })
}

// ─── Skill repos ─────────────────────────────────────────────

fn export_skill_repos(conn: &Connection) -> Result<(Value, u64), AppError> {
    let mut stmt = conn
        .prepare(
            "SELECT owner, name, branch, enabled FROM skill_repos ORDER BY owner, name",
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(json!({
                "owner": row.get::<_, String>(0)?,
                "name": row.get::<_, String>(1)?,
                "branch": row.get::<_, String>(2)?,
                "enabled": row.get::<_, bool>(3)?,
            }))
        })
        .map_err(|e| AppError::Database(e.to_string()))?;
    let mut repos = Vec::new();
    for row in rows {
        repos.push(row.map_err(|e| AppError::Database(e.to_string()))?);
    }
    let initialized = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'default_skill_repos_initialized'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| AppError::Database(e.to_string()))?
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);
    let item_count = repos.len() as u64;
    Ok((
        json!({
            "repos": repos,
            "defaultSkillReposInitialized": initialized,
        }),
        item_count,
    ))
}

fn replace_skill_repos(tx: &Transaction<'_>, value: &Value) -> Result<CategoryApplyReport, AppError> {
    tx.execute("DELETE FROM skill_repos", [])
        .map_err(|e| AppError::Database(e.to_string()))?;
    let repos = value
        .get("repos")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    for repo in &repos {
        tx.execute(
            "INSERT INTO skill_repos (owner, name, branch, enabled) VALUES (?1, ?2, ?3, ?4)",
            params![
                required_str(repo, "owner")?,
                required_str(repo, "name")?,
                opt_str(repo, "branch").unwrap_or_else(|| "main".to_string()),
                opt_bool(repo, "enabled"),
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
    }
    // Applying D always records the initialized marker so an explicit empty set
    // is not reseeded on the next startup.
    upsert_setting(tx, "default_skill_repos_initialized", "true")?;
    Ok(CategoryApplyReport {
        item_count: repos.len() as u64,
        ..Default::default()
    })
}

// ─── Skill metadata ──────────────────────────────────────────

fn export_skill_metadata(conn: &Connection) -> Result<(Value, u64), AppError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, name, description, directory, repo_owner, repo_name, repo_branch,
                    readme_url, enabled_claude, enabled_codex, enabled_gemini, enabled_grokbuild,
                    enabled_opencode, enabled_hermes, installed_at, content_hash, updated_at
             FROM skills
             ORDER BY id",
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(json!({
                "id": row.get::<_, String>(0)?,
                "name": row.get::<_, String>(1)?,
                "description": row.get::<_, Option<String>>(2)?,
                "directory": row.get::<_, String>(3)?,
                "repoOwner": row.get::<_, Option<String>>(4)?,
                "repoName": row.get::<_, Option<String>>(5)?,
                "repoBranch": row.get::<_, Option<String>>(6)?,
                "readmeUrl": row.get::<_, Option<String>>(7)?,
                "enabledClaude": row.get::<_, bool>(8)?,
                "enabledCodex": row.get::<_, bool>(9)?,
                "enabledGemini": row.get::<_, bool>(10)?,
                "enabledGrokbuild": row.get::<_, bool>(11)?,
                "enabledOpencode": row.get::<_, bool>(12)?,
                "enabledHermes": row.get::<_, bool>(13)?,
                "installedAt": row.get::<_, i64>(14)?,
                "contentHash": row.get::<_, Option<String>>(15)?,
                "updatedAt": row.get::<_, i64>(16)?,
            }))
        })
        .map_err(|e| AppError::Database(e.to_string()))?;
    let mut skills = Vec::new();
    for row in rows {
        skills.push(row.map_err(|e| AppError::Database(e.to_string()))?);
    }
    let item_count = skills.len() as u64;
    Ok((json!({ "skills": skills }), item_count))
}

fn replace_skill_metadata(
    tx: &Transaction<'_>,
    value: &Value,
    context: &ApplyContext,
) -> Result<CategoryApplyReport, AppError> {
    let remote_skills = value
        .get("skills")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    if context.skill_files_included {
        tx.execute("DELETE FROM skills", [])
            .map_err(|e| AppError::Database(e.to_string()))?;
        for skill in &remote_skills {
            insert_skill_row(tx, skill)?;
        }
        return Ok(CategoryApplyReport {
            item_count: remote_skills.len() as u64,
            ..Default::default()
        });
    }

    let mut local_ids = HashSet::new();
    {
        let mut stmt = tx
            .prepare("SELECT id FROM skills")
            .map_err(|e| AppError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| AppError::Database(e.to_string()))?;
        for row in rows {
            local_ids.insert(row.map_err(|e| AppError::Database(e.to_string()))?);
        }
    }

    let mut updated = 0u64;
    let mut skipped = 0u64;
    for skill in &remote_skills {
        let id = required_str(skill, "id")?;
        if !local_ids.contains(id) {
            skipped += 1;
            continue;
        }
        tx.execute(
            "UPDATE skills SET
                repo_owner = ?2,
                repo_name = ?3,
                repo_branch = ?4,
                readme_url = ?5,
                enabled_claude = ?6,
                enabled_codex = ?7,
                enabled_gemini = ?8,
                enabled_grokbuild = ?9,
                enabled_opencode = ?10,
                enabled_hermes = ?11,
                updated_at = ?12
             WHERE id = ?1",
            params![
                id,
                opt_str(skill, "repoOwner"),
                opt_str(skill, "repoName"),
                opt_str(skill, "repoBranch"),
                opt_str(skill, "readmeUrl"),
                opt_bool(skill, "enabledClaude"),
                opt_bool(skill, "enabledCodex"),
                opt_bool(skill, "enabledGemini"),
                opt_bool(skill, "enabledGrokbuild"),
                opt_bool(skill, "enabledOpencode"),
                opt_bool(skill, "enabledHermes"),
                opt_i64(skill, "updatedAt").unwrap_or(0),
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        updated += 1;
    }

    let mut warnings = Vec::new();
    if skipped > 0 {
        warnings.push(SyncWarning {
            code: "skill_metadata.skipped_missing_local_files".to_string(),
            category: Some(SyncCategory::SkillMetadata),
            message: format!("缺少本机文件，未导入 {skipped} 个远端 Skill"),
        });
    }
    Ok(CategoryApplyReport {
        item_count: updated,
        skipped,
        warnings,
    })
}

fn insert_skill_row(tx: &Transaction<'_>, skill: &Value) -> Result<(), AppError> {
    tx.execute(
        "INSERT INTO skills (
            id, name, description, directory, repo_owner, repo_name, repo_branch,
            readme_url, enabled_claude, enabled_codex, enabled_gemini, enabled_grokbuild,
            enabled_opencode, enabled_hermes, installed_at, content_hash, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        params![
            required_str(skill, "id")?,
            opt_str(skill, "name").unwrap_or_default(),
            opt_str(skill, "description"),
            opt_str(skill, "directory").unwrap_or_default(),
            opt_str(skill, "repoOwner"),
            opt_str(skill, "repoName"),
            opt_str(skill, "repoBranch"),
            opt_str(skill, "readmeUrl"),
            opt_bool(skill, "enabledClaude"),
            opt_bool(skill, "enabledCodex"),
            opt_bool(skill, "enabledGemini"),
            opt_bool(skill, "enabledGrokbuild"),
            opt_bool(skill, "enabledOpencode"),
            opt_bool(skill, "enabledHermes"),
            opt_i64(skill, "installedAt").unwrap_or(0),
            opt_str(skill, "contentHash"),
            opt_i64(skill, "updatedAt").unwrap_or(0),
        ],
    )
    .map_err(|e| AppError::Database(e.to_string()))?;
    Ok(())
}

// ─── Profiles ────────────────────────────────────────────────

fn export_profiles(conn: &Connection) -> Result<(Value, u64), AppError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, name, payload, sort_order, created_at, updated_at
             FROM profiles
             ORDER BY id",
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(json!({
                "id": row.get::<_, String>(0)?,
                "name": row.get::<_, String>(1)?,
                "payload": parse_json_or_null(&row.get::<_, String>(2)?),
                "sortOrder": row.get::<_, Option<i64>>(3)?,
                "createdAt": row.get::<_, Option<i64>>(4)?,
                "updatedAt": row.get::<_, Option<i64>>(5)?,
            }))
        })
        .map_err(|e| AppError::Database(e.to_string()))?;
    let mut profiles = Vec::new();
    for row in rows {
        profiles.push(row.map_err(|e| AppError::Database(e.to_string()))?);
    }

    let mut current = Map::new();
    let mut stmt = conn
        .prepare("SELECT key, value FROM settings WHERE key LIKE 'current_profile_id_%' ORDER BY key")
        .map_err(|e| AppError::Database(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| AppError::Database(e.to_string()))?;
    for row in rows {
        let (key, value) = row.map_err(|e| AppError::Database(e.to_string()))?;
        if matches!(
            classify_settings_key(&key),
            SettingsKeyClass::Category(SyncCategory::Profiles)
        ) {
            current.insert(key, Value::String(value));
        }
    }
    let item_count = profiles.len() as u64;
    Ok((
        json!({
            "profiles": profiles,
            "currentProfileIds": current,
        }),
        item_count,
    ))
}

fn replace_profiles(
    tx: &Transaction<'_>,
    value: &Value,
    context: &ApplyContext,
) -> Result<CategoryApplyReport, AppError> {
    let remote_profiles = value
        .get("profiles")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut local_payloads: BTreeMap<String, Value> = BTreeMap::new();
    {
        let mut stmt = tx
            .prepare("SELECT id, payload FROM profiles")
            .map_err(|e| AppError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| AppError::Database(e.to_string()))?;
        for row in rows {
            let (id, payload) = row.map_err(|e| AppError::Database(e.to_string()))?;
            local_payloads.insert(id, parse_json_or_null(&payload));
        }
    }

    let provider_ids = load_provider_ids(tx)?;
    let mcp_ids = load_id_set(tx, "SELECT id FROM mcp_servers")?;
    let prompt_ids = load_prompt_ids(tx)?;
    let skill_ids = load_id_set(tx, "SELECT id FROM skills")?;

    tx.execute("DELETE FROM profiles", [])
        .map_err(|e| AppError::Database(e.to_string()))?;

    let mut warnings = Vec::new();
    for profile in &remote_profiles {
        let id = required_str(profile, "id")?.to_string();
        let mut payload = profile.get("payload").cloned().unwrap_or(json!({}));
        let local = local_payloads.get(&id);
        let (filtered, profile_warnings) = filter_profile_payload(
            &payload,
            local,
            context,
            &provider_ids,
            &mcp_ids,
            &prompt_ids,
            &skill_ids,
        );
        payload = filtered;
        warnings.extend(profile_warnings);
        tx.execute(
            "INSERT INTO profiles (id, name, payload, sort_order, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id,
                opt_str(profile, "name").unwrap_or_default(),
                serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string()),
                opt_i64(profile, "sortOrder"),
                opt_i64(profile, "createdAt"),
                opt_i64(profile, "updatedAt"),
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
    }

    delete_settings_matching(tx, |key| {
        matches!(
            classify_settings_key(key),
            SettingsKeyClass::Category(SyncCategory::Profiles)
        )
    })?;
    if let Some(current) = value.get("currentProfileIds").and_then(|v| v.as_object()) {
        for (key, val) in current {
            if matches!(
                classify_settings_key(key),
                SettingsKeyClass::Category(SyncCategory::Profiles)
            ) {
                if let Some(id) = val.as_str() {
                    upsert_setting(tx, key, id)?;
                }
            }
        }
    }

    Ok(CategoryApplyReport {
        item_count: remote_profiles.len() as u64,
        warnings,
        ..Default::default()
    })
}

fn filter_profile_payload(
    remote: &Value,
    local: Option<&Value>,
    context: &ApplyContext,
    provider_ids: &HashSet<(String, String)>,
    mcp_ids: &HashSet<String>,
    prompt_ids: &HashSet<(String, String)>,
    skill_ids: &HashSet<String>,
) -> (Value, Vec<SyncWarning>) {
    let mut out = remote.clone();
    let mut warnings = Vec::new();
    let apps: [&str; 3] = ["claude", "claude-desktop", "codex"];

    filter_slot(
        &mut out,
        local,
        "providers",
        context.selected.is_enabled(SyncCategory::Providers),
        &apps,
        |app, id| {
            if provider_ids.contains(&(id.to_string(), app.to_string())) {
                Ok(())
            } else {
                Err(id.to_string())
            }
        },
        &mut warnings,
    );
    filter_id_list_slot(
        &mut out,
        local,
        "mcp",
        context.selected.is_enabled(SyncCategory::Mcp),
        &apps,
        mcp_ids,
        &mut warnings,
    );
    filter_id_list_slot(
        &mut out,
        local,
        "skills",
        context.selected.is_enabled(SyncCategory::SkillMetadata),
        &apps,
        skill_ids,
        &mut warnings,
    );
    filter_slot(
        &mut out,
        local,
        "prompts",
        context.selected.is_enabled(SyncCategory::Prompts),
        &apps,
        |app, id| {
            if prompt_ids.contains(&(id.to_string(), app.to_string())) {
                Ok(())
            } else {
                Err(id.to_string())
            }
        },
        &mut warnings,
    );
    (out, warnings)
}

fn filter_slot<F>(
    remote: &mut Value,
    local: Option<&Value>,
    field: &str,
    allowed: bool,
    apps: &[&str],
    exists: F,
    warnings: &mut Vec<SyncWarning>,
) where
    F: Fn(&str, &str) -> Result<(), String>,
{
    let Some(map) = remote.get_mut(field).and_then(|v| v.as_object_mut()) else {
        return;
    };
    for app in apps {
        if !allowed {
            let local_value = local
                .and_then(|v| v.get(field))
                .and_then(|v| v.get(*app))
                .cloned();
            map.insert((*app).to_string(), local_value.unwrap_or(Value::Null));
            continue;
        }
        if let Some(Value::String(id)) = map.get(*app).cloned() {
            if exists(app, &id).is_err() {
                warnings.push(SyncWarning {
                    code: "profiles.missing_reference".to_string(),
                    category: Some(SyncCategory::Profiles),
                    message: format!("skipped {field} reference {id} for {app}"),
                });
                map.insert((*app).to_string(), Value::Null);
            }
        }
    }
}

fn filter_id_list_slot(
    remote: &mut Value,
    local: Option<&Value>,
    field: &str,
    allowed: bool,
    apps: &[&str],
    existing: &HashSet<String>,
    warnings: &mut Vec<SyncWarning>,
) {
    let Some(map) = remote.get_mut(field).and_then(|v| v.as_object_mut()) else {
        return;
    };
    for app in apps {
        if !allowed {
            let local_value = local
                .and_then(|v| v.get(field))
                .and_then(|v| v.get(*app))
                .cloned();
            map.insert((*app).to_string(), local_value.unwrap_or(Value::Null));
            continue;
        }
        if let Some(Value::Array(ids)) = map.get(*app).cloned() {
            let mut kept = Vec::new();
            for id in ids {
                if let Some(id_str) = id.as_str() {
                    if existing.contains(id_str) {
                        kept.push(Value::String(id_str.to_string()));
                    } else {
                        warnings.push(SyncWarning {
                            code: "profiles.missing_reference".to_string(),
                            category: Some(SyncCategory::Profiles),
                            message: format!("skipped {field} reference {id_str} for {app}"),
                        });
                    }
                }
            }
            map.insert((*app).to_string(), Value::Array(kept));
        }
    }
}

fn load_provider_ids(tx: &Transaction<'_>) -> Result<HashSet<(String, String)>, AppError> {
    let mut stmt = tx
        .prepare("SELECT id, app_type FROM providers")
        .map_err(|e| AppError::Database(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .map_err(|e| AppError::Database(e.to_string()))?;
    let mut set = HashSet::new();
    for row in rows {
        set.insert(row.map_err(|e| AppError::Database(e.to_string()))?);
    }
    Ok(set)
}

fn load_prompt_ids(tx: &Transaction<'_>) -> Result<HashSet<(String, String)>, AppError> {
    let mut stmt = tx
        .prepare("SELECT id, app_type FROM prompts")
        .map_err(|e| AppError::Database(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .map_err(|e| AppError::Database(e.to_string()))?;
    let mut set = HashSet::new();
    for row in rows {
        set.insert(row.map_err(|e| AppError::Database(e.to_string()))?);
    }
    Ok(set)
}

fn load_id_set(tx: &Transaction<'_>, sql: &str) -> Result<HashSet<String>, AppError> {
    let mut stmt = tx
        .prepare(sql)
        .map_err(|e| AppError::Database(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| AppError::Database(e.to_string()))?;
    let mut set = HashSet::new();
    for row in rows {
        set.insert(row.map_err(|e| AppError::Database(e.to_string()))?);
    }
    Ok(set)
}

// ─── Settings-backed categories ──────────────────────────────

fn export_settings_category(
    conn: &Connection,
    category: SyncCategory,
) -> Result<(Value, u64), AppError> {
    let mut stmt = conn
        .prepare("SELECT key, value FROM settings ORDER BY key")
        .map_err(|e| AppError::Database(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .map_err(|e| AppError::Database(e.to_string()))?;
    let mut entries = Map::new();
    for row in rows {
        let (key, value) = row.map_err(|e| AppError::Database(e.to_string()))?;
        if matches!(classify_settings_key(&key), SettingsKeyClass::Category(mapped) if mapped == category)
        {
            entries.insert(key, Value::String(value));
        }
    }
    let item_count = entries.len() as u64;
    Ok((json!({ "entries": entries }), item_count))
}

fn replace_settings_category(
    tx: &Transaction<'_>,
    category: SyncCategory,
    value: &Value,
) -> Result<CategoryApplyReport, AppError> {
    delete_settings_matching(tx, |key| {
        matches!(classify_settings_key(key), SettingsKeyClass::Category(mapped) if mapped == category)
    })?;
    let entries = value
        .get("entries")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    for (key, val) in &entries {
        if matches!(classify_settings_key(key), SettingsKeyClass::Category(mapped) if mapped == category)
        {
            if let Some(text) = val.as_str() {
                upsert_setting(tx, key, text)?;
            }
        }
    }
    Ok(CategoryApplyReport {
        item_count: entries.len() as u64,
        ..Default::default()
    })
}

fn export_proxy_settings(conn: &Connection) -> Result<(Value, u64), AppError> {
    let mut stmt = conn
        .prepare(
            "SELECT app_type, proxy_enabled, listen_address, listen_port, enable_logging,
                    enabled, auto_failover_enabled, max_retries, streaming_first_byte_timeout,
                    streaming_idle_timeout, non_streaming_timeout, circuit_failure_threshold,
                    circuit_success_threshold, circuit_timeout_seconds, circuit_error_rate_threshold,
                    circuit_min_requests, default_cost_multiplier, pricing_model_source
             FROM proxy_config
             ORDER BY app_type",
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(json!({
                "appType": row.get::<_, String>(0)?,
                "proxyEnabled": row.get::<_, i64>(1)? != 0,
                "listenAddress": row.get::<_, String>(2)?,
                "listenPort": row.get::<_, i64>(3)?,
                "enableLogging": row.get::<_, i64>(4)? != 0,
                "enabled": row.get::<_, i64>(5)? != 0,
                "autoFailoverEnabled": row.get::<_, i64>(6)? != 0,
                "maxRetries": row.get::<_, i64>(7)?,
                "streamingFirstByteTimeout": row.get::<_, i64>(8)?,
                "streamingIdleTimeout": row.get::<_, i64>(9)?,
                "nonStreamingTimeout": row.get::<_, i64>(10)?,
                "circuitFailureThreshold": row.get::<_, i64>(11)?,
                "circuitSuccessThreshold": row.get::<_, i64>(12)?,
                "circuitTimeoutSeconds": row.get::<_, i64>(13)?,
                "circuitErrorRateThreshold": row.get::<_, f64>(14)?,
                "circuitMinRequests": row.get::<_, i64>(15)?,
                "defaultCostMultiplier": row.get::<_, String>(16)?,
                "pricingModelSource": row.get::<_, String>(17)?,
            }))
        })
        .map_err(|e| AppError::Database(e.to_string()))?;
    let mut rows_json = Vec::new();
    for row in rows {
        rows_json.push(row.map_err(|e| AppError::Database(e.to_string()))?);
    }
    let (settings_payload, settings_count) =
        export_settings_category(conn, SyncCategory::ProxySettings)?;
    let item_count = rows_json.len() as u64 + settings_count;
    Ok((
        json!({
            "proxyConfig": rows_json,
            "entries": settings_payload.get("entries").cloned().unwrap_or(json!({})),
        }),
        item_count,
    ))
}

fn replace_proxy_settings(
    tx: &Transaction<'_>,
    value: &Value,
) -> Result<CategoryApplyReport, AppError> {
    let mut live_takeover: BTreeMap<String, i64> = BTreeMap::new();
    {
        let mut stmt = tx
            .prepare("SELECT app_type, live_takeover_active FROM proxy_config")
            .map_err(|e| AppError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))
            .map_err(|e| AppError::Database(e.to_string()))?;
        for row in rows {
            let (app, flag) = row.map_err(|e| AppError::Database(e.to_string()))?;
            live_takeover.insert(app, flag);
        }
    }

    let rows = value
        .get("proxyConfig")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    for row in &rows {
        let app_type = required_str(row, "appType")?;
        let live = live_takeover.get(app_type).copied().unwrap_or(0);
        tx.execute(
            "INSERT INTO proxy_config (
                app_type, proxy_enabled, listen_address, listen_port, enable_logging,
                enabled, auto_failover_enabled, max_retries, streaming_first_byte_timeout,
                streaming_idle_timeout, non_streaming_timeout, circuit_failure_threshold,
                circuit_success_threshold, circuit_timeout_seconds, circuit_error_rate_threshold,
                circuit_min_requests, default_cost_multiplier, pricing_model_source,
                live_takeover_active, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, datetime('now'))
            ON CONFLICT(app_type) DO UPDATE SET
                proxy_enabled = excluded.proxy_enabled,
                listen_address = excluded.listen_address,
                listen_port = excluded.listen_port,
                enable_logging = excluded.enable_logging,
                enabled = excluded.enabled,
                auto_failover_enabled = excluded.auto_failover_enabled,
                max_retries = excluded.max_retries,
                streaming_first_byte_timeout = excluded.streaming_first_byte_timeout,
                streaming_idle_timeout = excluded.streaming_idle_timeout,
                non_streaming_timeout = excluded.non_streaming_timeout,
                circuit_failure_threshold = excluded.circuit_failure_threshold,
                circuit_success_threshold = excluded.circuit_success_threshold,
                circuit_timeout_seconds = excluded.circuit_timeout_seconds,
                circuit_error_rate_threshold = excluded.circuit_error_rate_threshold,
                circuit_min_requests = excluded.circuit_min_requests,
                default_cost_multiplier = excluded.default_cost_multiplier,
                pricing_model_source = excluded.pricing_model_source,
                updated_at = excluded.updated_at",
            params![
                app_type,
                bool_as_int(opt_bool(row, "proxyEnabled")),
                opt_str(row, "listenAddress").unwrap_or_else(|| "127.0.0.1".to_string()),
                opt_i64(row, "listenPort").unwrap_or(15721),
                bool_as_int(opt_bool(row, "enableLogging")),
                bool_as_int(opt_bool(row, "enabled")),
                bool_as_int(opt_bool(row, "autoFailoverEnabled")),
                opt_i64(row, "maxRetries").unwrap_or(3),
                opt_i64(row, "streamingFirstByteTimeout").unwrap_or(60),
                opt_i64(row, "streamingIdleTimeout").unwrap_or(120),
                opt_i64(row, "nonStreamingTimeout").unwrap_or(600),
                opt_i64(row, "circuitFailureThreshold").unwrap_or(4),
                opt_i64(row, "circuitSuccessThreshold").unwrap_or(2),
                opt_i64(row, "circuitTimeoutSeconds").unwrap_or(60),
                row.get("circuitErrorRateThreshold")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.6),
                opt_i64(row, "circuitMinRequests").unwrap_or(10),
                opt_str(row, "defaultCostMultiplier").unwrap_or_else(|| "1".to_string()),
                opt_str(row, "pricingModelSource").unwrap_or_else(|| "response".to_string()),
                live,
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
    }
    replace_settings_category(tx, SyncCategory::ProxySettings, value)?;
    Ok(CategoryApplyReport {
        item_count: rows.len() as u64,
        ..Default::default()
    })
}

fn export_model_pricing() -> Result<(Value, u64), AppError> {
    let path = crate::services::model_pricing::model_pricing_file_path();
    let file = if path.exists() {
        let raw = std::fs::read_to_string(&path).map_err(|e| AppError::io(&path, e))?;
        serde_json::from_str::<Value>(&raw).unwrap_or(json!({
            "version": 1,
            "models": [],
            "deletedModelIds": []
        }))
    } else {
        json!({
            "version": 1,
            "modelsDevSync": {},
            "models": [],
            "deletedModelIds": []
        })
    };
    Ok((json!({ "file": file }), 1))
}

fn apply_model_pricing_in_transaction(
    tx: &Transaction<'_>,
    artifact: &CategoryArtifact,
) -> Result<CategoryApplyReport, AppError> {
    validate_artifact(SyncCategory::ModelPricing, &artifact.bytes)?;
    let value: Value = serde_json::from_slice(&artifact.bytes).map_err(|e| AppError::Json {
        path: "model_pricing".to_string(),
        source: e,
    })?;
    crate::services::model_pricing::apply_pricing_file_value_in_transaction(
        tx,
        value.get("file").cloned().unwrap_or(Value::Null),
    )?;
    Ok(CategoryApplyReport {
        item_count: 1,
        ..Default::default()
    })
}

pub fn model_pricing_file_bytes_from_artifact(artifact: &CategoryArtifact) -> Result<Vec<u8>, AppError> {
    let value: Value = serde_json::from_slice(&artifact.bytes).map_err(|e| AppError::Json {
        path: "model_pricing".to_string(),
        source: e,
    })?;
    let file = value.get("file").cloned().unwrap_or(json!({
        "version": 1,
        "models": [],
        "deletedModelIds": []
    }));
    let mut bytes = serde_json::to_vec_pretty(&file).map_err(|e| AppError::JsonSerialize { source: e })?;
    bytes.push(b'\n');
    Ok(bytes)
}

// ─── Helpers ─────────────────────────────────────────────────

fn parse_json_or_null(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or(Value::Null)
}

fn required_str<'a>(value: &'a Value, key: &str) -> Result<&'a str, AppError> {
    value.get(key).and_then(|v| v.as_str()).ok_or_else(|| {
        localized(
            "sync.artifact.missing_field",
            format!("缺少字段 {key}"),
            format!("Missing field {key}"),
        )
    })
}

fn opt_str(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(|v| {
        if v.is_null() {
            None
        } else {
            v.as_str().map(|s| s.to_string())
        }
    })
}

fn opt_i64(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(|v| v.as_i64())
}

fn opt_bool(value: &Value, key: &str) -> bool {
    value.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

fn bool_as_int(value: bool) -> i64 {
    if value {
        1
    } else {
        0
    }
}

fn json_field_as_text(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(other) => serde_json::to_string(other).unwrap_or_else(|_| "{}".to_string()),
        None => "{}".to_string(),
    }
}

fn upsert_setting(tx: &Transaction<'_>, key: &str, value: &str) -> Result<(), AppError> {
    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
        params![key, value],
    )
    .map_err(|e| AppError::Database(e.to_string()))?;
    Ok(())
}

fn delete_settings_matching<F>(tx: &Transaction<'_>, predicate: F) -> Result<(), AppError>
where
    F: Fn(&str) -> bool,
{
    let mut keys = Vec::new();
    {
        let mut stmt = tx
            .prepare("SELECT key FROM settings")
            .map_err(|e| AppError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| AppError::Database(e.to_string()))?;
        for row in rows {
            let key = row.map_err(|e| AppError::Database(e.to_string()))?;
            if predicate(&key) {
                keys.push(key);
            }
        }
    }
    for key in keys {
        tx.execute("DELETE FROM settings WHERE key = ?1", params![key])
            .map_err(|e| AppError::Database(e.to_string()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Database;

    fn memory_db() -> Database {
        Database::memory().expect("memory db")
    }

    #[test]
    fn providers_roundtrip_empty_and_seed_flag() {
        let db = memory_db();
        db.set_setting("official_providers_seeded", "true").unwrap();
        let artifact = export_category(&db, SyncCategory::Providers).unwrap();
        validate_artifact(SyncCategory::Providers, &artifact.bytes).unwrap();

        db.set_setting("official_providers_seeded", "false").unwrap();
        {
            let mut conn = db.conn.lock().unwrap();
            let tx = conn.transaction().unwrap();
            let ctx = ApplyContext::new(CloudSyncSelection::default(), true);
            replace_db_category(&tx, &artifact, &ctx).unwrap();
            tx.commit().unwrap();
        }
        assert!(db.get_bool_flag("official_providers_seeded").unwrap());
    }

    #[test]
    fn unknown_settings_keys_are_not_exported_in_common_config() {
        let db = memory_db();
        db.set_setting("common_config_claude", "snippet").unwrap();
        db.set_setting("totally_local_only", "secret").unwrap();
        let artifact = export_category(&db, SyncCategory::CommonConfig).unwrap();
        let value: Value = serde_json::from_slice(&artifact.bytes).unwrap();
        let entries = value.get("entries").unwrap().as_object().unwrap();
        assert!(entries.contains_key("common_config_claude"));
        assert!(!entries.contains_key("totally_local_only"));
    }

    #[test]
    fn skill_metadata_e_only_does_not_create_missing_skills() {
        let db = memory_db();
        db.save_skill(&crate::app_config::InstalledSkill {
            id: "local-one".to_string(),
            name: "Local".to_string(),
            description: Some("desc".to_string()),
            directory: "dir".to_string(),
            repo_owner: Some("old".to_string()),
            repo_name: Some("repo".to_string()),
            repo_branch: Some("main".to_string()),
            readme_url: None,
            apps: crate::app_config::SkillApps {
                claude: false,
                codex: false,
                gemini: false,
                grokbuild: false,
                opencode: false,
                hermes: false,
                pi: false,
            },
            installed_at: 10,
            content_hash: Some("hash".to_string()),
            updated_at: 10,
        })
        .unwrap();

        let remote = json!({
            "schemaVersion": 1,
            "skills": [
                {
                    "id": "local-one",
                    "name": "RemoteName",
                    "description": "remote-desc",
                    "directory": "remote-dir",
                    "repoOwner": "new",
                    "repoName": "new-repo",
                    "repoBranch": "dev",
                    "enabledClaude": true,
                    "installedAt": 99,
                    "contentHash": "remote-hash",
                    "updatedAt": 99
                },
                {
                    "id": "remote-only",
                    "name": "Missing",
                    "directory": "x"
                }
            ]
        });
        let bytes = serde_json::to_vec(&remote).unwrap();
        let artifact = CategoryArtifact::from_bytes(SyncCategory::SkillMetadata, 1, bytes, 2);
        let report = {
            let mut conn = db.conn.lock().unwrap();
            let tx = conn.transaction().unwrap();
            let ctx = ApplyContext::new(CloudSyncSelection::default(), false);
            let report = replace_db_category(&tx, &artifact, &ctx).unwrap();
            tx.commit().unwrap();
            report
        };

        assert_eq!(report.skipped, 1);
        let skills = db.get_all_installed_skills().unwrap();
        assert_eq!(skills.len(), 1);
        let local = skills.get("local-one").unwrap();
        assert_eq!(local.directory, "dir");
        assert_eq!(local.content_hash.as_deref(), Some("hash"));
        assert_eq!(local.installed_at, 10);
        assert_eq!(local.name, "Local");
        assert_eq!(local.repo_owner.as_deref(), Some("new"));
        assert!(local.apps.claude);
        assert!(!skills.contains_key("remote-only"));
    }

    #[test]
    fn empty_skill_repos_keeps_initialized_flag() {
        let db = memory_db();
        let artifact = export_category(&db, SyncCategory::SkillRepos).unwrap();
        {
            let mut conn = db.conn.lock().unwrap();
            let tx = conn.transaction().unwrap();
            let ctx = ApplyContext::new(CloudSyncSelection::default(), false);
            replace_db_category(&tx, &artifact, &ctx).unwrap();
            tx.commit().unwrap();
        }
        assert!(db.get_bool_flag("default_skill_repos_initialized").unwrap());
    }

    #[test]
    fn selective_restore_leaves_unselected_prompts_untouched() {
        let db = memory_db();
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
        db.set_setting("common_config_claude", "local-snippet")
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
        let mut artifacts = BTreeMap::new();
        artifacts.insert(SyncCategory::Prompts, remote_prompts);
        let mut selection = CloudSyncSelection::default();
        selection.set_enabled(SyncCategory::CommonConfig, false);
        let ctx = ApplyContext::new(selection, false);
        {
            let mut conn = db.conn.lock().unwrap();
            let tx = conn.transaction().unwrap();
            apply_categories_in_transaction(&tx, &artifacts, &ctx).unwrap();
            tx.commit().unwrap();
        }
        let prompts = db.get_prompts("claude").unwrap();
        assert!(prompts.contains_key("remote"));
        assert!(!prompts.contains_key("keep-me"));
        assert_eq!(
            db.get_setting("common_config_claude").unwrap().as_deref(),
            Some("local-snippet")
        );
    }

    #[test]
    fn profile_payload_keeps_local_provider_slot_when_a_disabled() {
        let db = memory_db();
        db.save_profile(&crate::database::Profile {
            id: "p1".to_string(),
            name: "Project".to_string(),
            payload: r#"{"providers":{"claude":"local-provider"},"mcp":{"claude":["local-mcp"]}}"#
                .to_string(),
            sort_order: Some(1),
            created_at: Some(1),
            updated_at: Some(1),
        })
        .unwrap();
        let remote = CategoryArtifact::from_bytes(
            SyncCategory::Profiles,
            1,
            serde_json::to_vec(&serde_json::json!({
                "schemaVersion": 1,
                "profiles": [{
                    "id": "p1",
                    "name": "Project",
                    "payload": {
                        "providers": { "claude": "remote-provider" },
                        "mcp": { "claude": ["remote-mcp"] }
                    }
                }],
                "currentProfileIds": {}
            }))
            .unwrap(),
            1,
        );
        let mut selection = CloudSyncSelection::default();
        selection.set_enabled(SyncCategory::Providers, false);
        selection.set_enabled(SyncCategory::Mcp, true);
        let ctx = ApplyContext::new(selection, false);
        {
            let mut conn = db.conn.lock().unwrap();
            let tx = conn.transaction().unwrap();
            replace_db_category(&tx, &remote, &ctx).unwrap();
            tx.commit().unwrap();
        }
        let profile = db.get_profile("p1").unwrap().unwrap();
        let payload: serde_json::Value = serde_json::from_str(&profile.payload).unwrap();
        assert_eq!(payload["providers"]["claude"], "local-provider");
        assert_eq!(payload["mcp"]["claude"], serde_json::json!([]));
    }

    #[test]
    fn v2_sql_isolated_extract_does_not_touch_live_db() {
        let live = memory_db();
        live.set_setting("common_config_claude", "live").unwrap();
        let source = memory_db();
        source.set_setting("common_config_claude", "from-v2").unwrap();
        let sql = source.export_sql_string_for_sync().unwrap();
        let isolated = crate::database::Database::from_sync_sql_export(&sql).unwrap();
        let artifact = export_category(&isolated, SyncCategory::CommonConfig).unwrap();
        let value: Value = serde_json::from_slice(&artifact.bytes).unwrap();
        assert_eq!(
            value["entries"]["common_config_claude"],
            "from-v2"
        );
        assert_eq!(
            live.get_setting("common_config_claude").unwrap().as_deref(),
            Some("live")
        );
    }

    #[test]
    fn db_apply_failure_rolls_back_transaction() {
        let db = memory_db();
        db.set_setting("common_config_claude", "keep").unwrap();
        let bad = CategoryArtifact::from_bytes(
            SyncCategory::CommonConfig,
            1,
            b"not-json".to_vec(),
            0,
        );
        let mut artifacts = BTreeMap::new();
        artifacts.insert(SyncCategory::CommonConfig, bad);
        let ctx = ApplyContext::new(CloudSyncSelection::default(), false);
        let err = {
            let mut conn = db.conn.lock().unwrap();
            let tx = conn.transaction().unwrap();
            let result = apply_categories_in_transaction(&tx, &artifacts, &ctx);
            assert!(result.is_err());
            // drop tx without commit
            result.err()
        };
        assert!(err.is_some());
        assert_eq!(
            db.get_setting("common_config_claude").unwrap().as_deref(),
            Some("keep")
        );
    }
}

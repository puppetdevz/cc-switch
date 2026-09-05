use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::database::{lock_conn, Database};
use crate::error::AppError;
use crate::services::skill::SkillService;
use crate::services::sync_categories::{
    classify_settings_key, SettingsKeyClass, SyncCategory,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalCategoryStats {
    pub category: SyncCategory,
    pub item_count: u64,
    pub bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncompressed_bytes: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct SkillFilesListing {
    pub file_count: u64,
    pub uncompressed_bytes: u64,
    pub fingerprint: String,
}

pub fn collect_local_category_stats(db: &Database) -> Result<Vec<LocalCategoryStats>, AppError> {
    let conn = lock_conn!(db.conn);
    let mut stats = Vec::new();
    for category in SyncCategory::ALL {
        stats.push(stats_for_category(&conn, category)?);
    }
    Ok(stats)
}

fn stats_for_category(conn: &Connection, category: SyncCategory) -> Result<LocalCategoryStats, AppError> {
    match category {
        SyncCategory::Providers => {
            let count = count_sql(conn, "SELECT COUNT(*) FROM providers")?;
            Ok(LocalCategoryStats {
                category,
                item_count: count,
                bytes: 0,
                file_count: None,
                uncompressed_bytes: None,
            })
        }
        SyncCategory::Mcp => Ok(LocalCategoryStats {
            category,
            item_count: count_sql(conn, "SELECT COUNT(*) FROM mcp_servers")?,
            bytes: 0,
            file_count: None,
            uncompressed_bytes: None,
        }),
        SyncCategory::Prompts => Ok(LocalCategoryStats {
            category,
            item_count: count_sql(conn, "SELECT COUNT(*) FROM prompts")?,
            bytes: 0,
            file_count: None,
            uncompressed_bytes: None,
        }),
        SyncCategory::SkillRepos => Ok(LocalCategoryStats {
            category,
            item_count: count_sql(conn, "SELECT COUNT(*) FROM skill_repos")?,
            bytes: 0,
            file_count: None,
            uncompressed_bytes: None,
        }),
        SyncCategory::SkillMetadata => Ok(LocalCategoryStats {
            category,
            item_count: count_sql(conn, "SELECT COUNT(*) FROM skills")?,
            bytes: 0,
            file_count: None,
            uncompressed_bytes: None,
        }),
        SyncCategory::SkillFiles => {
            let listing = skill_files_listing_stats()?;
            Ok(LocalCategoryStats {
                category,
                item_count: listing.file_count,
                bytes: listing.uncompressed_bytes,
                file_count: Some(listing.file_count),
                uncompressed_bytes: Some(listing.uncompressed_bytes),
            })
        }
        SyncCategory::Profiles => Ok(LocalCategoryStats {
            category,
            item_count: count_sql(conn, "SELECT COUNT(*) FROM profiles")?,
            bytes: 0,
            file_count: None,
            uncompressed_bytes: None,
        }),
        SyncCategory::CommonConfig => Ok(LocalCategoryStats {
            category,
            item_count: count_settings(conn, SyncCategory::CommonConfig)?,
            bytes: 0,
            file_count: None,
            uncompressed_bytes: None,
        }),
        SyncCategory::ProxySettings => {
            let proxy_rows = count_sql(conn, "SELECT COUNT(*) FROM proxy_config")?;
            let settings_rows = count_settings(conn, SyncCategory::ProxySettings)?;
            Ok(LocalCategoryStats {
                category,
                item_count: proxy_rows + settings_rows,
                bytes: 0,
                file_count: None,
                uncompressed_bytes: None,
            })
        }
        SyncCategory::DiagnosticsSettings => Ok(LocalCategoryStats {
            category,
            item_count: count_settings(conn, SyncCategory::DiagnosticsSettings)?,
            bytes: 0,
            file_count: None,
            uncompressed_bytes: None,
        }),
        SyncCategory::ModelPricing => {
            let path = crate::services::model_pricing::model_pricing_file_path();
            let bytes = if path.exists() {
                fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
            } else {
                0
            };
            Ok(LocalCategoryStats {
                category,
                item_count: if path.exists() { 1 } else { 0 },
                bytes,
                file_count: None,
                uncompressed_bytes: None,
            })
        }
    }
}

fn count_sql(conn: &Connection, sql: &str) -> Result<u64, AppError> {
    let count: i64 = conn
        .query_row(sql, [], |row| row.get(0))
        .map_err(|e| AppError::Database(e.to_string()))?;
    Ok(count.max(0) as u64)
}

fn count_settings(conn: &Connection, category: SyncCategory) -> Result<u64, AppError> {
    let mut stmt = conn
        .prepare("SELECT key FROM settings")
        .map_err(|e| AppError::Database(e.to_string()))?;
    let keys = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| AppError::Database(e.to_string()))?;
    let mut count = 0u64;
    for key in keys {
        let key = key.map_err(|e| AppError::Database(e.to_string()))?;
        if matches!(
            classify_settings_key(&key),
            SettingsKeyClass::Category(mapped) if mapped == category
        ) {
            count += 1;
        }
    }
    Ok(count)
}

pub fn skill_files_listing_stats() -> Result<SkillFilesListing, AppError> {
    let source = SkillService::get_ssot_dir().map_err(|e| {
        AppError::localized(
            "sync.skills_ssot_dir_failed",
            format!("获取 Skills SSOT 目录失败: {e}"),
            format!("Failed to resolve Skills SSOT directory: {e}"),
        )
    })?;
    if !source.exists() {
        return Ok(SkillFilesListing::default());
    }

    let canonical_root = fs::canonicalize(&source).unwrap_or(source.clone());
    let mut visited = HashSet::new();
    let mut files: Vec<(String, u64, i64)> = Vec::new();
    walk_files(&canonical_root, &canonical_root, &mut visited, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut uncompressed_bytes = 0u64;
    let mut fingerprint_parts = Vec::new();
    for (rel, size, mtime) in &files {
        uncompressed_bytes = uncompressed_bytes.saturating_add(*size);
        fingerprint_parts.push(format!("{rel}:{size}:{mtime}"));
    }
    Ok(SkillFilesListing {
        file_count: files.len() as u64,
        uncompressed_bytes,
        fingerprint: crate::services::sync_protocol::sha256_hex(fingerprint_parts.join("|").as_bytes()),
    })
}

fn walk_files(
    root: &Path,
    current: &Path,
    visited: &mut HashSet<PathBuf>,
    files: &mut Vec<(String, u64, i64)>,
) -> Result<(), AppError> {
    if !visited.insert(current.to_path_buf()) {
        return Ok(());
    }
    let mut entries: Vec<_> = match fs::read_dir(current) {
        Ok(rd) => rd.collect::<Result<Vec<_>, _>>().map_err(|e| AppError::io(current, e))?,
        Err(e) => return Err(AppError::io(current, e)),
    };
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            walk_files(root, &path, visited, files)?;
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        files.push((rel, meta.len(), mtime));
    }
    Ok(())
}

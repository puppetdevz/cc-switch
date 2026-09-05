use indexmap::IndexMap;
use std::collections::HashSet;
use std::path::Path;

use crate::app_config::AppType;
use crate::config::write_text_file;
use crate::error::AppError;
use crate::prompt::Prompt;
use crate::prompt_files::prompt_file_path;
use crate::services::pi_prompt_files::PiAgentsFileGuard;
use crate::store::AppState;

/// 安全地获取当前 Unix 时间戳
fn get_unix_timestamp() -> Result<i64, AppError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .map_err(|e| AppError::Message(format!("Failed to get system time: {e}")))
}

pub struct PromptService;

pub(crate) fn compose_prompt_content<'a, I>(enabled: I) -> String
where
    I: IntoIterator<Item = &'a Prompt>,
{
    enabled
        .into_iter()
        .map(|prompt| prompt.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub(crate) fn compose_legacy_prompt_content<'a, I>(enabled: I) -> String
where
    I: IntoIterator<Item = &'a Prompt>,
{
    enabled
        .into_iter()
        .map(|prompt| format!("## {}\n\n{}", prompt.name, prompt.content))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn normalize_prompt_file(content: &str) -> String {
    content.replace("\r\n", "\n").trim_end().to_string()
}

fn enabled_prompts(prompts: &IndexMap<String, Prompt>) -> Vec<&Prompt> {
    prompts.values().filter(|prompt| prompt.enabled).collect()
}

fn live_matches_applied(live: &str, enabled: &[&Prompt]) -> bool {
    let live_n = normalize_prompt_file(live);
    live_n == normalize_prompt_file(&compose_prompt_content(enabled.iter().copied()))
        || live_n == normalize_prompt_file(&compose_legacy_prompt_content(enabled.iter().copied()))
}

fn next_sort_order(prompts: &IndexMap<String, Prompt>) -> i64 {
    prompts
        .values()
        .filter_map(|prompt| prompt.sort_order)
        .max()
        .map(|value| value + 1)
        .unwrap_or(0)
}

fn unique_backup_id(prompts: &IndexMap<String, Prompt>, timestamp: i64) -> String {
    let base = format!("backup-{timestamp}");
    if !prompts.contains_key(&base) {
        return base;
    }
    for suffix in 2_u64.. {
        let candidate = format!("{base}-{suffix}");
        if !prompts.contains_key(&candidate) {
            return candidate;
        }
    }
    unreachable!("the backup suffix space is finite only after u64 exhaustion")
}

fn build_backup_prompt(
    prompts: &IndexMap<String, Prompt>,
    content: String,
    timestamp: i64,
) -> Result<Prompt, AppError> {
    Ok(Prompt {
        id: unique_backup_id(prompts, timestamp),
        name: format!(
            "原始提示词 {}",
            chrono::Local::now().format("%Y-%m-%d %H:%M")
        ),
        content,
        description: Some("自动备份的原始提示词".to_string()),
        enabled: false,
        created_at: Some(timestamp),
        updated_at: Some(timestamp),
        sort_order: Some(next_sort_order(prompts)),
    })
}

fn read_non_pi_live_content(app: &AppType) -> Result<Option<String>, AppError> {
    let path = prompt_file_path(app)?;
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path).map_err(|e| AppError::io(&path, e))?;
    Ok(Some(content))
}

fn project_prompt_set_to_path(
    prompts: &IndexMap<String, Prompt>,
    target_path: &Path,
) -> Result<Option<String>, AppError> {
    let enabled = enabled_prompts(prompts);

    if enabled.is_empty() {
        // With nothing enabled, leave the target file untouched. This projection
        // only runs after a database restore, and the live file is not part of
        // the sync payload — clearing it here would wipe local content the
        // restored snapshot never contained. Disabling the last prompt from the
        // UI still clears the file via `PromptService::apply_prompts`.
        return Ok(None);
    }

    write_text_file(target_path, &compose_prompt_content(enabled))?;
    Ok(None)
}

fn restore_prompt_set(
    state: &AppState,
    app: &AppType,
    previous: &IndexMap<String, Prompt>,
    current: &IndexMap<String, Prompt>,
) -> Result<(), AppError> {
    for id in current.keys() {
        if !previous.contains_key(id) {
            state.db.delete_prompt(app.as_str(), id)?;
        }
    }
    for prompt in previous.values() {
        state.db.save_prompt(app.as_str(), prompt)?;
    }
    Ok(())
}

fn write_applied_prompts(
    app: &AppType,
    prompts: &IndexMap<String, Prompt>,
    pi_guard: Option<&PiAgentsFileGuard>,
    pi_revision: Option<&str>,
) -> Result<(), AppError> {
    let enabled = enabled_prompts(prompts);
    let content = compose_prompt_content(enabled.iter().copied());

    if matches!(app, AppType::Pi) {
        let guard = pi_guard.ok_or_else(|| {
            AppError::Message("Pi AGENTS.md write requires an acquired file guard".to_string())
        })?;
        let revision = pi_revision.ok_or_else(|| {
            AppError::Message("Pi AGENTS.md write requires a file revision".to_string())
        })?;
        if enabled.is_empty() {
            guard.delete(revision)?;
        } else {
            guard.replace(revision, &content)?;
        }
        return Ok(());
    }

    let target_path = prompt_file_path(app)?;
    if enabled.is_empty() {
        if target_path.exists() {
            write_text_file(&target_path, "")?;
        }
    } else {
        write_text_file(&target_path, &content)?;
    }
    Ok(())
}

fn projection_changed(
    previous: &IndexMap<String, Prompt>,
    next: &IndexMap<String, Prompt>,
) -> bool {
    let previous_ids: Vec<&str> = previous
        .values()
        .filter(|prompt| prompt.enabled)
        .map(|prompt| prompt.id.as_str())
        .collect();
    let next_ids: Vec<&str> = next
        .values()
        .filter(|prompt| prompt.enabled)
        .map(|prompt| prompt.id.as_str())
        .collect();
    previous_ids != next_ids
        || compose_prompt_content(enabled_prompts(previous))
            != compose_prompt_content(enabled_prompts(next))
}

/// 写盘前若 live 相对「写之前的已应用集合」发生外部漂移，则构造未启用备份块（尚未入库）。
fn build_live_drift_backup(
    applied_before: &IndexMap<String, Prompt>,
    live_content: Option<&str>,
) -> Result<Option<Prompt>, AppError> {
    let Some(live) = live_content.filter(|content| !content.trim().is_empty()) else {
        return Ok(None);
    };
    let enabled = enabled_prompts(applied_before);
    if live_matches_applied(live, &enabled) {
        return Ok(None);
    }

    let timestamp = get_unix_timestamp()?;
    let backup = build_backup_prompt(applied_before, live.to_string(), timestamp)?;
    log::info!("检测到提示词文件外部修改，将备份为 {}", backup.id);
    Ok(Some(backup))
}

fn save_prompt_set(
    state: &AppState,
    app: &AppType,
    previous: &IndexMap<String, Prompt>,
    next: &IndexMap<String, Prompt>,
) -> Result<(), AppError> {
    for prompt in next.values() {
        state.db.save_prompt(app.as_str(), prompt)?;
    }
    for id in previous.keys() {
        if !next.contains_key(id) {
            state.db.delete_prompt(app.as_str(), id)?;
        }
    }
    Ok(())
}

fn persist_applied_prompts(
    state: &AppState,
    app: AppType,
    previous: &IndexMap<String, Prompt>,
    next: &IndexMap<String, Prompt>,
    force_write: bool,
) -> Result<(), AppError> {
    let mut next = next.clone();
    let project = force_write || projection_changed(previous, &next);

    if !project {
        save_prompt_set(state, &app, previous, &next)?;
        return Ok(());
    }

    let write_result = if matches!(app, AppType::Pi) {
        let guard = PiAgentsFileGuard::acquire()?;
        let snapshot = guard.read()?;
        if let Some(backup) = build_live_drift_backup(previous, snapshot.content.as_deref())? {
            next.insert(backup.id.clone(), backup);
        }
        save_prompt_set(state, &app, previous, &next)?;
        write_applied_prompts(&app, &next, Some(&guard), Some(&snapshot.revision))
    } else {
        let live = read_non_pi_live_content(&app)?;
        if let Some(backup) = build_live_drift_backup(previous, live.as_deref())? {
            next.insert(backup.id.clone(), backup);
        }
        save_prompt_set(state, &app, previous, &next)?;
        write_applied_prompts(&app, &next, None, None)
    };

    if let Err(error) = write_result {
        if let Err(rollback_error) = restore_prompt_set(state, &app, previous, &next) {
            return Err(AppError::Message(format!(
                "Prompt file update failed ({error}); database rollback also failed: {rollback_error}"
            )));
        }
        return Err(error);
    }

    Ok(())
}

fn assign_missing_sort_order(prompt: &mut Prompt, prompts: &IndexMap<String, Prompt>) {
    if prompt.sort_order.is_none() {
        if let Some(existing) = prompts.get(&prompt.id) {
            prompt.sort_order = existing.sort_order;
        } else {
            prompt.sort_order = Some(next_sort_order(prompts));
        }
    }
}

impl PromptService {
    pub fn get_prompts(
        state: &AppState,
        app: AppType,
    ) -> Result<IndexMap<String, Prompt>, AppError> {
        state.db.get_prompts(app.as_str())
    }

    /// 将所有已启用的提示词按 sort_order 裸拼接写入文件。
    pub fn sync_merged_prompts_to_file(state: &AppState, app: AppType) -> Result<(), AppError> {
        let prompts = state.db.get_prompts(app.as_str())?;
        persist_applied_prompts(state, app, &prompts, &prompts, true)
    }

    pub fn upsert_prompt(
        state: &AppState,
        app: AppType,
        id: &str,
        mut prompt: Prompt,
    ) -> Result<(), AppError> {
        if prompt.id != id {
            return Err(AppError::InvalidInput(
                "Prompt id does not match the requested id".to_string(),
            ));
        }

        let previous = state.db.get_prompts(app.as_str())?;
        assign_missing_sort_order(&mut prompt, &previous);

        let mut next = previous.clone();
        next.insert(id.to_string(), prompt);
        persist_applied_prompts(state, app, &previous, &next, false)
    }

    pub fn delete_prompt(state: &AppState, app: AppType, id: &str) -> Result<(), AppError> {
        let prompts = Self::get_prompts(state, app.clone())?;

        if let Some(prompt) = prompts.get(id) {
            if prompt.enabled {
                return Err(AppError::InvalidInput("无法删除已启用的提示词".to_string()));
            }
        }

        state.db.delete_prompt(app.as_str(), id)?;
        Ok(())
    }

    /// 将目标提示词加入已应用集合（不踢掉其它块）并立刻写盘。
    pub fn enable_prompt(state: &AppState, app: AppType, id: &str) -> Result<(), AppError> {
        let previous = state.db.get_prompts(app.as_str())?;
        let Some(target) = previous.get(id).cloned() else {
            return Err(AppError::InvalidInput(format!("提示词 {id} 不存在")));
        };

        let mut next = previous.clone();
        if !target.enabled {
            let mut enabled = target;
            enabled.enabled = true;
            next.insert(id.to_string(), enabled);
        }
        persist_applied_prompts(state, app, &previous, &next, true)
    }

    /// 按草稿的完整顺序和勾选集合应用到数据库并写盘。
    /// `ordered_ids` 中出现未知 id 则失败并保持原状。
    pub fn apply_prompts(
        state: &AppState,
        app: AppType,
        ordered_ids: Vec<String>,
        enabled_ids: Vec<String>,
    ) -> Result<(), AppError> {
        let previous = state.db.get_prompts(app.as_str())?;
        let mut seen = HashSet::new();
        for id in &ordered_ids {
            if !previous.contains_key(id) {
                return Err(AppError::InvalidInput(format!("提示词 {id} 不存在")));
            }
            if !seen.insert(id) {
                return Err(AppError::InvalidInput(format!(
                    "提示词 {id} 在顺序列表中重复"
                )));
            }
        }
        for id in &enabled_ids {
            if !previous.contains_key(id) {
                return Err(AppError::InvalidInput(format!("提示词 {id} 不存在")));
            }
        }

        let enabled_set: HashSet<&str> = enabled_ids.iter().map(String::as_str).collect();
        let mut next = IndexMap::new();
        for (index, id) in ordered_ids.iter().enumerate() {
            let Some(prompt) = previous.get(id) else {
                continue;
            };
            let mut prompt = prompt.clone();
            prompt.enabled = enabled_set.contains(id.as_str());
            prompt.sort_order = Some(index as i64);
            next.insert(id.clone(), prompt);
        }

        let mut trailing = previous
            .values()
            .filter(|prompt| !seen.contains(&prompt.id))
            .cloned()
            .collect::<Vec<_>>();
        trailing.sort_by_key(|prompt| {
            (
                prompt.sort_order.is_none(),
                prompt.sort_order.unwrap_or(0),
                prompt.created_at.unwrap_or(0),
                prompt.id.clone(),
            )
        });
        let mut sort_index = ordered_ids.len() as i64;
        for mut prompt in trailing {
            prompt.sort_order = Some(sort_index);
            sort_index += 1;
            next.insert(prompt.id.clone(), prompt);
        }

        persist_applied_prompts(state, app, &previous, &next, true)
    }

    pub fn import_from_file(state: &AppState, app: AppType) -> Result<String, AppError> {
        let content = if matches!(app, AppType::Pi) {
            PiAgentsFileGuard::acquire()?
                .read()?
                .content
                .ok_or_else(|| AppError::Message("提示词文件不存在".to_string()))?
        } else {
            let file_path = prompt_file_path(&app)?;
            if !file_path.exists() {
                return Err(AppError::Message("提示词文件不存在".to_string()));
            }
            std::fs::read_to_string(&file_path).map_err(|e| AppError::io(&file_path, e))?
        };
        let timestamp = get_unix_timestamp()?;
        let existing = state.db.get_prompts(app.as_str())?;

        let id = format!("imported-{timestamp}");
        let prompt = Prompt {
            id: id.clone(),
            name: format!(
                "导入的提示词 {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M")
            ),
            content,
            description: Some("从现有配置文件导入".to_string()),
            enabled: false,
            created_at: Some(timestamp),
            updated_at: Some(timestamp),
            sort_order: Some(next_sort_order(&existing)),
        };

        Self::upsert_prompt(state, app, &id, prompt)?;
        Ok(id)
    }

    pub fn get_current_file_content(app: AppType) -> Result<Option<String>, AppError> {
        if matches!(app, AppType::Pi) {
            return Ok(PiAgentsFileGuard::acquire()?.read()?.content);
        }
        read_non_pi_live_content(&app)
    }

    /// Project the database SSOT to one application's managed prompt file.
    ///
    /// This deliberately does not call `enable_prompt`: restore paths must not
    /// read stale live content and write it back into the freshly imported DB.
    pub fn sync_to_live(state: &AppState, app: AppType) -> Result<(), AppError> {
        if matches!(app, AppType::ClaudeDesktop) {
            return Ok(());
        }

        let prompts = state.db.get_prompts(app.as_str())?;
        if matches!(app, AppType::Pi) {
            if enabled_prompts(&prompts).is_empty() {
                return Ok(());
            }
            let guard = PiAgentsFileGuard::acquire()?;
            let snapshot = guard.read()?;
            write_applied_prompts(&app, &prompts, Some(&guard), Some(&snapshot.revision))?;
            return Ok(());
        }

        let target_path = prompt_file_path(&app)?;
        if let Some(warning) = project_prompt_set_to_path(&prompts, &target_path)? {
            return Err(AppError::Message(warning));
        }
        Ok(())
    }

    /// Best-effort projection for every Prompt-capable application.
    pub fn sync_all_to_live(state: &AppState) -> Result<(), AppError> {
        let mut failures = Vec::new();
        for app in AppType::all() {
            if matches!(app, AppType::ClaudeDesktop) {
                continue;
            }
            if let Err(error) = Self::sync_to_live(state, app.clone()) {
                log::warn!("同步 Prompt 到 {app:?} 失败: {error}");
                failures.push(format!("{}: {error}", app.as_str()));
            }
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(AppError::Message(format!(
                "部分应用 Prompt 同步失败: {}",
                failures.join("; ")
            )))
        }
    }

    /// 首次启动时从现有提示词文件自动导入（如果存在）
    /// 返回导入的数量
    pub fn import_from_file_on_first_launch(
        state: &AppState,
        app: AppType,
    ) -> Result<usize, AppError> {
        // 幂等性保护：该应用已有提示词则跳过
        let existing = state.db.get_prompts(app.as_str())?;
        if !existing.is_empty() {
            return Ok(0);
        }

        let file_path = prompt_file_path(&app)?;

        // 读取文件内容。Pi 与交互式管理路径共用限长读取和协调锁。
        let content = if matches!(app, AppType::Pi) {
            match PiAgentsFileGuard::acquire().and_then(|guard| guard.read()) {
                Ok(snapshot) => match snapshot.content {
                    Some(content) => content,
                    None => return Ok(0),
                },
                Err(error) => {
                    log::warn!("读取提示词文件失败: {file_path:?}, 错误: {error}");
                    return Ok(0);
                }
            }
        } else {
            if !file_path.exists() {
                return Ok(0);
            }
            match std::fs::read_to_string(&file_path) {
                Ok(content) => content,
                Err(error) => {
                    log::warn!("读取提示词文件失败: {file_path:?}, 错误: {error}");
                    return Ok(0);
                }
            }
        };

        // 检查内容是否为空
        if content.trim().is_empty() {
            return Ok(0);
        }

        log::info!("发现提示词文件，自动导入: {file_path:?}");

        let timestamp = get_unix_timestamp()?;
        let id = format!("auto-imported-{timestamp}");
        let prompt = Prompt {
            id: id.clone(),
            name: format!(
                "Auto-imported Prompt {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M")
            ),
            content,
            description: Some("Automatically imported on first launch".to_string()),
            enabled: true,
            created_at: Some(timestamp),
            updated_at: Some(timestamp),
            sort_order: Some(0),
        };

        state.db.save_prompt(app.as_str(), &prompt)?;

        log::info!("自动导入完成: {}", app.as_str());
        Ok(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn prompt(id: &str, content: &str, enabled: bool) -> Prompt {
        Prompt {
            id: id.to_string(),
            name: id.to_string(),
            content: content.to_string(),
            description: None,
            enabled,
            created_at: None,
            updated_at: None,
            sort_order: None,
        }
    }

    #[test]
    fn compose_joins_enabled_contents_without_headings() {
        let first = prompt("first", "alpha", true);
        let second = prompt("second", "beta", true);
        assert_eq!(compose_prompt_content([&first, &second]), "alpha\n\nbeta");
        assert_eq!(
            compose_legacy_prompt_content([&first, &second]),
            "## first\n\nalpha\n\n## second\n\nbeta"
        );
    }

    #[test]
    fn live_matches_both_current_and_legacy_formats() {
        let first = prompt("first", "alpha", true);
        let second = prompt("second", "beta", true);
        let enabled = vec![&first, &second];
        assert!(live_matches_applied("alpha\n\nbeta", &enabled));
        assert!(live_matches_applied(
            "## first\n\nalpha\n\n## second\n\nbeta",
            &enabled
        ));
        assert!(!live_matches_applied("external edit", &enabled));
    }

    #[test]
    fn restored_prompt_projection_writes_the_enabled_content() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("AGENTS.md");
        let mut prompts = IndexMap::new();
        prompts.insert("off".to_string(), prompt("off", "old", false));
        prompts.insert("on".to_string(), prompt("on", "restored", true));

        let warning = project_prompt_set_to_path(&prompts, &path).expect("project prompt");
        assert!(warning.is_none());
        assert_eq!(
            std::fs::read_to_string(path).expect("read prompt"),
            "restored"
        );
    }

    #[test]
    fn restored_prompt_projection_preserves_the_live_file_when_none_are_enabled() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("AGENTS.md");
        std::fs::write(&path, "local content").expect("seed live prompt file");
        let mut prompts = IndexMap::new();
        prompts.insert("off".to_string(), prompt("off", "managed", false));

        let warning = project_prompt_set_to_path(&prompts, &path).expect("project prompt");
        assert!(warning.is_none());
        assert_eq!(
            std::fs::read_to_string(path).expect("read prompt"),
            "local content"
        );
    }

    #[test]
    fn restored_prompt_projection_merges_all_enabled_prompts() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("AGENTS.md");
        let mut prompts = IndexMap::new();
        prompts.insert("first".to_string(), prompt("first", "first body", true));
        prompts.insert("second".to_string(), prompt("second", "second body", true));

        let warning = project_prompt_set_to_path(&prompts, &path).expect("project prompt");
        assert!(warning.is_none());
        assert_eq!(
            std::fs::read_to_string(path).expect("read prompt"),
            "first body\n\nsecond body"
        );
    }
}

#[cfg(test)]
mod apply_prompt_tests {
    use super::*;
    use crate::database::Database;
    use std::sync::Arc;

    fn prompt(id: &str, content: &str, enabled: bool, sort_order: i64) -> Prompt {
        Prompt {
            id: id.to_string(),
            name: id.to_string(),
            content: content.to_string(),
            description: None,
            enabled,
            created_at: Some(1),
            updated_at: Some(1),
            sort_order: Some(sort_order),
        }
    }

    fn memory_state() -> AppState {
        AppState::new(Arc::new(
            Database::memory().expect("create in-memory database"),
        ))
    }

    #[test]
    fn apply_prompts_rejects_unknown_ids_without_writing() {
        let state = memory_state();
        state
            .db
            .save_prompt("claude", &prompt("keep", "keep", true, 0))
            .expect("save");

        let error = PromptService::apply_prompts(
            &state,
            AppType::Claude,
            vec!["missing".to_string()],
            vec!["missing".to_string()],
        )
        .expect_err("unknown id");
        assert!(error.to_string().contains("missing"));
        let saved = state.db.get_prompts("claude").expect("reload");
        assert!(saved["keep"].enabled);
    }
}

#[cfg(test)]
mod pi_prompt_tests {
    use super::*;
    use crate::database::Database;
    use crate::pi_config::test_support::TestAgentDir;
    use serial_test::serial;
    use std::sync::Arc;

    fn prompt(enabled: bool) -> Prompt {
        Prompt {
            id: "test-prompt".to_string(),
            name: "Test prompt".to_string(),
            content: "managed content".to_string(),
            description: None,
            enabled,
            created_at: Some(1),
            updated_at: Some(1),
            sort_order: Some(0),
        }
    }

    #[test]
    #[serial]
    fn pi_enabled_flag_comes_from_the_database() {
        let _agent = TestAgentDir::new();
        let state = AppState::new(Arc::new(
            Database::memory().expect("create in-memory database"),
        ));
        state
            .db
            .save_prompt(AppType::Pi.as_str(), &prompt(true))
            .expect("save prompt");

        let saved = PromptService::get_prompts(&state, AppType::Pi).expect("load prompts");
        assert!(saved["test-prompt"].enabled);

        let path = prompt_file_path(&AppType::Pi).expect("prompt path");
        write_text_file(&path, "external edit").expect("edit AGENTS.md externally");
        let drifted = PromptService::get_prompts(&state, AppType::Pi).expect("load prompts");
        assert!(drifted["test-prompt"].enabled);

        PromptService::enable_prompt(&state, AppType::Pi, "test-prompt").expect("rewrite file");
        let after = state
            .db
            .get_prompts(AppType::Pi.as_str())
            .expect("reload prompts");
        assert!(after.values().any(|item| item.id.starts_with("backup-")));
        assert_eq!(
            std::fs::read_to_string(&path).expect("read AGENTS.md"),
            "managed content"
        );
    }

    #[test]
    #[serial]
    fn applying_no_pi_prompts_deletes_agents_file() {
        let _agent = TestAgentDir::new();
        let state = AppState::new(Arc::new(
            Database::memory().expect("create in-memory database"),
        ));
        state
            .db
            .save_prompt(AppType::Pi.as_str(), &prompt(true))
            .expect("save prompt");
        let path = prompt_file_path(&AppType::Pi).expect("prompt path");
        write_text_file(&path, "managed content").expect("write AGENTS.md");

        PromptService::apply_prompts(
            &state,
            AppType::Pi,
            vec!["test-prompt".to_string()],
            Vec::new(),
        )
        .expect("apply empty");
        assert!(!path.exists());
    }

    #[test]
    #[serial]
    fn enable_prompt_keeps_other_pi_prompts_enabled() {
        let _agent = TestAgentDir::new();
        let state = AppState::new(Arc::new(
            Database::memory().expect("create in-memory database"),
        ));
        let first = prompt(true);
        let mut second = prompt(false);
        second.id = "second-prompt".to_string();
        second.name = "Second".to_string();
        second.content = "second body".to_string();
        second.sort_order = Some(1);
        state
            .db
            .save_prompt(AppType::Pi.as_str(), &first)
            .expect("save first");
        state
            .db
            .save_prompt(AppType::Pi.as_str(), &second)
            .expect("save second");
        let path = prompt_file_path(&AppType::Pi).expect("prompt path");
        write_text_file(&path, "managed content").expect("write AGENTS.md");

        PromptService::enable_prompt(&state, AppType::Pi, "second-prompt").expect("enable second");

        let saved = state.db.get_prompts(AppType::Pi.as_str()).expect("reload");
        assert!(saved["test-prompt"].enabled);
        assert!(saved["second-prompt"].enabled);
        assert_eq!(
            std::fs::read_to_string(path).expect("read AGENTS.md"),
            "managed content\n\nsecond body"
        );
    }

    #[test]
    #[serial]
    fn generic_prompt_projection_does_not_rewrite_pi_agents_file_when_none_enabled() {
        let _agent = TestAgentDir::new();
        let state = AppState::new(Arc::new(
            Database::memory().expect("create in-memory database"),
        ));
        state
            .db
            .save_prompt(AppType::Pi.as_str(), &prompt(false))
            .expect("save Pi prompt");

        let path = prompt_file_path(&AppType::Pi).expect("prompt path");
        write_text_file(&path, "native instructions").expect("write AGENTS.md");

        PromptService::sync_to_live(&state, AppType::Pi).expect("sync prompts");

        assert_eq!(
            std::fs::read_to_string(path).expect("read AGENTS.md"),
            "native instructions"
        );
    }

    #[test]
    #[serial]
    fn editing_an_inactive_duplicate_pi_prompt_preserves_agents_file() {
        let _agent = TestAgentDir::new();
        let state = AppState::new(Arc::new(
            Database::memory().expect("create in-memory database"),
        ));
        let first = prompt(true);
        let mut duplicate = first.clone();
        duplicate.id = "duplicate-prompt".to_string();
        duplicate.name = "Duplicate prompt".to_string();
        duplicate.enabled = false;
        duplicate.created_at = Some(2);
        duplicate.sort_order = Some(1);
        state
            .db
            .save_prompt(AppType::Pi.as_str(), &first)
            .expect("save first prompt");
        state
            .db
            .save_prompt(AppType::Pi.as_str(), &duplicate)
            .expect("save duplicate prompt");
        let path = prompt_file_path(&AppType::Pi).expect("prompt path");
        write_text_file(&path, "managed content").expect("write AGENTS.md");

        duplicate.content = "edited duplicate".to_string();
        PromptService::upsert_prompt(&state, AppType::Pi, "duplicate-prompt", duplicate)
            .expect("edit inactive duplicate");

        assert_eq!(
            std::fs::read_to_string(&path).expect("read AGENTS.md"),
            "managed content"
        );
        let refreshed = PromptService::get_prompts(&state, AppType::Pi).expect("reload prompts");
        assert!(refreshed["test-prompt"].enabled);
        assert!(!refreshed["duplicate-prompt"].enabled);
    }

    #[test]
    fn pi_backup_ids_do_not_replace_an_existing_same_second_backup() {
        let mut prompts = IndexMap::new();
        let mut first = prompt(false);
        first.id = "backup-42".to_string();
        prompts.insert(first.id.clone(), first);
        let mut second = prompt(false);
        second.id = "backup-42-2".to_string();
        prompts.insert(second.id.clone(), second);

        assert_eq!(unique_backup_id(&prompts, 42), "backup-42-3");
    }
}

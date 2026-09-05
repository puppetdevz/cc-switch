//! Explicit classification of every known `settings` table key.
//!
//! Unknown keys default to `LocalOnly` so exporting the whole settings table
//! can never accidentally sync device-local or one-shot flags.

use crate::app_config::AppType;
use crate::services::sync_categories::SyncCategory;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsKeyClass {
    Category(SyncCategory),
    LocalOnly,
}

pub const ALL_KNOWN_SETTINGS_KEYS: &[&str] = &[
    // A providers
    "official_providers_seeded",
    "universal_providers",
    // D skill repos
    "default_skill_repos_initialized",
    // G profiles (prefix + known scopes)
    "current_profile_id_claude",
    "current_profile_id_claude-desktop",
    "current_profile_id_codex",
    // H common config
    "common_config_claude",
    "common_config_claude-desktop",
    "common_config_codex",
    "common_config_gemini",
    "common_config_grokbuild",
    "common_config_opencode",
    "common_config_openclaw",
    "common_config_hermes",
    "common_config_pi",
    "common_config_claude_cleared",
    "common_config_claude-desktop_cleared",
    "common_config_codex_cleared",
    "common_config_gemini_cleared",
    "common_config_grokbuild_cleared",
    "common_config_opencode_cleared",
    "common_config_openclaw_cleared",
    "common_config_hermes_cleared",
    "common_config_pi_cleared",
    // I proxy / request
    "global_proxy_url",
    "rectifier_config",
    "optimizer_config",
    "copilot_optimizer_config",
    // J diagnostics
    "stream_check_config",
    "log_config",
    // Local-only / one-shot
    "common_config_legacy_migrated_v1",
    "skills_ssot_migration_pending",
    "skills_ssot_migration_snapshot",
    "gemini_common_config_scrub_audit_v1",
    "gemini_common_config_credentials_scrubbed_v1",
    "claude_desktop_gateway_token",
    "first_run_notice_shown",
];

fn is_known_app_suffix(suffix: &str) -> bool {
    AppType::all().any(|app| app.as_str() == suffix)
}

pub fn classify_settings_key(key: &str) -> SettingsKeyClass {
    match key {
        "official_providers_seeded" | "universal_providers" => {
            SettingsKeyClass::Category(SyncCategory::Providers)
        }
        "default_skill_repos_initialized" => SettingsKeyClass::Category(SyncCategory::SkillRepos),
        "global_proxy_url"
        | "rectifier_config"
        | "optimizer_config"
        | "copilot_optimizer_config" => SettingsKeyClass::Category(SyncCategory::ProxySettings),
        "stream_check_config" | "log_config" => {
            SettingsKeyClass::Category(SyncCategory::DiagnosticsSettings)
        }
        "common_config_legacy_migrated_v1"
        | "skills_ssot_migration_pending"
        | "skills_ssot_migration_snapshot"
        | "gemini_common_config_scrub_audit_v1"
        | "gemini_common_config_credentials_scrubbed_v1"
        | "claude_desktop_gateway_token"
        | "first_run_notice_shown" => SettingsKeyClass::LocalOnly,
        other => classify_prefixed_key(other),
    }
}

fn classify_prefixed_key(key: &str) -> SettingsKeyClass {
    if let Some(scope) = key.strip_prefix("current_profile_id_") {
        if is_known_app_suffix(scope) || scope == "claude-desktop" {
            return SettingsKeyClass::Category(SyncCategory::Profiles);
        }
        return SettingsKeyClass::LocalOnly;
    }
    if let Some(rest) = key.strip_prefix("common_config_") {
        if let Some(app) = rest.strip_suffix("_cleared") {
            if is_known_app_suffix(app) {
                return SettingsKeyClass::Category(SyncCategory::CommonConfig);
            }
        } else if is_known_app_suffix(rest) {
            return SettingsKeyClass::Category(SyncCategory::CommonConfig);
        }
        return SettingsKeyClass::LocalOnly;
    }
    if key.starts_with("proxy_takeover_") || key.starts_with("auto_failover_enabled_") {
        return SettingsKeyClass::LocalOnly;
    }
    SettingsKeyClass::LocalOnly
}

pub fn known_settings_keys() -> Vec<&'static str> {
    ALL_KNOWN_SETTINGS_KEYS.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_known_key_is_explicitly_classified() {
        for key in ALL_KNOWN_SETTINGS_KEYS {
            let class = classify_settings_key(key);
            match *key {
                "official_providers_seeded" | "universal_providers" => {
                    assert_eq!(class, SettingsKeyClass::Category(SyncCategory::Providers));
                }
                "default_skill_repos_initialized" => {
                    assert_eq!(class, SettingsKeyClass::Category(SyncCategory::SkillRepos));
                }
                "current_profile_id_claude"
                | "current_profile_id_claude-desktop"
                | "current_profile_id_codex" => {
                    assert_eq!(class, SettingsKeyClass::Category(SyncCategory::Profiles));
                }
                "global_proxy_url"
                | "rectifier_config"
                | "optimizer_config"
                | "copilot_optimizer_config" => {
                    assert_eq!(
                        class,
                        SettingsKeyClass::Category(SyncCategory::ProxySettings)
                    );
                }
                "stream_check_config" | "log_config" => {
                    assert_eq!(
                        class,
                        SettingsKeyClass::Category(SyncCategory::DiagnosticsSettings)
                    );
                }
                "common_config_legacy_migrated_v1"
                | "skills_ssot_migration_pending"
                | "skills_ssot_migration_snapshot"
                | "gemini_common_config_scrub_audit_v1"
                | "gemini_common_config_credentials_scrubbed_v1"
                | "claude_desktop_gateway_token"
                | "first_run_notice_shown" => {
                    assert_eq!(class, SettingsKeyClass::LocalOnly);
                }
                key if key.starts_with("common_config_") => {
                    assert_eq!(
                        class,
                        SettingsKeyClass::Category(SyncCategory::CommonConfig),
                        "{key}"
                    );
                }
                other => panic!("unclassified known key in assertion: {other}"),
            }
        }
    }

    #[test]
    fn unknown_keys_default_to_local_only() {
        assert_eq!(
            classify_settings_key("totally_unknown_device_flag"),
            SettingsKeyClass::LocalOnly
        );
        assert_eq!(
            classify_settings_key("proxy_takeover_claude"),
            SettingsKeyClass::LocalOnly
        );
        assert_eq!(
            classify_settings_key("common_config_notanapp"),
            SettingsKeyClass::LocalOnly
        );
    }

    #[test]
    fn empty_common_config_cleared_flags_follow_h() {
        assert_eq!(
            classify_settings_key("common_config_gemini_cleared"),
            SettingsKeyClass::Category(SyncCategory::CommonConfig)
        );
    }
}

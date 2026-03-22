mod autostart;
mod backup;
mod config_render;
mod credentials;
mod interactive;
mod managed_skills;
mod openclaw_json;
mod skills;
mod types;

#[cfg(test)]
mod test_support;

pub use autostart::{
    autostart_service_path, install_autostart_service, remove_autostart_service,
    start_autostart_service,
};
pub use backup::{backup_openclaw_config, openclaw_config_root};
pub use config_render::generate_clawshell_config;
pub use credentials::{
    cleanup_openclaw_provider_credentials, preview_openclaw_provider_credential_cleanup,
};
pub use interactive::collect_onboard_config_tui;
pub use managed_skills::{
    ManagedSkillInspection, ManagedSkillManifestEntry, ManagedSkillUninstallState,
    build_managed_skill_manifest_entry, build_managed_skill_metadata,
    inspect_managed_skill_for_uninstall, read_managed_skill_manifest_entry,
    upsert_managed_skill_manifest_entry, write_managed_skill_metadata,
};
pub use openclaw_json::{patch_openclaw_config_for_clawshell, remove_clawshell_openclaw_entries};
pub use skills::{render_openclaw_email_messages_skill, should_setup_openclaw_email_skill};
pub use types::{
    OPENCLAW_EMAIL_MESSAGES_SKILL_NAME, OnboardAuthMethod, OnboardConfig,
    OpenclawFileRemovalPreview,
};

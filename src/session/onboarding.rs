//! Shared discovery and registration of external native conversations.

use super::{GroupTree, Instance, ResumeIntent, Storage};
use anyhow::{bail, Context, Result};
use clap::ValueEnum;
use serde::Serialize;
use std::path::Path;

#[derive(Clone, Copy, Debug, ValueEnum, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    Codex,
    Claude,
}

impl Agent {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Conversation {
    pub(crate) agent: String,
    pub(crate) session_id: String,
    pub(crate) cwd: String,
    pub(crate) title: Option<String>,
    pub(crate) last_modified_ms: u64,
    pub(crate) cwd_exists: bool,
}

pub(crate) fn discover(profile: &str, agent: Agent) -> Result<Vec<Conversation>> {
    let environment =
        crate::session::config::profile_config::resolve_config_or_warn(profile).environment;
    let (variable, fallback) = match agent {
        Agent::Codex => ("CODEX_HOME", ".codex"),
        Agent::Claude => ("CLAUDE_CONFIG_DIR", ".claude"),
    };
    let home = crate::hooks::resolve_config_dir_override(variable, &environment)
        .map(std::path::PathBuf::from)
        .or_else(|| dirs::home_dir().map(|p| p.join(fallback)))
        .context("Cannot resolve agent conversation directory")?;
    match agent {
        Agent::Codex => {
            let sessions = home.join("sessions");
            if !sessions.exists() {
                return Ok(Vec::new());
            }
            Ok(crate::session::capture::codex_import_candidates(&sessions)?
                .into_iter()
                .map(|(session_id, cwd, modified)| Conversation {
                    agent: agent.name().to_string(),
                    session_id,
                    cwd_exists: Path::new(&cwd).is_dir(),
                    cwd,
                    title: None,
                    last_modified_ms: crate::util::system_time_to_ms(modified),
                })
                .collect())
        }
        Agent::Claude => Ok(
            crate::session::claude_import::scan_sessions_for_onboarding(&home)
                .into_iter()
                .map(|s| Conversation {
                    agent: agent.name().to_string(),
                    session_id: s.session_id,
                    cwd: s.cwd,
                    title: s.title,
                    last_modified_ms: s.last_modified_ms,
                    cwd_exists: s.cwd_exists,
                })
                .collect(),
        ),
    }
}

pub(crate) fn owns_conversation(instance: &Instance, conversation: &Conversation) -> bool {
    instance
        .resolved_agent()
        .is_some_and(|agent| agent.name == conversation.agent)
        && (instance.agent_session_id.as_deref() == Some(&conversation.session_id)
            || instance.acp_session_id.as_deref() == Some(&conversation.session_id)
            || matches!(&instance.resume_intent, ResumeIntent::Use(id) if id == &conversation.session_id))
}

pub(crate) fn build_instance(
    conversation: &Conversation,
    title: Option<&str>,
    group: &str,
) -> Result<Instance> {
    if !crate::session::is_valid_session_id(&conversation.session_id) {
        bail!("Conversation has an invalid native session ID");
    }
    if !Path::new(&conversation.cwd).is_absolute() || !Path::new(&conversation.cwd).is_dir() {
        bail!(
            "Conversation's original working directory is unavailable: {}",
            conversation.cwd
        );
    }
    let title = title.or(conversation.title.as_deref());
    if title.is_some_and(|title| title.trim().is_empty()) {
        bail!("Session title must not be empty");
    }
    let fallback = format!(
        "{} {}",
        conversation.agent,
        &conversation.session_id[..8.min(conversation.session_id.len())]
    );
    let mut instance = Instance::new(title.unwrap_or(&fallback), &conversation.cwd);
    instance.tool = conversation.agent.clone();
    instance.command = conversation.agent.clone();
    instance.agent_session_id = Some(conversation.session_id.clone());
    instance.resume_intent = ResumeIntent::Use(conversation.session_id.clone());
    instance.group_path = group.to_string();
    Ok(instance)
}

pub(crate) fn register(
    profile: &str,
    conversation: &Conversation,
    instance: &Instance,
) -> Result<(Instance, bool)> {
    let storage = Storage::open_unwatched(profile)?;
    storage.update(|instances, groups| {
        if let Some(existing) = instances
            .iter()
            .find(|i| owns_conversation(i, conversation))
        {
            return Ok((existing.clone(), false));
        }
        instances.push(instance.clone());
        if !instance.group_path.is_empty() {
            let mut tree = GroupTree::new_with_groups(instances, groups);
            tree.create_group(&instance.group_path);
            *groups = tree.get_all_groups();
        }
        Ok((instance.clone(), true))
    })
}

pub(crate) fn list_available(profile: &str, agent: Agent) -> Result<Vec<Conversation>> {
    let mut existing = Storage::open_unwatched(profile)?.load()?;
    for instance in &mut existing {
        instance.source_profile = profile.to_string();
    }
    Ok(discover(profile, agent)?
        .into_iter()
        .filter(|c| !existing.iter().any(|i| owns_conversation(i, c)))
        .collect())
}

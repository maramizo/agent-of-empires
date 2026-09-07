//! Explicit onboarding of a native conversation created outside AoE.

use anyhow::{Context, Result};
use clap::Args;

use crate::session::onboarding::{build_instance, discover, list_available, register, Agent};

#[derive(Args)]
pub struct OnboardArgs {
    /// Exact native conversation ID. Use --list to discover IDs.
    #[arg(required_unless_present = "list", conflicts_with = "list")]
    session_id: Option<String>,

    /// Agent that owns the conversation.
    #[arg(long, value_enum, default_value = "codex")]
    agent: Agent,

    /// List conversations available to onboard without changing anything.
    #[arg(long, conflicts_with_all = ["title", "group", "launch"])]
    list: bool,

    /// Title for the new AoE session.
    #[arg(long)]
    title: Option<String>,

    /// Place the session in this AoE group.
    #[arg(long)]
    group: Option<String>,

    /// Immediately resume in AoE. Close the external agent first.
    #[arg(long)]
    launch: bool,

    /// Output the discovered conversations or created session as JSON.
    #[arg(long, conflicts_with = "launch")]
    json: bool,
}

pub(super) async fn run(profile: &str, args: OnboardArgs) -> Result<()> {
    if args.list {
        let available = list_available(profile, args.agent)?;
        if args.json {
            println!("{}", serde_json::to_string_pretty(&available)?);
        } else if available.is_empty() {
            println!("No external {} conversations found.", args.agent.name());
        } else {
            for c in available {
                let missing = if c.cwd_exists {
                    ""
                } else {
                    " [directory missing]"
                };
                println!("{}  {}{}", c.session_id, c.cwd, missing);
                if let Some(title) = c.title {
                    println!("  {title}");
                }
            }
        }
        return Ok(());
    }
    let conversations = discover(profile, args.agent)?;
    let id = args
        .session_id
        .as_deref()
        .context("A native conversation ID is required")?;
    let conversation = conversations.iter().find(|c| c.session_id == id).with_context(|| {
        format!("{} conversation {id:?} not found. Run `aoe session onboard --agent {} --list` to see available IDs.", args.agent.name(), args.agent.name())
    })?;
    let instance = build_instance(
        conversation,
        args.title.as_deref(),
        args.group.as_deref().unwrap_or_default(),
    )?;
    let (instance, created) = register(profile, conversation, &instance)?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "id": instance.id,
                "title": instance.title,
                "agent": conversation.agent,
                "session_id": conversation.session_id,
                "cwd": instance.project_path,
                "created": created,
            }))?
        );
    } else {
        let action = if created {
            "Onboarded"
        } else {
            "Already managed"
        };
        println!("{action}: {} ({})", instance.title, instance.id);
    }
    if args.launch {
        super::start_session(
            profile,
            super::SessionIdArgs {
                identifier: instance.id,
            },
        )
        .await?;
    } else if !args.json {
        println!("Close the external agent, then resume with `aoe session start {}` or open this session in AoE.", instance.id);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::onboarding::{owns_conversation, Conversation};
    use crate::session::{ResumeIntent, Storage};

    #[test]
    fn onboarding_requires_an_explicit_conversation_or_listing() {
        use clap::CommandFactory;
        let command = crate::cli::Cli::command();
        for args in [
            vec!["aoe", "session", "onboard"],
            vec!["aoe", "session", "onboard", "--list", "--launch"],
            vec!["aoe", "session", "onboard", "--list", "some-id"],
        ] {
            assert!(command.clone().try_get_matches_from(args).is_err());
        }
        assert!(command
            .try_get_matches_from([
                "aoe", "session", "adopt", "some-id", "--agent", "claude", "--json",
            ])
            .is_ok());
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn onboarding_registers_once_without_launching() {
        let temp = tempfile::tempdir().unwrap();
        let _home = crate::session::test_support::isolate_home(temp.path());
        crate::session::create_profile("main").unwrap();
        let codex = temp.path().join("codex");
        let _env = crate::session::test_support::EnvGuard::set(&[("CODEX_HOME", &codex)]);
        let sessions = codex.join("sessions/2026/09/06");
        std::fs::create_dir_all(&sessions).unwrap();
        let native = "11111111-2222-4333-8444-555555555555";
        std::fs::write(sessions.join(format!("rollout-date-{native}.jsonl")), format!("{}\n", serde_json::json!({
            "type": "session_meta", "payload": {"id": native, "cwd": temp.path(), "source": "cli"}
        }))).unwrap();
        for _ in 0..2 {
            run(
                "main",
                OnboardArgs {
                    session_id: Some(native.to_string()),
                    agent: Agent::Codex,
                    list: false,
                    title: Some("External work".to_string()),
                    group: Some("imported".to_string()),
                    launch: false,
                    json: true,
                },
            )
            .await
            .unwrap();
        }
        let rows = Storage::open_unwatched("main").unwrap().load().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].agent_session_id.as_deref(), Some(native));
        assert_eq!(rows[0].resume_intent, ResumeIntent::Use(native.to_string()));
        assert_eq!(rows[0].status, crate::session::Status::Idle);
        assert!(!rows[0].tmux_session().unwrap().exists());
    }

    #[test]
    fn onboarding_preserves_identity_directory_and_agent() {
        let dir = tempfile::tempdir().unwrap();
        for agent in [Agent::Codex, Agent::Claude] {
            let conversation = Conversation {
                agent: agent.name().to_string(),
                session_id: "11111111-2222-4333-8444-555555555555".to_string(),
                cwd: dir.path().to_string_lossy().into_owned(),
                title: Some("External conversation".to_string()),
                last_modified_ms: 0,
                cwd_exists: true,
            };
            let args = OnboardArgs {
                session_id: Some(conversation.session_id.clone()),
                agent,
                list: false,
                title: Some("Imported work".to_string()),
                group: Some("external".to_string()),
                launch: false,
                json: false,
            };
            let instance = build_instance(
                &conversation,
                args.title.as_deref(),
                args.group.as_deref().unwrap_or_default(),
            )
            .unwrap();
            assert_eq!(instance.project_path, conversation.cwd);
            assert_eq!(instance.command, agent.name());
            assert_eq!(instance.title, "Imported work");
            assert_eq!(instance.group_path, "external");
            assert_eq!(
                instance.resume_intent,
                ResumeIntent::Use(conversation.session_id.clone())
            );
            assert!(owns_conversation(&instance, &conversation));
            let mut other = instance.clone();
            other.tool = "gemini".to_string();
            assert!(!owns_conversation(&other, &conversation));
            let mut missing = conversation.clone();
            missing.cwd = dir.path().join("missing").to_string_lossy().into_owned();
            assert!(build_instance(
                &missing,
                args.title.as_deref(),
                args.group.as_deref().unwrap_or_default()
            )
            .is_err());
        }
    }
}

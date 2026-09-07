//! Default prompt delivery: Codex's native queue, terminal input for other agents.
use super::Instance;
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Debug, Serialize)]
pub struct PromptDelivery {
    pub sent: bool,
    pub backend: &'static str,
    pub disposition: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    pub idempotent: bool,
}

impl Instance {
    pub fn uses_codex_queue(&self) -> bool {
        !self.is_structured() && self.resolved_agent().map(|a| a.name) == Some("codex")
    }

    pub fn send_prompt(&self, message: &str) -> Result<PromptDelivery> {
        anyhow::ensure!(!message.trim().is_empty(), "Message cannot be empty");
        if self.uses_codex_queue() {
            return self.queue_codex_prompt(message);
        }
        anyhow::ensure!(
            !self.is_structured(),
            "Structured agents require their structured prompt API"
        );
        let session = self.tmux_session()?;
        let tool = self.effective_detect_as();
        session.wait_until_ready(Duration::from_secs(5), crate::agents::ready_marker(&tool));
        session.send_keys_with_delay(message, crate::agents::send_keys_enter_delay(&tool))?;
        Ok(PromptDelivery {
            sent: true,
            backend: "terminal",
            disposition: "submitted",
            thread_id: None,
            idempotent: false,
        })
    }

    pub fn queue_codex_prompt(&self, message: &str) -> Result<PromptDelivery> {
        anyhow::ensure!(self.uses_codex_queue() && !self.is_sandboxed() && !self.has_command_override() && !self.extra_args.contains("--remote"),
            "Native queue requires a local, unsandboxed Codex terminal agent without a custom command or remote override");
        anyhow::ensure!(!message.trim().is_empty(), "Message cannot be empty");
        let session = self.tmux_session()?;
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut connected = false;
        let thread = loop {
            anyhow::ensure!(
                session.exists() && !session.is_pane_dead(),
                "Codex agent is not running; start it before queueing"
            );
            if let Some(endpoint) = crate::tmux::env::get_hidden_env(
                session.name(),
                super::codex_terminal::ENDPOINT_KEY,
            ) {
                connected = true;
                if let Some(thread) = super::codex_terminal::active_thread(&endpoint)? {
                    break (thread, endpoint);
                }
            }
            if Instant::now() >= deadline {
                if connected {
                    bail!("Codex has not opened a conversation yet. Complete any startup or trust prompts in the terminal and retry. No message was sent.");
                }
                bail!("Codex native terminal connection is unavailable. Restart this agent once to enable queue delivery. No message was sent.");
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let (thread, endpoint) = thread;
        let mut environment =
            super::environment::resolve_host_environment_pairs(&self.resolved_host_environment());
        environment.push(("AOE_INSTANCE_ID".into(), self.id.clone()));
        environment.push((
            "AOE_HOOK_BIN".into(),
            crate::process::current_exe_for_spawn()?
                .to_string_lossy()
                .into_owned(),
        ));
        environment.push(("AOE_CODEX_QUEUE_ENDPOINT".into(), endpoint));
        run_codex_queue("codex", &thread, message, &self.project_path, environment)?;
        if let Err(error) = crate::hooks::write_session_id_via_guard(&self.id, &thread) {
            tracing::warn!("Queued Codex message but could not publish identity: {error}");
        }
        Ok(PromptDelivery {
            sent: true,
            backend: "codex",
            disposition: "queued",
            thread_id: Some(thread),
            idempotent: false,
        })
    }
}

pub(crate) fn run_codex_queue(
    executable: &str,
    thread: &str,
    text: &str,
    cwd: &str,
    environment: Vec<(String, String)>,
) -> Result<()> {
    let mut command = Command::new(executable);
    if let Some((_, endpoint)) = environment
        .iter()
        .find(|(key, _)| key == "AOE_CODEX_QUEUE_ENDPOINT")
    {
        command.args(["--remote", endpoint]);
    }
    command
        .args(["queue", "--thread", thread])
        .arg(format!("--message={text}"))
        .current_dir(cwd)
        .envs(environment)
        .stdin(Stdio::null());
    let output =
        crate::process::run_with_timeout_process_group(&mut command, Duration::from_secs(30))
            .context("Could not execute codex queue")?
            .context(
                "Codex queue timed out; delivery is unknown. Inspect the worker before retrying.",
            )?;
    anyhow::ensure!(
        output.status.success(),
        "codex queue failed ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
            .chars()
            .take(2000)
            .collect::<String>()
    );
    Ok(())
}

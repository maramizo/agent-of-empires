//! Explicit terminal launch choices, independent of profile defaults.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_mode: Option<bool>,
}

impl LaunchOptions {
    pub fn is_empty(&self) -> bool {
        self.model.is_none() && self.effort.is_none() && self.fast_mode.is_none()
    }

    pub fn merged(&self, patch: &Self) -> Self {
        Self {
            model: patch.model.clone().or_else(|| self.model.clone()),
            effort: patch.effort.clone().or_else(|| self.effort.clone()),
            fast_mode: patch.fast_mode.or(self.fast_mode),
        }
    }

    pub fn validate(
        &self,
        tool: &str,
        structured: bool,
        custom_command: bool,
    ) -> anyhow::Result<()> {
        for (name, value) in [("model", &self.model), ("effort", &self.effort)] {
            if let Some(value) = value {
                anyhow::ensure!(
                    !value.trim().is_empty()
                        && value.len() <= 200
                        && !value.chars().any(char::is_control),
                    "{name} must be nonempty, at most 200 bytes, and contain no control characters"
                );
            }
        }
        if self.is_empty() {
            return Ok(());
        }
        if structured {
            anyhow::ensure!(
                self.fast_mode.is_none(),
                "fast_mode requires a terminal Codex agent"
            );
            return Ok(());
        }
        anyhow::ensure!(
            !custom_command,
            "Launch choices are not supported with a custom command override"
        );
        anyhow::ensure!(
            matches!(tool, "codex" | "claude"),
            "Terminal launch choices currently support Codex and Claude"
        );
        anyhow::ensure!(
            tool == "codex" || self.fast_mode.is_none(),
            "fast_mode requires a terminal Codex agent"
        );
        if let Some(effort) = &self.effort {
            let valid = if tool == "codex" {
                matches!(
                    effort.as_str(),
                    "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra"
                )
            } else {
                matches!(effort.as_str(), "low" | "medium" | "high" | "max")
            };
            anyhow::ensure!(valid, "Unsupported effort level for {tool}");
        }
        Ok(())
    }

    pub fn append_args(&self, tool: &str, cmd: &mut String) -> anyhow::Result<()> {
        self.validate(tool, false, false)?;
        let mut args = Vec::new();
        if let Some(model) = &self.model {
            args.extend(["--model".to_string(), model.clone()]);
        }
        if let Some(effort) = &self.effort {
            if tool == "codex" {
                args.extend([
                    "-c".to_string(),
                    format!("model_reasoning_effort={}", serde_json::to_string(effort)?),
                ]);
            } else {
                args.extend(["--effort".to_string(), effort.clone()]);
            }
        }
        if let Some(fast) = self.fast_mode {
            args.extend([
                "-c".to_string(),
                format!("service_tier=\"{}\"", if fast { "fast" } else { "default" }),
            ]);
        }
        for arg in args {
            cmd.push(' ');
            cmd.push_str(&crate::session::environment::shell_escape(&arg));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_options_quote_values_and_preserve_explicit_false() {
        let opts = LaunchOptions {
            model: Some("model'$(touch /tmp/not-executed)[1m]".into()),
            effort: Some("xhigh".into()),
            fast_mode: Some(true),
        };
        let mut cmd = "codex".to_string();
        opts.append_args("codex", &mut cmd).unwrap();
        // Let a shell decode arguments without invoking an agent or evaluating the model value.
        let script = format!("set -- {}; printf '%s\\n' \"$@\"", cmd);
        let output = std::process::Command::new("sh")
            .args(["-c", &script])
            .output()
            .unwrap();
        let output = String::from_utf8(output.stdout).unwrap();
        let args: Vec<_> = output.lines().collect();
        assert_eq!(
            args,
            vec![
                "codex",
                "--model",
                opts.model.as_deref().unwrap(),
                "-c",
                "model_reasoning_effort=\"xhigh\"",
                "-c",
                "service_tier=\"fast\""
            ]
        );
        let updated = opts.merged(&LaunchOptions {
            fast_mode: Some(false),
            ..Default::default()
        });
        let mut cmd = "codex resume saved-thread".into();
        updated.append_args("codex", &mut cmd).unwrap();
        assert!(cmd.contains("default"));
        assert_eq!(updated.model, opts.model);
        assert!(LaunchOptions {
            effort: Some("invalid".into()),
            ..Default::default()
        }
        .validate("codex", false, false)
        .is_err());
        assert!(opts.validate("claude", false, false).is_err());
        assert!(opts.validate("codex", true, false).is_err());
        assert!(opts.validate("codex", false, true).is_err());
        let legacy = crate::session::Instance::new("legacy", "/tmp");
        let mut row = serde_json::to_value(&legacy).unwrap();
        row.as_object_mut().unwrap().remove("terminal_launch");
        let restored: crate::session::Instance = serde_json::from_value(row).unwrap();
        assert!(restored.terminal_launch.is_empty());
    }
}

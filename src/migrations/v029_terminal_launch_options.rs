//! Mark support for explicit terminal model, effort and service-tier choices.
//! Older rows retain their launch behavior through the empty serde default;
//! no existing agent configuration or native conversation is rewritten.
pub fn run() -> anyhow::Result<()> {
    tracing::info!(
        "Terminal launch options enabled; existing sessions inherit their current defaults"
    );
    Ok(())
}

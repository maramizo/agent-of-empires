//! Introduce the versioned per-profile monitor registry without enabling jobs.
pub fn run() -> anyhow::Result<()> {
    for profile in crate::session::list_profiles()? {
        crate::server::monitors::initialize_profile(&profile)?;
    }
    Ok(())
}

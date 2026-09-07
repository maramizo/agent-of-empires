//! Drop cached releases from upstream when adopting the fork's update source.

pub fn run() -> anyhow::Result<()> {
    let cache = crate::session::get_app_dir()?.join("update_cache.json");
    match std::fs::remove_file(cache) {
        Ok(()) => tracing::info!("Cleared upstream update cache"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    #[serial_test::serial]
    fn clears_old_releases_without_touching_sessions_and_is_repeatable() {
        let temp = tempfile::tempdir().unwrap();
        let _guard = crate::session::test_support::isolate_app_dir_at(temp.path());
        let app = crate::session::get_app_dir().unwrap();
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(
            app.join("update_cache.json"),
            r#"{"latest_version":"99.0.0"}"#,
        )
        .unwrap();
        std::fs::write(app.join("sessions.json"), "preserve sessions").unwrap();
        super::run().unwrap();
        super::run().unwrap();
        assert!(!app.join("update_cache.json").exists());
        assert_eq!(
            std::fs::read_to_string(app.join("sessions.json")).unwrap(),
            "preserve sessions"
        );
    }
}

//! Load/save user settings, as the single `'current'` row of the app-wide
//! database's `settings` table (`crate::app_db`), the same singleton-row
//! idiom that table's own doc comment establishes. Corrupt or missing rows
//! fall back to defaults — settings are never a reason the app fails to
//! start.

use crate::types::Settings;
use crate::Result;
use rusqlite::{params, Connection, OptionalExtension};

pub fn load(conn: &Connection) -> Settings {
    let row: Option<String> = conn
        .query_row(
            "SELECT json FROM settings WHERE key = 'current'",
            [],
            |row| row.get(0),
        )
        .optional()
        .unwrap_or(None);
    match row {
        Some(raw) => serde_json::from_str(&raw).unwrap_or_else(|e| {
            tracing::warn!("settings row is corrupt ({e}); using defaults");
            Settings::default()
        }),
        None => Settings::default(),
    }
}

pub fn save(conn: &Connection, settings: &Settings) -> Result<()> {
    let json = serde_json::to_string_pretty(settings)?;
    conn.execute(
        "INSERT INTO settings (key, json) VALUES ('current', ?1)
         ON CONFLICT(key) DO UPDATE SET json = excluded.json",
        params![json],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// See `crate::store::tests::test_conn`'s own doc comment for why
    /// `.keep()` is used here.
    fn test_conn() -> Connection {
        let dir = tempfile::tempdir().unwrap().keep();
        crate::app_db::open(&dir.join("app.sqlite"), None)
            .unwrap()
            .0
    }

    fn write_raw(conn: &Connection, json: &str) {
        conn.execute(
            "INSERT INTO settings (key, json) VALUES ('current', ?1)
             ON CONFLICT(key) DO UPDATE SET json = excluded.json",
            params![json],
        )
        .unwrap();
    }

    #[test]
    fn roundtrip_and_corrupt_fallback() {
        let conn = test_conn();

        // Missing row -> defaults
        let s = load(&conn);
        assert!(!s.onboarding_completed);

        // Roundtrip
        let mut s2 = s.clone();
        s2.onboarding_completed = true;
        s2.user_name = Some("Ada".into());
        s2.selected_model_id = Some("qwen3-4b".into());
        s2.default_model_id = Some("qwen3-4b".into());
        save(&conn, &s2).unwrap();
        let s3 = load(&conn);
        assert!(s3.onboarding_completed);
        assert_eq!(s3.user_name.as_deref(), Some("Ada"));
        assert_eq!(s3.selected_model_id.as_deref(), Some("qwen3-4b"));
        assert_eq!(s3.default_model_id.as_deref(), Some("qwen3-4b"));

        // Corrupt row -> defaults, no panic
        write_raw(&conn, "{not json");
        let s4 = load(&conn);
        assert!(!s4.onboarding_completed);
    }

    #[test]
    fn auto_unload_defaults_are_on_fifteen_minutes_disk_slow_exempt() {
        let s = Settings::default();
        assert!(s.auto_unload_enabled);
        assert_eq!(s.auto_unload_minutes, 15);
        assert!(!s.auto_unload_include_disk_slow);
    }

    #[test]
    fn auto_load_on_startup_defaults_to_true() {
        assert!(Settings::default().auto_load_on_startup);
    }

    #[test]
    fn custom_models_default_off_for_a_settings_row_predating_them() {
        let conn = test_conn();
        write_raw(&conn, r#"{"onboardingCompleted": true}"#);
        assert!(!load(&conn).custom_models_enabled);
    }

    #[test]
    fn auto_unload_fields_default_in_for_a_settings_row_predating_them() {
        // Simulates an existing user's settings row from before this feature
        // shipped: the new keys are simply absent. `#[serde(default)]` on the
        // `Settings` struct must fill them in with `Settings::default()`'s
        // values rather than failing to deserialize.
        let conn = test_conn();
        write_raw(&conn, r#"{"onboardingCompleted": true}"#);

        let s = load(&conn);
        assert!(s.onboarding_completed);
        assert!(s.auto_unload_enabled);
        assert_eq!(s.auto_unload_minutes, 15);
        assert!(!s.auto_unload_include_disk_slow);
        assert!(s.auto_load_on_startup);
        assert_eq!(s.user_name, None);
    }

    #[test]
    fn default_model_id_defaults_to_none_for_a_settings_row_predating_it() {
        let conn = test_conn();
        write_raw(
            &conn,
            r#"{"onboardingCompleted": true, "selectedModelId": "qwen3-4b"}"#,
        );

        let s = load(&conn);
        assert_eq!(s.selected_model_id.as_deref(), Some("qwen3-4b"));
        assert_eq!(s.default_model_id, None);
    }
}

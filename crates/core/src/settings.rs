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

    #[test]
    fn theme_defaults_to_system_for_a_settings_row_predating_it() {
        // An existing install's row from before the theme setting shipped has
        // no `theme` key. It must load as "system", so the app looks exactly
        // as it did before the upgrade. The other keys must come through too:
        // a row that failed to parse would also read "system" (the whole-row
        // default), so the theme alone can't tell the two apart.
        let conn = test_conn();
        write_raw(&conn, r#"{"onboardingCompleted": true, "userName": "Ada"}"#);

        let s = load(&conn);
        assert_eq!(s.theme, "system");
        assert!(s.onboarding_completed);
        assert_eq!(s.user_name.as_deref(), Some("Ada"));
    }

    #[test]
    fn a_saved_theme_survives_a_reload() {
        // The Rust tier of "the choice survives quitting and relaunching": a
        // relaunch is `app_db::open` on the same file followed by `load` (see
        // `finish_unlock` in the app crate), so this reopens the database
        // rather than reading back through the connection that wrote it. The
        // other fields are non-default so a row that came back as all
        // defaults can't pass for one that came back whole.
        let conn = test_conn();
        let db_path = conn.path().expect("test_conn opens a file").to_owned();
        let chosen = Settings {
            onboarding_completed: true,
            user_name: Some("Ada".into()),
            language: "fr".into(),
            theme: "dark".into(),
            ..Settings::default()
        };
        save(&conn, &chosen).unwrap();
        drop(conn);

        let (reopened, _, _) = crate::app_db::open(std::path::Path::new(&db_path), None).unwrap();
        let s = load(&reopened);
        assert_eq!(s.theme, "dark");
        assert!(s.onboarding_completed);
        assert_eq!(s.user_name.as_deref(), Some("Ada"));
        assert_eq!(s.language, "fr");
    }

    #[test]
    fn desktop_notifications_default_off_for_a_settings_row_predating_them() {
        // An existing install's row from before the notification switch
        // shipped has no `desktopNotifications` key. It must load as off, so
        // upgrading never starts raising OS notifications the user never
        // asked for. The other keys must come through too: off is also the
        // whole-row default, so a row that failed to parse would read off as
        // well, and the switch alone can't tell the two apart.
        let conn = test_conn();
        write_raw(&conn, r#"{"onboardingCompleted": true, "userName": "Ada"}"#);

        let s = load(&conn);
        assert!(!s.desktop_notifications);
        assert!(s.onboarding_completed);
        assert_eq!(s.user_name.as_deref(), Some("Ada"));
    }

    #[test]
    fn a_saved_desktop_notifications_choice_survives_a_reload() {
        // The Rust tier of "the switch's value survives a restart": a relaunch
        // is `app_db::open` on the same file followed by `load` (see
        // `finish_unlock` in the app crate), so this reopens the database
        // rather than reading back through the connection that wrote it. The
        // other fields are non-default so a row that came back as all
        // defaults can't pass for one that came back whole (and `false` is
        // the default, so only the `true` side proves anything).
        let conn = test_conn();
        let db_path = conn.path().expect("test_conn opens a file").to_owned();
        let chosen = Settings {
            onboarding_completed: true,
            user_name: Some("Ada".into()),
            language: "fr".into(),
            desktop_notifications: true,
            ..Settings::default()
        };
        save(&conn, &chosen).unwrap();
        drop(conn);

        let (reopened, _, _) = crate::app_db::open(std::path::Path::new(&db_path), None).unwrap();
        let s = load(&reopened);
        assert!(s.desktop_notifications);
        assert!(s.onboarding_completed);
        assert_eq!(s.user_name.as_deref(), Some("Ada"));
        assert_eq!(s.language, "fr");
    }

    #[test]
    fn an_unrecognised_theme_does_not_reset_the_rest_of_the_row() {
        // `load` resets the WHOLE row to defaults on any parse error, so a
        // theme this build doesn't know (a newer build's extra palette after
        // a downgrade, a hand edit) must not be able to fail the parse: the
        // user would be thrown back into onboarding and lose their name. This
        // is why `Settings.theme` is a `String` and not an enum. The value
        // comes through verbatim, and the UI reads it as "system".
        let conn = test_conn();
        write_raw(
            &conn,
            r#"{"onboardingCompleted": true, "userName": "Ada", "theme": "sepia"}"#,
        );

        let s = load(&conn);
        assert!(s.onboarding_completed);
        assert_eq!(s.user_name.as_deref(), Some("Ada"));
        assert_eq!(s.theme, "sepia");
    }
}

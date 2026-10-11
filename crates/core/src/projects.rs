//! Project persistence, in the app-wide SQLCipher database (`crate::app_db`)
//! — one row per project in the `projects` table. Unlike `crate::store`'s
//! conversations table, there's no blob column here: `Project` is just four
//! scalar fields (`{id, name, created_at_ms, updated_at_ms}`) with no
//! evolution history, so full relational columns are simpler and there's
//! nothing to keep in sync. Which conversations belong to a project lives on
//! the conversation side (`Conversation::project_id`, see `crate::store`),
//! not here, so moving a conversation between projects is a single
//! conversation row write instead of keeping two lists in sync.
//!
//! Same idioms as `crate::store`:
//! - `list` tolerates corrupt/unreadable rows (never fatal).
//! - Project ids come from the frontend (UUIDs). [`validate_id`] keeps the
//!   `[a-zA-Z0-9-]`-only charset check the old file-path version used —
//!   input hygiene now, not a path-traversal defense (see `crate::store`'s
//!   own doc comment for the same reasoning).
//! - [`delete`]'s conversation-unassign cascade goes through
//!   `crate::store::get`/`save` per conversation, never a bulk column-only
//!   `UPDATE` — `crate::store`'s schema keeps `project_id` in two places (an
//!   extracted column and embedded in the conversation's own JSON blob), and
//!   only `crate::store::save` is allowed to write either. Best-effort per
//!   conversation (a corrupt/unreadable sibling is skipped, same posture as
//!   `crate::store::list`), so one bad conversation never blocks the project
//!   delete itself — matching the file-based version's own resilience, and
//!   deliberately not wrapped in one transaction with the project row delete
//!   (an all-or-nothing failure mode this module has never had).

use crate::types::Project;
use crate::{Error, Result};
use rusqlite::{params, Connection};

pub fn list(conn: &Connection) -> Vec<Project> {
    let mut stmt = match conn.prepare(
        "SELECT id, name, created_at_ms, updated_at_ms FROM projects ORDER BY name COLLATE NOCASE",
    ) {
        Ok(stmt) => stmt,
        Err(e) => {
            tracing::warn!("couldn't prepare the project list query: {e}");
            return Vec::new();
        }
    };
    let rows = match stmt.query_map([], |row| {
        Ok(Project {
            id: row.get(0)?,
            name: row.get(1)?,
            created_at_ms: row.get::<_, i64>(2)? as u64,
            updated_at_ms: row.get::<_, i64>(3)? as u64,
        })
    }) {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!("couldn't query projects: {e}");
            return Vec::new();
        }
    };

    let mut projects = Vec::new();
    for row in rows {
        match row {
            Ok(p) => projects.push(p),
            Err(e) => tracing::warn!("skipping unreadable project row: {e}"),
        }
    }
    projects
}

pub fn get(conn: &Connection, id: &str) -> Result<Project> {
    validate_id(id)?;
    conn.query_row(
        "SELECT id, name, created_at_ms, updated_at_ms FROM projects WHERE id = ?1",
        params![id],
        |row| {
            Ok(Project {
                id: row.get(0)?,
                name: row.get(1)?,
                created_at_ms: row.get::<_, i64>(2)? as u64,
                updated_at_ms: row.get::<_, i64>(3)? as u64,
            })
        },
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Error::NotFound(format!("project {id}")),
        other => Error::Db(other),
    })
}

pub fn save(conn: &Connection, project: &Project) -> Result<()> {
    validate_id(&project.id)?;
    conn.execute(
        "INSERT OR REPLACE INTO projects (id, name, created_at_ms, updated_at_ms) VALUES (?1, ?2, ?3, ?4)",
        params![
            project.id,
            project.name,
            project.created_at_ms as i64,
            project.updated_at_ms as i64,
        ],
    )?;
    Ok(())
}

/// Delete the project, then clear `project_id` on every conversation that
/// still points at it — see this module's header for why that's a
/// `crate::store::get`/`save` loop, not a bulk `UPDATE`.
pub fn delete(conn: &Connection, id: &str) -> Result<()> {
    validate_id(id)?;
    conn.execute("DELETE FROM projects WHERE id = ?1", params![id])?;
    unassign_conversations(conn, id);
    Ok(())
}

fn unassign_conversations(conn: &Connection, project_id: &str) {
    for meta in crate::store::list(conn) {
        if meta.project_id.as_deref() != Some(project_id) {
            continue;
        }
        let Ok(mut conversation) = crate::store::get(conn, &meta.id) else {
            continue;
        };
        conversation.project_id = None;
        if let Err(e) = crate::store::save(conn, &conversation) {
            tracing::warn!(
                "couldn't unassign conversation {} from deleted project {project_id}: {e}",
                meta.id
            );
        }
    }
}

/// Input hygiene, not a path-traversal defense (see this module's header).
fn validate_id(id: &str) -> Result<()> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err(Error::NotFound(format!("project {id}")));
    }
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

    fn project(id: &str, name: &str, updated_at_ms: u64) -> Project {
        Project {
            id: id.to_string(),
            name: name.to_string(),
            created_at_ms: 1,
            updated_at_ms,
        }
    }

    #[test]
    fn save_get_list_delete_roundtrip() {
        let conn = test_conn();

        assert!(list(&conn).is_empty());

        save(&conn, &project("proj-b", "Bravo", 200)).unwrap();
        save(&conn, &project("proj-a", "Alpha", 100)).unwrap();

        let all = list(&conn);
        let names: Vec<&str> = all.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Alpha", "Bravo"], "alphabetical by name");

        let got = get(&conn, "proj-a").unwrap();
        assert_eq!(got.name, "Alpha");

        delete(&conn, "proj-a").unwrap();
        delete(&conn, "proj-a").unwrap(); // idempotent
        assert!(matches!(get(&conn, "proj-a"), Err(Error::NotFound(_))));
        assert_eq!(list(&conn).len(), 1);
    }

    /// A row that exists but fails to extract at the SQL level (as opposed
    /// to a value that's merely surprising) must be skipped, not abort the
    /// whole list — same "one bad row never blocks the rest" posture
    /// `crate::store::list` upholds. A `BLOB` literal written into `name`
    /// (`TEXT` affinity) is the concrete way to construct this: SQLite does
    /// not coerce it, so `row.get::<_, String>(..)` genuinely fails.
    #[test]
    fn a_row_that_fails_to_extract_at_the_sql_level_is_skipped_not_fatal() {
        let conn = test_conn();
        save(&conn, &project("proj-a", "Alpha", 100)).unwrap();
        conn.execute(
            "INSERT INTO projects (id, name, created_at_ms, updated_at_ms) \
             VALUES ('bad', X'0102', 1, 1)",
            [],
        )
        .unwrap();

        let all = list(&conn);
        assert_eq!(all.len(), 1, "the well-formed row still lists");
        assert_eq!(all[0].id, "proj-a");
    }

    #[test]
    fn hostile_ids_are_rejected() {
        let conn = test_conn();
        for id in ["", "../evil", "a/b", "a.b", "id with spaces", "café"] {
            assert!(
                matches!(get(&conn, id), Err(Error::NotFound(_))),
                "get {id:?}"
            );
            assert!(
                matches!(delete(&conn, id), Err(Error::NotFound(_))),
                "delete {id:?}"
            );
            assert!(
                matches!(save(&conn, &project(id, "x", 1)), Err(Error::NotFound(_))),
                "save {id:?}"
            );
        }
    }

    #[test]
    fn deleting_a_project_unassigns_its_conversations_but_leaves_others_untouched() {
        let conn = test_conn();
        save(&conn, &project("proj-1", "Research", 100)).unwrap();
        save(&conn, &project("proj-2", "Other", 100)).unwrap();

        let mut in_proj_1 = test_conversation("conv-1", "proj-1");
        let mut also_in_proj_1 = test_conversation("conv-2", "proj-1");
        let in_proj_2 = test_conversation("conv-3", "proj-2");
        crate::store::save(&conn, &in_proj_1).unwrap();
        crate::store::save(&conn, &also_in_proj_1).unwrap();
        crate::store::save(&conn, &in_proj_2).unwrap();

        delete(&conn, "proj-1").unwrap();

        in_proj_1.project_id = None;
        also_in_proj_1.project_id = None;
        assert_eq!(crate::store::get(&conn, "conv-1").unwrap().project_id, None);
        assert_eq!(crate::store::get(&conn, "conv-2").unwrap().project_id, None);
        assert_eq!(
            crate::store::get(&conn, "conv-3").unwrap().project_id,
            Some("proj-2".to_string()),
            "a conversation in a different project must be untouched"
        );
        // The other project itself is untouched, only proj-1 was deleted.
        assert_eq!(list(&conn).len(), 1);
    }

    fn test_conversation(id: &str, project_id: &str) -> crate::types::Conversation {
        crate::types::Conversation {
            id: id.to_string(),
            title: format!("Chat {id}"),
            created_at_ms: 1,
            updated_at_ms: 1,
            messages: Vec::new(),
            sampling: crate::types::SamplingParams::default(),
            context_length: 8192,
            task_type: crate::types::TaskType::General,
            project_id: Some(project_id.to_string()),
            pinned: false,
            branches: std::collections::HashMap::new(),
            forked_from: None,
        }
    }
}

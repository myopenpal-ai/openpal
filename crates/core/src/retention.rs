//! "Delete old conversations" (`Settings::auto_delete_after_days`): which
//! conversations an automatic cleanup removes. Pure selection only — the app
//! crate's `retention` ticker deletes what [`expired`] returns through the
//! same path a manual delete takes, so with sync on each deletion reaches
//! every paired device.
//!
//! Age is measured from a conversation's last activity (`updated_at_ms`),
//! never from when it was created. Pinned conversations and conversations in
//! a project are always kept, however old they are.

use crate::types::ConversationMeta;

const DAY_MS: u64 = 24 * 60 * 60 * 1000;

/// Ids of the conversations in `metas` whose last activity is strictly older
/// than `after_days` days before `now_ms`, excluding pinned ones and ones in a
/// project. A conversation exactly at the cutoff is kept. `None` — and `0`,
/// which the Settings UI never saves but a hand-edited row could hold — means
/// the cleanup is off and nothing is selected.
pub fn expired(metas: &[ConversationMeta], now_ms: u64, after_days: Option<u32>) -> Vec<String> {
    let Some(days) = after_days.filter(|&days| days > 0) else {
        return Vec::new();
    };
    let Some(cutoff_ms) = now_ms.checked_sub(u64::from(days) * DAY_MS) else {
        // The period reaches back before the epoch: nothing can be older.
        return Vec::new();
    };
    metas
        .iter()
        .filter(|meta| meta.updated_at_ms < cutoff_ms && !meta.pinned && meta.project_id.is_none())
        .map(|meta| meta.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TaskType;

    const NOW_MS: u64 = 1_800_000_000_000;

    fn meta(id: &str, updated_at_ms: u64) -> ConversationMeta {
        ConversationMeta {
            id: id.to_string(),
            title: format!("Chat {id}"),
            created_at_ms: 1,
            updated_at_ms,
            message_count: 1,
            task_type: TaskType::General,
            project_id: None,
            pinned: false,
            unread: false,
        }
    }

    fn days_ago(days: u64) -> u64 {
        NOW_MS - days * DAY_MS
    }

    #[test]
    fn selects_a_conversation_older_than_the_cutoff() {
        let metas = [meta("old", days_ago(31))];
        assert_eq!(expired(&metas, NOW_MS, Some(30)), ["old"]);
    }

    #[test]
    fn keeps_a_conversation_newer_than_the_cutoff() {
        let metas = [meta("recent", days_ago(29))];
        assert!(expired(&metas, NOW_MS, Some(30)).is_empty());
    }

    #[test]
    fn keeps_a_conversation_exactly_at_the_cutoff_and_selects_it_one_ms_later() {
        let at_cutoff = days_ago(30);
        assert!(expired(&[meta("edge", at_cutoff)], NOW_MS, Some(30)).is_empty());
        assert_eq!(
            expired(&[meta("edge", at_cutoff - 1)], NOW_MS, Some(30)),
            ["edge"]
        );
    }

    #[test]
    fn measures_age_from_last_activity_not_creation() {
        // Created long ago but used yesterday: kept.
        let metas = [ConversationMeta {
            created_at_ms: days_ago(400),
            ..meta("revived", days_ago(1))
        }];
        assert!(expired(&metas, NOW_MS, Some(30)).is_empty());
    }

    #[test]
    fn keeps_a_pinned_conversation_however_old() {
        let metas = [ConversationMeta {
            pinned: true,
            ..meta("pinned", days_ago(5000))
        }];
        assert!(expired(&metas, NOW_MS, Some(30)).is_empty());
    }

    #[test]
    fn keeps_a_conversation_in_a_project_however_old() {
        let metas = [ConversationMeta {
            project_id: Some("project-1".to_string()),
            ..meta("in-project", days_ago(5000))
        }];
        assert!(expired(&metas, NOW_MS, Some(30)).is_empty());
    }

    #[test]
    fn selects_nothing_when_the_setting_is_off() {
        let metas = [meta("ancient", 0), meta("old", days_ago(400))];
        assert!(expired(&metas, NOW_MS, None).is_empty());
        assert!(expired(&metas, NOW_MS, Some(0)).is_empty());
    }

    #[test]
    fn selects_only_the_qualifying_conversations_from_a_mix() {
        let metas = [
            meta("old-1", days_ago(91)),
            meta("recent", days_ago(10)),
            ConversationMeta {
                pinned: true,
                ..meta("old-pinned", days_ago(200))
            },
            meta("old-2", days_ago(365)),
        ];
        assert_eq!(expired(&metas, NOW_MS, Some(90)), ["old-1", "old-2"]);
    }

    #[test]
    fn a_period_longer_than_the_clock_selects_nothing() {
        assert!(expired(&[meta("zero", 0)], 1000, Some(u32::MAX)).is_empty());
    }
}

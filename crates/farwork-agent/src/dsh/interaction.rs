//! Pending DSH interactions: what the browser may answer, and how the answer
//! travels back.
//!
//! The queue exists because the answer is a two-step fact: the bridge names each
//! button, the browser sends one back, and the bridge dedups by `noticeId` +
//! `actionId`. Re-validating the action against the notice we actually received is
//! what keeps a stale browser tab from sending an id the runtime never offered.

use std::collections::HashMap;

use serde_json::Value;

use crate::protocol::Decision;

use super::projection;

#[derive(Debug, Clone, PartialEq)]
pub struct Interaction {
    pub notice_id: String,
    pub action_ids: Vec<String>,
    /// Structured answer shape (a question notice carries one). Kept verbatim:
    /// reshaping somebody's answer is how it gets silently corrupted.
    pub input_schema: Option<Value>,
}

#[derive(Debug, Default)]
pub struct NoticeQueue {
    pending: HashMap<String, Interaction>,
}

impl NoticeQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the actions a notice offers. Called for every open notice, so a
    /// re-published notice replaces its own earlier action list.
    pub fn track(&mut self, notice: &Value) -> Interaction {
        let notice_id = notice
            .get("noticeId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        let tracked = Interaction {
            notice_id: notice_id.clone(),
            action_ids: projection::notice_actions(notice)
                .into_iter()
                .map(|action| action.id)
                .collect(),
            input_schema: notice.get("inputSchema").cloned(),
        };
        self.pending.insert(notice_id, tracked.clone());
        tracked
    }

    pub fn restore(&mut self, interaction: Interaction) {
        self.pending
            .insert(interaction.notice_id.clone(), interaction);
    }

    pub fn action_ids(&self, notice_id: &str) -> Option<&[String]> {
        self.pending
            .get(notice_id)
            .map(|interaction| interaction.action_ids.as_slice())
    }

    pub fn contains(&self, notice_id: &str) -> bool {
        self.pending.contains_key(notice_id)
    }

    pub fn pop(&mut self, notice_id: &str) -> Option<Interaction> {
        self.pending.remove(notice_id)
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AnswerError {
    #[error("the answer carries neither a decision nor an action id")]
    Empty,
    #[error("this notice has no action {action:?} (available: {available})")]
    NoSuchAction { action: String, available: String },
}

/// Resolve one answer into the action id the bridge expects.
///
/// `action_id` is the browser's choice and wins when the notice really offers it.
/// `decision` is the older, closed vocabulary: it is translated into the runtime's
/// own spelling (`approve` → `allow-once`), and refused when no such button
/// exists — an approval recorded as "reject" because the mapping guessed wrong is
/// the failure this avoids.
pub fn action_for(
    interaction: &Interaction,
    decision: Option<Decision>,
    action_id: Option<&str>,
) -> Result<String, AnswerError> {
    if let Some(action_id) = action_id.map(str::trim).filter(|id| !id.is_empty()) {
        if interaction.action_ids.is_empty()
            || interaction.action_ids.iter().any(|id| id == action_id)
        {
            return Ok(action_id.to_string());
        }
        return Err(AnswerError::NoSuchAction {
            action: action_id.to_string(),
            available: interaction.action_ids.join(", "),
        });
    }
    let Some(decision) = decision else {
        return Err(AnswerError::Empty);
    };
    for candidate in candidates(decision) {
        if interaction.action_ids.iter().any(|id| id == candidate) {
            return Ok((*candidate).to_string());
        }
    }
    Err(AnswerError::NoSuchAction {
        action: format!("{decision:?}"),
        available: interaction.action_ids.join(", "),
    })
}

fn candidates(decision: Decision) -> &'static [&'static str] {
    match decision {
        Decision::Approve => &[
            "approve",
            "allow-once",
            "allow",
            "accept",
            "yes",
            "continue",
            "submit",
        ],
        Decision::ApproveForSession => &[
            "approve_for_session",
            "allow-always",
            "always",
            "approve-for-session",
        ],
        Decision::Reject => &["reject", "deny", "cancel", "no", "decline", "abort"],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn approval() -> Value {
        json!({
            "noticeId": "notice-1",
            "actions": [
                { "actionId": "allow-once", "label": "允许一次", "style": "primary" },
                { "actionId": "reject", "label": "拒绝", "style": "secondary" },
            ],
        })
    }

    #[test]
    fn a_notice_is_tracked_with_exactly_the_actions_it_offered() {
        let mut queue = NoticeQueue::new();
        let tracked = queue.track(&approval());
        assert_eq!(tracked.notice_id, "notice-1");
        assert_eq!(tracked.action_ids, vec!["allow-once", "reject"]);
        assert_eq!(
            queue.action_ids("notice-1"),
            Some(["allow-once".to_string(), "reject".to_string()].as_slice())
        );
        assert!(queue.contains("notice-1"));
        assert_eq!(queue.len(), 1);
        assert!(queue.pop("notice-1").is_some());
        assert!(queue.is_empty());
        assert!(queue.pop("notice-1").is_none());
    }

    #[test]
    fn the_browser_action_id_wins_when_the_notice_offers_it() {
        let mut queue = NoticeQueue::new();
        let tracked = queue.track(&approval());
        assert_eq!(
            action_for(&tracked, None, Some("allow-once")).expect("action"),
            "allow-once"
        );
        // The decision is ignored when an explicit action id is present and valid.
        assert_eq!(
            action_for(&tracked, Some(Decision::Reject), Some("allow-once")).expect("action"),
            "allow-once"
        );
        let error = action_for(&tracked, None, Some("delete-everything")).unwrap_err();
        assert_eq!(
            error,
            AnswerError::NoSuchAction {
                action: String::from("delete-everything"),
                available: String::from("allow-once, reject"),
            }
        );
    }

    #[test]
    fn the_closed_decision_vocabulary_is_translated_into_the_runtimes_own_actions() {
        let mut queue = NoticeQueue::new();
        let tracked = queue.track(&approval());
        assert_eq!(
            action_for(&tracked, Some(Decision::Approve), None).expect("approve"),
            "allow-once"
        );
        assert_eq!(
            action_for(&tracked, Some(Decision::Reject), None).expect("reject"),
            "reject"
        );
        // codex-style ids still work unchanged.
        let codex = Interaction {
            notice_id: String::from("n2"),
            action_ids: vec![
                String::from("approve"),
                String::from("approve_for_session"),
                String::from("reject"),
            ],
            input_schema: None,
        };
        assert_eq!(
            action_for(&codex, Some(Decision::ApproveForSession), None).expect("session"),
            "approve_for_session"
        );
    }

    #[test]
    fn a_decision_with_no_matching_button_is_refused_not_guessed() {
        let question = Interaction {
            notice_id: String::from("q1"),
            action_ids: vec![String::from("submit"), String::from("cancel")],
            input_schema: None,
        };
        let error = action_for(&question, Some(Decision::ApproveForSession), None).unwrap_err();
        assert!(matches!(error, AnswerError::NoSuchAction { .. }));
        assert_eq!(
            action_for(&question, None, None).unwrap_err(),
            AnswerError::Empty
        );
    }
}

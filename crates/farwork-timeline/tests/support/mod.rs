#![allow(dead_code)]

use farwork_timeline::{ItemInput, ItemStatus, Role, TimelineItemKind};
use serde_json::json;

pub const NOW: &str = "2026-02-03T04:05:06Z";
pub const LATER: &str = "2026-02-03T05:00:00Z";
pub const SESSION: &str = "session-1";

pub fn message(id: &str) -> ItemInput {
    ItemInput::new(id, TimelineItemKind::Message, json!({ "text": id }))
        .with_status(ItemStatus::Done)
        .with_role(Role::Assistant)
}

pub fn message_saying(id: &str, text: &str) -> ItemInput {
    ItemInput::new(id, TimelineItemKind::Message, json!({ "text": text }))
        .with_status(ItemStatus::Done)
        .with_role(Role::Assistant)
}

pub fn tool(id: &str) -> ItemInput {
    ItemInput::new(id, TimelineItemKind::Tool, json!({ "name": id }))
        .with_status(ItemStatus::Running)
        .with_role(Role::Tool)
}

pub fn ids(items: &[farwork_timeline::TimelineItem]) -> Vec<&str> {
    items.iter().map(|item| item.id.as_str()).collect()
}

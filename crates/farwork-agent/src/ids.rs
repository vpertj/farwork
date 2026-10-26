//! Identifier generation for agent-owned objects.
//!
//! Only item ids and notice ids are minted here: the hub owns streamId, session
//! ids and timeline ordering, so the agent must never invent those.

use uuid::Uuid;

pub fn item_id() -> String {
    format!("itm_{}", Uuid::new_v4().simple())
}

pub fn notice_id() -> String {
    format!("ntc_{}", Uuid::new_v4().simple())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ids_are_prefixed_and_unique() {
        let items: HashSet<String> = (0..64).map(|_| item_id()).collect();
        assert_eq!(items.len(), 64);
        assert!(items.iter().all(|id| id.starts_with("itm_")));
        let notices: HashSet<String> = (0..64).map(|_| notice_id()).collect();
        assert_eq!(notices.len(), 64);
        assert!(notices.iter().all(|id| id.starts_with("ntc_")));
    }
}

//! The last-N window over messages, newest last in, oldest first out.

use crate::model::{Message, Role};

/// Keep the last `n` messages matching `role`, preserving order.
/// `n == 0` keeps nothing.
pub fn last_n(messages: &[Message], n: usize, role: Option<Role>) -> Vec<Message> {
    let kept: Vec<Message> = messages
        .iter()
        .filter(|m| role.is_none_or(|r| m.role == r))
        .cloned()
        .collect();
    kept[kept.len().saturating_sub(n)..].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: Role, text: &str) -> Message {
        Message {
            role,
            text: text.to_string(),
            ts: None,
        }
    }

    #[test]
    fn window_keeps_order() {
        let all = vec![
            msg(Role::User, "a"),
            msg(Role::Assistant, "b"),
            msg(Role::User, "c"),
            msg(Role::Assistant, "d"),
        ];
        let got = last_n(&all, 2, None);
        assert_eq!(got, vec![msg(Role::User, "c"), msg(Role::Assistant, "d")]);
    }

    #[test]
    fn role_filters_before_windowing() {
        let all = vec![
            msg(Role::User, "a"),
            msg(Role::Assistant, "b"),
            msg(Role::User, "c"),
        ];
        let got = last_n(&all, 10, Some(Role::User));
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|m| m.role == Role::User));
    }

    #[test]
    fn zero_keeps_nothing() {
        let all = vec![msg(Role::User, "a")];
        assert!(last_n(&all, 0, None).is_empty());
    }
}

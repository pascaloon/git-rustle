//! Structured rebase drafts. Full IDs and stable keys survive every edit/move.
use crate::git::operations::Plan;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum CommitAction {
    #[default]
    Pick,
    Reword,
    Squash,
    Fixup,
    Edit,
    Drop,
}
impl CommitAction {
    pub const ALL: [Self; 6] = [
        Self::Pick,
        Self::Reword,
        Self::Squash,
        Self::Fixup,
        Self::Edit,
        Self::Drop,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::Pick => "pick",
            Self::Reword => "reword",
            Self::Squash => "squash",
            Self::Fixup => "fixup",
            Self::Edit => "edit",
            Self::Drop => "drop",
        }
    }
    pub fn description(self) -> &'static str {
        match self {
            Self::Pick => "Keep this commit as it is",
            Self::Reword => "Change the commit message",
            Self::Squash => "Combine changes and messages with the previous commit",
            Self::Fixup => "Combine changes; discard this message",
            Self::Edit => "Pause here to amend in your editor, then continue",
            Self::Drop => "Remove this commit from the rewritten history",
        }
    }
    pub fn combines(self) -> bool {
        matches!(self, Self::Squash | Self::Fixup)
    }
}

pub(super) struct Entry {
    pub key: usize,
    pub id: String,
    pub subject: String,
    pub message: String,
    pub action: CommitAction,
    pub target: Option<usize>,
    pub error: Option<&'static str>,
}
#[derive(Default)]
pub(super) struct Draft {
    pub entries: Vec<Entry>,
    pub summary: String,
    pub errors: usize,
}
impl Draft {
    pub fn new(plan: &Plan) -> Self {
        let entries = plan
            .text
            .lines()
            .enumerate()
            .filter_map(|(key, line)| {
                let mut fields = line.splitn(3, ' ');
                fields.next()?;
                let id = fields.next()?.to_string();
                let subject = fields.next().unwrap_or_default().to_string();
                Some(Entry {
                    key,
                    id,
                    message: subject.clone(),
                    subject,
                    action: CommitAction::Pick,
                    target: None,
                    error: None,
                })
            })
            .collect();
        let mut draft = Self {
            entries,
            ..Self::default()
        };
        draft.refresh();
        draft
    }
    pub fn index(&self, key: usize) -> Option<usize> {
        self.entries.iter().position(|e| e.key == key)
    }
    pub fn set_action(&mut self, key: usize, action: CommitAction) {
        if let Some(i) = self.index(key) {
            self.entries[i].action = action;
            self.refresh();
        }
    }
    pub fn move_entry(&mut self, key: usize, direction: isize) -> Option<usize> {
        let from = self.index(key)?;
        let to = from.checked_add_signed(direction)?;
        if to >= self.entries.len() {
            return None;
        }
        self.entries.swap(from, to);
        self.refresh();
        Some(to)
    }
    pub fn refresh(&mut self) {
        let mut target = None;
        let mut dropped = 0;
        let mut combined = 0;
        self.errors = 0;
        for (i, entry) in self.entries.iter_mut().enumerate() {
            entry.target = target;
            entry.error = if entry.action.combines() && target.is_none() {
                Some("Needs an earlier kept commit. Move down or choose pick.")
            } else if entry.action == CommitAction::Reword && entry.message.trim().is_empty() {
                Some("Enter a new commit message.")
            } else {
                None
            };
            self.errors += usize::from(entry.error.is_some());
            match entry.action {
                CommitAction::Drop => dropped += 1,
                CommitAction::Squash | CommitAction::Fixup => combined += 1,
                _ => target = Some(i),
            }
        }
        self.summary = format!(
            "{} commits  ·  {} kept  ·  {combined} combined  ·  {dropped} dropped",
            self.entries.len(),
            self.entries.len() - dropped - combined
        );
    }
    pub fn text(&self) -> String {
        let mut text = String::new();
        for entry in &self.entries {
            text.push_str(entry.action.name());
            text.push(' ');
            text.push_str(&entry.id);
            text.push(' ');
            text.push_str(if entry.action == CommitAction::Reword {
                entry.message.trim()
            } else {
                &entry.subject
            });
            text.push('\n');
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn draft() -> Draft {
        Draft::new(&Plan { text: "pick aaaaaaaa First commit\npick bbbbbbbb Second commit\npick cccccccc Third commit\n".into(), ..Plan::default() })
    }
    #[test]
    fn moves_keep_identity_and_reword_message_with_commit() {
        let mut d = draft();
        d.set_action(1, CommitAction::Reword);
        d.entries[1].message = "New message with spaces".into();
        d.move_entry(1, -1);
        d.set_action(0, CommitAction::Fixup);
        d.set_action(2, CommitAction::Drop);
        assert_eq!(
            d.text(),
            "reword bbbbbbbb New message with spaces\nfixup aaaaaaaa First commit\ndrop cccccccc Third commit\n"
        );
        assert_eq!(d.errors, 0);
        assert_eq!(d.entries[1].target, Some(0));
        assert_eq!(d.move_entry(1, -1), None);
        assert_eq!(d.move_entry(2, 1), None);
        assert_eq!(d.entries.len(), 3);
    }
    #[test]
    fn combined_chains_skip_dropped_commits_and_revalidate_after_move() {
        let mut d = draft();
        d.set_action(1, CommitAction::Drop);
        d.set_action(2, CommitAction::Squash);
        assert_eq!(d.entries[2].target, Some(0));
        d.move_entry(2, -1);
        d.move_entry(2, -1);
        assert_eq!(d.errors, 1);
        d.move_entry(2, 1);
        assert_eq!(d.errors, 0);
        d.set_action(1, CommitAction::Fixup);
        assert_eq!(d.entries[2].target, Some(0));
        d.set_action(0, CommitAction::Drop);
        assert_eq!(d.errors, 2);
    }
    #[test]
    fn empty_reword_is_blocked_but_all_drop_is_valid() {
        let mut d = draft();
        d.set_action(0, CommitAction::Reword);
        d.entries[0].message = "  ".into();
        d.refresh();
        assert_eq!(d.errors, 1);
        for key in 0..3 {
            d.set_action(key, CommitAction::Drop);
        }
        assert_eq!(d.errors, 0);
        assert!(d.summary.contains("0 kept"));
    }
}

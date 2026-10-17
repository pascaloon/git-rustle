//! Cached, selectable sections for the commit inspector.
use crate::desktop_text::{Document, TextRole};
use crate::git::CommitDetails;
use crate::ui::time::format_absolute;

pub fn document(details: &CommitDetails) -> Document {
    let mut doc = Document::default();
    let message = details.message.trim_end();
    let (subject, body) = message.split_once('\n').unwrap_or((message, ""));
    doc.push(
        TextRole::Title,
        if subject.is_empty() {
            "Untitled commit"
        } else {
            subject
        },
    );
    if !body.trim().is_empty() {
        doc.push(TextRole::Body, body.trim_start_matches('\n'));
    }
    let same_identity = details.author_name == details.committer_name
        && details.author_email == details.committer_email;
    let authored = format_absolute(details.author_time, details.author_offset);
    let committed = format_absolute(details.commit_time, details.commit_offset);
    doc.push(
        TextRole::Label,
        if same_identity {
            "AUTHOR & COMMITTER"
        } else {
            "AUTHOR"
        },
    );
    doc.push(TextRole::Value, &details.author_name);
    doc.push(TextRole::Muted, &details.author_email);
    if same_identity && authored != committed {
        doc.push(
            TextRole::Muted,
            &format!("Authored   {authored}\nCommitted  {committed}"),
        );
    } else {
        doc.push(TextRole::Muted, &authored);
    }
    if !same_identity {
        doc.push(TextRole::Label, "COMMITTER");
        doc.push(TextRole::Value, &details.committer_name);
        doc.push(TextRole::Muted, &details.committer_email);
        doc.push(TextRole::Muted, &committed);
    }
    doc.push(TextRole::Label, "COMMIT");
    doc.push(TextRole::Hash, &details.id.to_string());
    doc.push(
        TextRole::Label,
        if details.parents.len() == 1 {
            "PARENT"
        } else {
            "PARENTS"
        },
    );
    if details.parents.is_empty() {
        doc.push(TextRole::Muted, "Root commit · no parents");
    } else {
        for parent in &details.parents {
            doc.push(TextRole::Hash, &parent.to_string());
        }
    }
    doc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_sections_keep_copy_offsets_and_distinct_signatures() {
        let details = CommitDetails {
            id: gix::ObjectId::null(gix::hash::Kind::Sha1),
            parents: vec![],
            author_name: "Zoë 🦀".into(),
            author_email: "author@example.invalid".into(),
            author_time: 1,
            author_offset: 0,
            committer_name: "René".into(),
            committer_email: "committer@example.invalid".into(),
            commit_time: 2,
            commit_offset: 3600,
            message: "Fix café\n\n  keep indentation\n第二行\n".into(),
        };
        let doc = document(&details);
        assert!(doc.text.contains("  keep indentation\n第二行"));
        assert!(doc.text.contains("Zoë 🦀"));
        assert!(doc.text.contains("René"));
        assert!(doc.text.contains("Root commit"));
        for section in &doc.sections {
            assert_eq!(&doc.text[section.range.clone()], section.text.as_ref());
        }
        let mut identical = details;
        identical.committer_name = identical.author_name.clone();
        identical.committer_email = identical.author_email.clone();
        identical.commit_time = identical.author_time;
        identical.commit_offset = identical.author_offset;
        assert!(document(&identical).text.contains("AUTHOR & COMMITTER"));
        identical.commit_time += 600;
        let doc = document(&identical);
        assert!(doc.text.contains("Authored   1970-01-01 00:00 +0000"));
        assert!(doc.text.contains("Committed  1970-01-01 00:10 +0000"));
    }
}

//! Frozen, explicitly ordered commit replay using the shared recovery journal.
use super::*;

pub(super) fn apply(
    context: &Context,
    commits: &[String],
    destination: Option<(&str, &str)>,
    replay_started: &mut bool,
) -> Result<()> {
    apply_with_before_run(context, commits, destination, replay_started, || {
        #[cfg(test)]
        BEFORE_RUN.with(|hook| {
            if let Some(callback) = hook.borrow_mut().take() {
                callback();
            }
        });
    })
}

pub(super) fn apply_with_before_run(
    context: &Context,
    commits: &[String],
    destination: Option<(&str, &str)>,
    replay_started: &mut bool,
    before_run: impl FnOnce(),
) -> Result<()> {
    if commits.is_empty() {
        return Err("Select at least one commit to cherry-pick.".into());
    }
    clean_worktree(&context.root)?;
    let repo = super::super::open(&context.root).map_err(err)?;
    let mut seen = BTreeSet::new();
    for value in commits {
        let id = gix::ObjectId::from_hex(value.as_bytes()).map_err(err)?;
        if value.bytes().any(|byte| byte.is_ascii_uppercase()) || !seen.insert(id) {
            return Err("Cherry-pick requires distinct, full commit object IDs.".into());
        }
        let detail = super::super::commit_details(&repo, &id).map_err(err)?;
        if detail.parents.len() > 1 {
            return Err("Merge commits require choosing a mainline parent; cherry-pick non-merge commits instead.".into());
        }
    }
    // Supplying individual OIDs preserves the frozen order; Git must not walk a
    // range or resolve branch names after the user reviews the confirmation.
    let mut args = vec!["cherry-pick", "--no-edit", "--no-gpg-sign", "--"];
    args.extend(commits.iter().map(String::as_str));
    before_run();
    if let Some((branch, head)) = destination
        && (output(&context.root, &["symbolic-ref", "--short", "HEAD"])? != branch
            || revision(&context.root, "HEAD")? != head)
    {
        return Err("The current branch changed during cherry-pick preparation. Review the new destination before retrying.".into());
    }
    *replay_started = true;
    run(&context.root, &args, None, &editor_env())?;
    Ok(())
}

#[cfg(test)]
thread_local! {
    static BEFORE_RUN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}
#[cfg(test)]
pub(super) fn set_before_run(callback: impl FnOnce() + 'static) {
    BEFORE_RUN.with(|hook| *hook.borrow_mut() = Some(Box::new(callback)));
}

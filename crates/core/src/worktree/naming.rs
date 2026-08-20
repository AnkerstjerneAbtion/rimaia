//! The branch name for a task's worktree: `rimaia/<task-id>-<slug>`
//! (ADR-0005).
//!
//! The task id makes the name unique; the slug is there so the branch is
//! readable in a PR list. Collisions are resolved by suffixing, never by
//! reuse — reusing a branch silently continues someone else's work.
//!
//! # Why the alphabet is this narrow, rather than checked afterwards
//!
//! `git check-ref-format` refuses a ref name that contains an ASCII control
//! character, a space, or any of `~ ^ : ? * [ \`; that contains `..`, `//` or
//! `@{`; that has a path component beginning with `.` or ending in `.lock`;
//! that ends in `.` or `/`; or that is the single character `@`. Producing a
//! candidate and validating it against that list means encoding the list
//! twice — once to check, once to repair — and the failure when the repair is
//! wrong is git rejecting a ref for a reason the user cannot see.
//!
//! Restricting the slug to lowercase ASCII alphanumerics separated by single
//! hyphens makes every one of those rules unreachable instead. With no `.`
//! there is no `..`, no `.lock` suffix and no component starting or ending
//! with a dot. With no `/` the name is always exactly the two components
//! `rimaia` and the rest, so it can neither end in a slash nor contain `//`.
//! With no `@` there is no `@{`, and the name is never `@` alone. And because
//! every byte is one ASCII character, truncation is safe at *any* byte —
//! which is the whole reason the alphabet was chosen rather than inherited.
//!
//! What this cannot rule out is a directory/file conflict with a branch a
//! *human* created — `rimaia/<id>-<slug>/something` would make the name we
//! want a directory. Git refuses that with its own message, which reaches the
//! user through [`super::git::checked`].
//!
//! `pub(super)`: [`crate::worktree`]'s implementation detail. Not
//! [`crate::repo`]'s `slugify`, which is `pub(super)` to that module and
//! answers a different question — a filesystem path segment, with no length
//! budget and a `"repository"` fallback that would be a nonsense branch name.

/// Every Rimaia branch lives under this, so `git branch --list 'rimaia/*'`
/// finds all of them and nothing else.
const BRANCH_NAMESPACE: &str = "rimaia/";

/// The longest branch name Rimaia will produce.
///
/// Git imposes no limit of its own, but a loose ref is a file at
/// `.git/refs/<name>` and updating it writes that path plus `.lock`, so every
/// component has to fit the filesystem's own per-component limit — 255 bytes
/// on ext4, APFS and NTFS alike. 200 clears that with room for the suffix and
/// keeps the name short enough to read in a PR list, which is the point of
/// having a slug at all.
const MAX_BRANCH_NAME_BYTES: usize = 200;

/// Bytes held back from the slug so that appending a collision suffix never
/// has to re-truncate, and so the suffixed name is still under the limit.
/// Enough for `-999999`, far past [`MAX_COLLISION_ATTEMPTS`].
const COLLISION_SUFFIX_BYTES: usize = 8;

/// How many suffixes to try before giving up. A collision needs a branch whose
/// name already contains this task's UUID, so reaching even two is a repository
/// somebody has been hand-editing; a bound exists so a pathological repository
/// cannot spin here forever.
pub(super) const MAX_COLLISION_ATTEMPTS: u32 = 100;

/// What a title with nothing sluggable in it becomes — a title written
/// entirely in a script with no ASCII letters or digits, or one made of
/// punctuation. The id still carries the uniqueness; this only keeps the name
/// from ending in a bare hyphen.
const UNSLUGGABLE_TITLE: &str = "task";

/// `rimaia/<task-id>-<slug>`, with the slug truncated so the whole name fits
/// [`MAX_BRANCH_NAME_BYTES`] even after a collision suffix.
///
/// A task id long enough to leave no budget at all yields `rimaia/<task-id>`
/// with no slug, which is still a valid, unique ref.
pub(super) fn branch_name(task_id: &str, title: &str) -> String {
    let stem = format!("{BRANCH_NAMESPACE}{task_id}");
    // `+ 1` for the hyphen that would join the slug on.
    let budget = MAX_BRANCH_NAME_BYTES.saturating_sub(stem.len() + 1 + COLLISION_SUFFIX_BYTES);

    match slug(title, budget) {
        Some(slug) => format!("{stem}-{slug}"),
        None => stem,
    }
}

/// The `n`th alternative to a taken branch name.
///
/// Numeric, appended, and never a replacement: task 007's Scope is explicit
/// that a collision is "resolved by numeric suffix, never by reuse", because
/// checking out a branch somebody else's run created would continue their work
/// under this task's name.
pub(super) fn with_collision_suffix(branch: &str, attempt: u32) -> String {
    format!("{branch}-{attempt}")
}

/// A `[a-z0-9-]` slug of `title` no longer than `budget` bytes, or `None` when
/// `budget` leaves no room for one.
fn slug(title: &str, budget: usize) -> Option<String> {
    if budget == 0 {
        return None;
    }

    let mut slug = String::with_capacity(budget.min(title.len()));
    // Seeded true so a leading separator is dropped rather than hyphenated.
    let mut last_was_hyphen = true;
    for ch in title.chars() {
        if slug.len() >= budget {
            break;
        }
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_was_hyphen = false;
        } else if !last_was_hyphen {
            slug.push('-');
            last_was_hyphen = true;
        }
    }

    // Truncation can land on the hyphen the loop had just written, and a
    // trailing hyphen is ugly rather than illegal — but it also makes the
    // collision suffix read as `...--2`.
    while slug.ends_with('-') {
        slug.pop();
    }

    if slug.is_empty() {
        return (budget >= UNSLUGGABLE_TITLE.len()).then(|| UNSLUGGABLE_TITLE.to_string());
    }
    Some(slug)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// A hyphenated UUID, the shape seam-contract D10 fixes every id to.
    const TASK_ID: &str = "3f2b1c00-0000-4000-8000-000000000001";

    /// The rules `git check-ref-format` enforces, asserted against a produced
    /// name rather than assumed from the alphabet — the module doc argues the
    /// alphabet makes them unreachable, and this is what would notice if the
    /// alphabet ever widened.
    fn assert_is_a_legal_ref_name(branch: &str) {
        assert!(!branch.is_empty(), "{branch}: empty");
        assert!(
            branch.len() <= MAX_BRANCH_NAME_BYTES,
            "{branch}: {} bytes is over the limit",
            branch.len()
        );
        assert!(!branch.contains(".."), "{branch}: contains ..");
        assert!(!branch.contains("//"), "{branch}: contains //");
        assert!(!branch.contains("@{"), "{branch}: contains @{{");
        assert!(!branch.ends_with('.'), "{branch}: ends with .");
        assert!(!branch.ends_with('/'), "{branch}: ends with /");
        assert!(!branch.ends_with(".lock"), "{branch}: ends with .lock");
        assert!(branch != "@", "{branch}: is the bare @");
        for component in branch.split('/') {
            assert!(!component.is_empty(), "{branch}: empty component");
            assert!(
                !component.starts_with('.'),
                "{branch}: component starts with ."
            );
            assert!(
                !component.ends_with(".lock"),
                "{branch}: component ends with .lock"
            );
        }
        for ch in branch.chars() {
            assert!(
                !ch.is_ascii_control() && !" ~^:?*[\\".contains(ch),
                "{branch}: illegal character {ch:?}"
            );
        }
    }

    #[test]
    fn a_branch_name_is_the_namespace_the_task_id_and_a_slug_of_the_title() {
        assert_eq!(
            branch_name(TASK_ID, "Wire the board to the store"),
            "rimaia/3f2b1c00-0000-4000-8000-000000000001-wire-the-board-to-the-store"
        );
    }

    #[test]
    fn punctuation_and_repeated_separators_collapse_to_one_hyphen() {
        assert_eq!(
            branch_name(TASK_ID, "Fix:  the   parser!!"),
            "rimaia/3f2b1c00-0000-4000-8000-000000000001-fix-the-parser"
        );
    }

    #[test]
    fn characters_git_refuses_in_a_ref_never_survive_into_the_name() {
        // Every one of these is on `git check-ref-format`'s list, and a title
        // is free text a user typed.
        let branch = branch_name(TASK_ID, "a~b^c:d?e*f[g\\h..i@{j.lock");

        assert_is_a_legal_ref_name(&branch);
        assert_eq!(
            branch,
            "rimaia/3f2b1c00-0000-4000-8000-000000000001-a-b-c-d-e-f-g-h-i-j-lock"
        );
    }

    #[test]
    fn a_title_far_over_the_limit_truncates_to_a_name_git_still_accepts() {
        let branch = branch_name(TASK_ID, &"supercalifragilistic ".repeat(60));

        assert_is_a_legal_ref_name(&branch);
        assert!(
            branch.len() + COLLISION_SUFFIX_BYTES <= MAX_BRANCH_NAME_BYTES,
            "a truncated name must still leave room for a collision suffix, got {}",
            branch.len()
        );
        assert!(branch.starts_with(&format!("rimaia/{TASK_ID}-supercalifragilistic-")));
    }

    #[test]
    fn truncation_never_leaves_a_trailing_hyphen() {
        // The budget lands mid-separator for some word length; sweeping the
        // lengths finds it without hard-coding which one it is.
        for words in 1..80 {
            let branch = branch_name(TASK_ID, &"alpha beta ".repeat(words));
            assert!(
                !branch.ends_with('-'),
                "{branch} ends with a hyphen at {words} repetitions"
            );
        }
    }

    #[test]
    fn a_title_with_nothing_sluggable_falls_back_to_a_placeholder() {
        assert_eq!(
            branch_name(TASK_ID, "☕☕☕"),
            "rimaia/3f2b1c00-0000-4000-8000-000000000001-task"
        );
    }

    #[test]
    fn a_task_id_that_leaves_no_budget_yields_a_name_with_no_slug_at_all() {
        let long_id = "x".repeat(MAX_BRANCH_NAME_BYTES);

        let branch = branch_name(&long_id, "Wire the board");

        assert_eq!(branch, format!("rimaia/{long_id}"));
    }

    #[test]
    fn a_collision_suffix_is_appended_and_never_replaces_the_name() {
        let base = branch_name(TASK_ID, "Wire the board");

        assert_eq!(with_collision_suffix(&base, 2), format!("{base}-2"));
        assert_is_a_legal_ref_name(&with_collision_suffix(&base, MAX_COLLISION_ATTEMPTS));
    }

    #[test]
    fn a_suffixed_truncated_name_is_still_under_the_limit() {
        let branch = branch_name(TASK_ID, &"lorem ipsum dolor ".repeat(40));

        let suffixed = with_collision_suffix(&branch, MAX_COLLISION_ATTEMPTS);

        assert_is_a_legal_ref_name(&suffixed);
    }
}

//! Deriving a repository's display name and worktree-root slug from its
//! directory (task 003's "derived display name from the directory name;
//! editable").
//!
//! `pub(super)`: [`crate::repo`]'s implementation detail.

use std::path::Path;

/// The directory's own basename, used verbatim as the first guess at a
/// display name. The user can rename afterwards through
/// [`RepositoryPatch`](super::RepositoryPatch); this only supplies the
/// default the "add repository" dialog shows before any edit.
pub(super) fn derive_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// A filesystem-safe slug for `<worktrees_dir>/<slug>` (ADR-0005): lowercase
/// ASCII alphanumerics, with every run of anything else collapsed to a
/// single hyphen and no leading or trailing hyphen. A name that slugifies to
/// nothing (for example a directory named entirely in a script with no ASCII
/// letters or digits) falls back to the literal string `"repository"` rather
/// than handing the caller an empty path segment.
pub(super) fn slugify(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    let mut last_was_hyphen = true; // seeded true so a leading separator is dropped, not hyphenated
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_was_hyphen = false;
        } else if !last_was_hyphen {
            slug.push('-');
            last_was_hyphen = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }

    if slug.is_empty() {
        "repository".to_string()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn a_name_is_derived_from_the_last_path_component() {
        assert_eq!(
            derive_name(Path::new("/Users/someone/Code/rimaia")),
            "rimaia"
        );
    }

    #[test]
    fn a_name_derived_from_a_path_with_a_trailing_slash_still_reads_the_directory() {
        assert_eq!(
            derive_name(Path::new("/Users/someone/Code/rimaia/")),
            "rimaia"
        );
    }

    #[test]
    fn spaces_slugify_to_a_single_hyphen() {
        assert_eq!(slugify("work tree"), "work-tree");
    }

    #[test]
    fn punctuation_and_repeated_separators_collapse_to_one_hyphen() {
        assert_eq!(slugify("My--Project!!"), "my-project");
    }

    #[test]
    fn leading_and_trailing_separators_are_dropped() {
        assert_eq!(slugify("  Rimaia  "), "rimaia");
    }

    #[test]
    fn a_name_with_nothing_slugifiable_falls_back_to_a_placeholder() {
        assert_eq!(slugify("☕☕☕"), "repository");
    }

    #[test]
    fn an_already_slug_shaped_name_is_left_alone() {
        assert_eq!(slugify("already-a-slug"), "already-a-slug");
    }
}

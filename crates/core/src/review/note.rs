//! The review note a reject or a request for changes leaves on a task.
//!
//! A pure function, and its output is a contract: the next run reads the note as
//! ordinary extra instructions (ADR-0009), so the text below is what the prompt
//! says about a review, and what the review screen's copy refers to.

/// Which verdict a note was written under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Rejected,
    ChangesRequested,
}

impl Verdict {
    /// Past tense on purpose. Notes accumulate over several rounds, and a header
    /// has to stay true when a later run reads it after another verdict has
    /// followed it.
    const fn header(self) -> &'static str {
        match self {
            Verdict::ChangesRequested => {
                "Review note (changes requested; the reviewed commits were kept, so build on them):"
            }
            Verdict::Rejected => {
                "Review note (rejected; the task restarted on a fresh branch without those commits):"
            }
        }
    }
}

/// `existing` extra instructions with a review block appended.
///
/// The block is the header, a newline and the trimmed note, with newlines inside
/// the note kept byte for byte. It follows existing text after one blank line,
/// and stands alone when there is nothing but whitespace before it.
pub fn append(existing: Option<&str>, verdict: Verdict, note: &str) -> String {
    let block = format!("{}\n{}", verdict.header(), note.trim());
    match existing.map(str::trim_end).filter(|text| !text.is_empty()) {
        Some(text) => format!("{text}\n\n{block}"),
        None => block,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const CHANGES: &str =
        "Review note (changes requested; the reviewed commits were kept, so build on them):";
    const REJECTED: &str =
        "Review note (rejected; the task restarted on a fresh branch without those commits):";

    #[test]
    fn a_review_note_on_empty_extra_instructions_is_the_block_alone() {
        for existing in [None, Some(""), Some("  \n\n")] {
            assert_eq!(
                append(existing, Verdict::ChangesRequested, "Add a test."),
                format!("{CHANGES}\nAdd a test."),
            );
            assert_eq!(
                append(existing, Verdict::Rejected, "Start over."),
                format!("{REJECTED}\nStart over."),
            );
        }
    }

    #[test]
    fn a_review_note_is_separated_from_existing_instructions_by_one_blank_line() {
        assert_eq!(
            append(
                Some("Keep the public API unchanged.\n"),
                Verdict::ChangesRequested,
                "  The migration must be reversible.\n",
            ),
            "Keep the public API unchanged.\n\nReview note (changes requested; the reviewed commits were kept, so build on them):\nThe migration must be reversible.",
        );
        assert_eq!(
            append(Some("Keep it small."), Verdict::Rejected, "Wrong approach."),
            format!("Keep it small.\n\n{REJECTED}\nWrong approach."),
        );
    }

    #[test]
    fn trailing_whitespace_is_trimmed_before_a_note_is_appended() {
        assert_eq!(
            append(
                Some("Keep it small.  \n\n\n"),
                Verdict::Rejected,
                "\n\tNo.  \n"
            ),
            format!("Keep it small.\n\n{REJECTED}\nNo."),
        );
        // Leading whitespace of the existing text is the author's, and stays.
        assert_eq!(
            append(Some("  indented"), Verdict::Rejected, "No."),
            format!("  indented\n\n{REJECTED}\nNo."),
        );
    }

    #[test]
    fn two_review_notes_append_in_the_order_they_were_given() {
        let first = append(None, Verdict::ChangesRequested, "First.\nSecond line.");
        let second = append(Some(&first), Verdict::Rejected, "Third.");
        assert_eq!(
            second,
            format!("{CHANGES}\nFirst.\nSecond line.\n\n{REJECTED}\nThird."),
        );
    }
}

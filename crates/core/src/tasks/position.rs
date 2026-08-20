//! Fractional board ordering (ADR-0007's Ordering section, seam-contract D1).
//!
//! `position` is a float within `(repository, column)`. Inserting between two
//! cards takes the midpoint and rewrites no neighbours; a rebalance
//! renormalizes a column to evenly spaced integers when the gap between two
//! neighbours gets too close to represent. Board order *is* execution order —
//! there is no separate priority field.
//!
//! The two halves live in one file because they share a caller's obligation:
//! whoever calls [`position_between`] and gets [`Placement::NeedsRebalance`]
//! must call [`rebalance_column`] and try again, in the same transaction. Task
//! 004's `move_task` owns that transaction and the neighbour lookup; this
//! module owns only the numbers.

use sqlx::SqliteConnection;

use crate::db::BoardColumn;
use crate::error::Result;

/// How far a new edge position steps away from its only neighbour, and the
/// spacing [`rebalanced_positions`] renormalizes a column to.
///
/// A constant step rather than halving toward a bound: subtraction never
/// loses precision and a negative position sorts perfectly well below zero,
/// whereas halving toward zero would spend the mantissa on the one operation
/// here that has genuinely unlimited room.
pub const POSITION_STEP: f64 = 1.0;

/// The smallest gap [`position_between`] will still split.
///
/// Twenty successive drops into the same slot — always inserting right above
/// the same lower neighbour, so the gap halves each time — close a
/// `POSITION_STEP` gap to `1.0 / 2^20`, which is `9.536743...e-7`: just under
/// this threshold. That pins the number at twenty rather than some other
/// count, and
/// [`twenty_drops_into_the_same_slot_are_absorbed_before_a_rebalance_is_needed`](tests::twenty_drops_into_the_same_slot_are_absorbed_before_a_rebalance_is_needed)
/// asserts exactly that boundary, so changing either constant is a deliberate
/// edit to a failing test rather than a silent shift.
///
/// `9.5e-7` is about ten orders of magnitude above where f64 stops being able
/// to represent a value strictly between two neighbours near 1.0 (its spacing
/// there is `~2.2e-16`, `f64::EPSILON`). The arithmetic this crate does never
/// runs anywhere near the point where two neighbours have no representable
/// value between them — that failure mode is checked separately, in
/// [`position_between`]'s own midpoint comparison, for the case (large
/// magnitudes) where it can happen well above this threshold. This constant
/// is chosen for an early, predictable rebalance and a value that still
/// prints legibly to six decimal places in a log line, not because the float
/// format demands it here. SQLite's `REAL` is the same IEEE-754 binary64, so
/// nothing is lost between Rust and the database. Rebalancing this early is
/// one `UPDATE` over a column of tens of cards; waiting for the precision
/// cliff turns a routine renumber into a correctness bug that only shows up
/// at 2am.
pub const MIN_POSITION_GAP: f64 = 1e-6;

/// The result of trying to place a card between two neighbours.
///
/// Collapses four distinct conditions into one variant: a gap closed below
/// [`MIN_POSITION_GAP`], neighbours supplied out of order (or equal),
/// a non-finite position, and a midpoint that lands exactly on a neighbour
/// because f64 has nothing representable between them. All four mean the same
/// thing to a caller — renumber the column with [`rebalance_column`] and place
/// the card again — and collapsing them keeps the drag path from having to
/// tell a rare, legitimate condition apart from a bug. Three of the four are
/// only reachable from a database somebody edited by hand, which ADR-0003
/// says will happen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Placement {
    /// A representable value strictly between `before` and `after` (or past
    /// the edge of the column, when one side was absent).
    At(f64),
    /// No such value exists; the column needs [`rebalance_column`] before this
    /// call can be retried.
    NeedsRebalance,
}

/// Where a new card should land between `before` (the card above it in board
/// order) and `after` (the card below), or a signal to rebalance instead.
///
/// `None` on either side is the edge of the column: `(None, None)` is an
/// empty column, `(Some(_), None)` appends below the last card, `(None,
/// Some(_))` prepends above the first.
///
/// The midpoint is computed as `before + (after - before) / 2.0`, not
/// `(before + after) / 2.0`: the first form cannot overflow when both
/// operands are large and share a sign, and it is the more accurate of the
/// two near the extremes regardless.
pub fn position_between(before: Option<f64>, after: Option<f64>) -> Placement {
    match (before, after) {
        (None, None) => Placement::At(0.0),

        (Some(before), None) => {
            if !before.is_finite() {
                return Placement::NeedsRebalance;
            }
            let candidate = before + POSITION_STEP;
            at_if(candidate.is_finite() && candidate > before, candidate)
        }

        (None, Some(after)) => {
            if !after.is_finite() {
                return Placement::NeedsRebalance;
            }
            let candidate = after - POSITION_STEP;
            at_if(candidate.is_finite() && candidate < after, candidate)
        }

        (Some(before), Some(after)) => {
            if !before.is_finite() || !after.is_finite() {
                return Placement::NeedsRebalance;
            }
            let gap = after - before;
            if gap < MIN_POSITION_GAP {
                // Covers both "too small to represent" and "arrived out of
                // order" (a negative or zero gap is always below the
                // threshold), so callers see one outcome for both.
                return Placement::NeedsRebalance;
            }
            let midpoint = before + gap / 2.0;
            // Catches the case a plain gap check cannot: at large enough
            // magnitude, adjacent representable floats can be farther apart
            // than MIN_POSITION_GAP even though nothing lies between them.
            // Rounding then sends the midpoint straight back onto whichever
            // neighbour it is closer to, which this equality catches directly.
            at_if(
                midpoint.is_finite() && midpoint > before && midpoint < after,
                midpoint,
            )
        }
    }
}

fn at_if(condition: bool, value: f64) -> Placement {
    if condition {
        Placement::At(value)
    } else {
        Placement::NeedsRebalance
    }
}

/// Evenly spaced positions for a column of `count` cards, renumbered from
/// scratch after [`position_between`] reports [`Placement::NeedsRebalance`].
///
/// Integers (as `f64`), [`POSITION_STEP`] apart, starting at zero — the same
/// spacing a fresh sequence of appends would produce, so a column that has
/// just been rebalanced behaves exactly like one that never needed it.
pub fn rebalanced_positions(count: usize) -> Vec<f64> {
    (0..count)
        .map(|index| index as f64 * POSITION_STEP)
        .collect()
}

/// Renumbers every task in one `(repository_id, column)` to evenly spaced
/// positions, in the order they currently sort.
///
/// Takes a connection, not a pool, because the caller is normally partway
/// through a move that found no gap: the renumber and the insert that follows
/// it must be one transaction, or the MCP server and the scheduler can
/// interleave and put back the very collision this just removed. Callers pass
/// `&mut *tx`.
///
/// **That transaction is an obligation, and nothing here enforces it** —
/// `&mut SqliteConnection` is what a `Transaction` derefs to, so a pooled
/// connection in autocommit satisfies the signature just as well. Break it and
/// the loop below is N independent commits. A failure part-way through — a
/// `SQLITE_BUSY` that outlasts the pool's busy timeout, an I/O error — keeps
/// the rows already renumbered and abandons the rest. Negative positions are
/// ordinary, since `position_between(None, Some(x))` prepends at `x - 1.0`, so
/// three drags to the top of a column give `[-3.0, -2.0, -1.0]`; renumber that
/// and fail on the third `UPDATE` and the column reads `[0.0, 1.0, -1.0]` —
/// the card that was last is now first. **The column was correctly ordered
/// before the call.** Partial renumbering does not leave a precision problem
/// half-fixed, it turns one into a wrong execution order (ADR-0007), and that,
/// rather than an interleaving writer, is the reason not to "simplify" this
/// signature to take a `&SqlitePool`.
///
/// Ties are broken on `created_at` then `id`, so a column whose cards share a
/// position — the degenerate case this function exists to repair — comes out
/// deterministically ordered rather than in whatever order SQLite happened to
/// store them.
///
/// Does **not** stamp `updated_at`. Renumbering is not a change to any card,
/// and marking a whole column modified would make every client refresh for
/// nothing. Task 004 owns `updated_at`'s rule; revisit this with a clock in
/// hand if it decides renumbering should count.
pub async fn rebalance_column(
    conn: &mut SqliteConnection,
    repository_id: &str,
    column: BoardColumn,
) -> Result<usize> {
    // Binding `column: BoardColumn` directly works because sqlx's SQLite
    // driver is `ParamChecking::Weak`: the macro checks argument arity only,
    // and the derive on `BoardColumn` supplies `Encode` and `Type`.
    let ids = sqlx::query_scalar!(
        r#"
        SELECT id
        FROM tasks
        WHERE repository_id = ?1 AND board_column = ?2
        ORDER BY position ASC, created_at ASC, id ASC
        "#,
        repository_id,
        column,
    )
    .fetch_all(&mut *conn)
    .await?;

    for (id, position) in ids.iter().zip(rebalanced_positions(ids.len())) {
        sqlx::query!("UPDATE tasks SET position = ?1 WHERE id = ?2", position, id,)
            .execute(&mut *conn)
            .await?;
    }

    Ok(ids.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn an_empty_column_places_its_first_card_at_zero() {
        assert_eq!(position_between(None, None), Placement::At(0.0));
    }

    #[test]
    fn appending_below_the_last_card_steps_by_the_position_step() {
        assert_eq!(position_between(Some(5.0), None), Placement::At(6.0));
    }

    #[test]
    fn prepending_above_the_first_card_steps_back_by_the_position_step() {
        assert_eq!(position_between(None, Some(5.0)), Placement::At(4.0));
    }

    #[test]
    fn the_midpoint_between_two_cards_is_returned() {
        assert_eq!(position_between(Some(1.0), Some(3.0)), Placement::At(2.0));
    }

    #[test]
    fn a_gap_narrower_than_the_minimum_needs_a_rebalance() {
        assert_eq!(
            position_between(Some(1.0), Some(1.0 + 5e-7)),
            Placement::NeedsRebalance
        );
    }

    #[test]
    fn twenty_drops_into_the_same_slot_are_absorbed_before_a_rebalance_is_needed() {
        // Every drop targets the top of the column: `before` stays fixed at
        // 0.0 and each new card's position becomes the next `after`, so the
        // gap halves on every successful call. This pins MIN_POSITION_GAP's
        // budget — the constant's doc comment justifies 1e-6 with exactly
        // this arithmetic, so a change to either constant that moves the
        // threshold below or above twenty drops has to fail this test rather
        // than pass unnoticed.
        let before = 0.0;
        let mut after = 1.0;

        for attempt in 1..=20 {
            match position_between(Some(before), Some(after)) {
                Placement::At(midpoint) => after = midpoint,
                Placement::NeedsRebalance => {
                    panic!("rebalance requested on drop {attempt}, before the twentieth")
                }
            }
        }

        // Exact: every halving of a power-of-two fraction is exact in binary
        // floating point, so this is not an approximation.
        assert_eq!(after, 1.0 / 1_048_576.0, "gap after twenty drops");
        assert!(
            after < MIN_POSITION_GAP,
            "the gap must have closed below the threshold"
        );

        assert_eq!(
            position_between(Some(before), Some(after)),
            Placement::NeedsRebalance,
            "the twenty-first drop into the same slot must ask for a rebalance"
        );
    }

    #[test]
    fn neighbours_supplied_out_of_order_need_a_rebalance() {
        assert_eq!(
            position_between(Some(5.0), Some(2.0)),
            Placement::NeedsRebalance
        );
    }

    #[test]
    fn equal_neighbours_need_a_rebalance() {
        // The degenerate case rebalance_column exists to repair: a column
        // whose cards already share a position.
        assert_eq!(
            position_between(Some(3.0), Some(3.0)),
            Placement::NeedsRebalance
        );
    }

    #[test]
    fn a_non_finite_neighbour_needs_a_rebalance() {
        assert_eq!(
            position_between(Some(f64::NAN), Some(1.0)),
            Placement::NeedsRebalance
        );
        assert_eq!(
            position_between(Some(0.0), Some(f64::INFINITY)),
            Placement::NeedsRebalance
        );
        assert_eq!(
            position_between(Some(f64::NEG_INFINITY), None),
            Placement::NeedsRebalance
        );
    }

    #[test]
    fn a_gap_too_far_from_zero_to_hold_a_midpoint_needs_a_rebalance() {
        // Two adjacent representable f64 values: nothing lies between them no
        // matter the absolute size of the gap. At this magnitude the gap
        // itself (~2.2e284) is enormously larger than MIN_POSITION_GAP, so
        // only the post-rounding equality check in `position_between` — not
        // the gap threshold — can catch this.
        let before: f64 = 1e300;
        let after = f64::from_bits(before.to_bits() + 1);
        assert!(
            after - before > MIN_POSITION_GAP,
            "the fixture must not be caught by the gap threshold alone"
        );

        assert_eq!(
            position_between(Some(before), Some(after)),
            Placement::NeedsRebalance
        );
    }

    #[test]
    fn rebalanced_positions_are_evenly_spaced_integers() {
        assert_eq!(rebalanced_positions(5), vec![0.0, 1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn rebalancing_an_empty_column_produces_no_positions() {
        assert_eq!(rebalanced_positions(0), Vec::<f64>::new());
    }

    #[test]
    fn a_rebalanced_column_has_room_for_a_drop_between_every_pair() {
        let positions = rebalanced_positions(200);

        for pair in positions.windows(2) {
            let (before, after) = (pair[0], pair[1]);
            assert!(
                matches!(
                    position_between(Some(before), Some(after)),
                    Placement::At(_)
                ),
                "no room between {before} and {after}"
            );
        }
    }
}

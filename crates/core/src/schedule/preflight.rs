//! What a scheduled queue would do, answered before the user leaves.
//!
//! Task 013's scope asks for "a pre-flight summary before a scheduled queue
//! starts: which tasks will run, in what order, and which are blocked and why".
//! Its real product value is the *evening*: `preview_schedule_preflight` is a
//! button next to a schedule, pressed at 18:00, that answers the question the
//! user would otherwise answer at 09:00 by reading what did not happen.
//!
//! # It computes nothing of its own
//!
//! [`preview`] calls [`selection::plan`] verbatim and reports what it returns —
//! the same order, the same [`queue_position`], the same [`SkipReason`] and the
//! same [`explanation`]. Not "the same rules": literally the same function, the
//! one `try_step` itself calls on every pass. A second implementation of
//! eligibility that agreed with the first today is a second implementation that
//! disagrees with it in a month, and a preflight that lies is worse than no
//! preflight — the whole point is that the user trusts it enough to walk away.
//!
//! [`selection::plan`]: crate::scheduler::selection::plan
//! [`queue_position`]: crate::scheduler::QueueEntry::queue_position
//! [`SkipReason`]: crate::scheduler::SkipReason
//! [`explanation`]: crate::scheduler::SkipReason::explanation
//!
//! # Computed, never stored
//!
//! There is no preflight column and no preflight table. The answer is true of a
//! board at an instant, and a board changes — a card dragged at 21:55 changes
//! what runs at 22:00, which is exactly the property `selection::plan`'s own
//! doc calls out ("re-read from scratch on every pass, never a snapshot"). A
//! stored summary would be a promise the queue then breaks.
//!
//! # The name is deliberately not `plan`
//!
//! [`selection::plan`] is the queue's, and task 023 will add
//! `strategy::plan_all`. Three things called `plan` in one codebase, two of them
//! about different subjects, is how a reviewer ends up reading the wrong one.
//! [`PreflightSummary`] is a *report about* a plan, and it is named for that.

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::context::ServiceContext;
use crate::db::ScheduleMode;
use crate::error::Result;
use crate::schedule::{self, fire};
use crate::scheduler::selection::{self, QueueEntry};

/// What a schedule would do if it fired now.
///
/// Deliberately open to extension: task 023's review-and-fix loop will want to
/// say something here about what each entry is going to be *run with*, and this
/// struct is shaped so that is a field rather than a second endpoint.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreflightSummary {
    pub schedule_id: String,
    pub schedule_name: String,
    /// When this schedule next fires, or `None` for a one-off that already has.
    /// In the past when the schedule is overdue — see
    /// [`fire::next_fire_at`](crate::schedule::fire::next_fire_at).
    pub next_fire_at: Option<DateTime<Utc>>,
    /// When the window that fire opens would stop starting new tasks.
    pub closes_at: Option<DateTime<Utc>>,
    /// The configuration the window would run under, which is the schedule's
    /// own and not the settings default while it is open (seam-contract D24).
    pub mode: ScheduleMode,
    pub max_concurrency: i64,
    /// Every `ready` task in board order, exactly as the queue sees it —
    /// including the ones it will pass over, each carrying its reason. A
    /// preflight that filtered the skipped tasks out would answer "which tasks
    /// will run" and silently drop "and which are blocked and why", which is
    /// half of what the scope asks for and the half that costs a night.
    pub plan: Vec<QueueEntry>,
}

impl PreflightSummary {
    /// How many tasks the queue would actually start, eventually, in this
    /// window.
    ///
    /// Derived rather than stored beside [`plan`](Self::plan), so the number and
    /// the list can never disagree. Counts eligibility, not capacity: three
    /// startable tasks in a repository capped at one is still three tasks this
    /// window will get through, one after another.
    pub fn startable(&self) -> usize {
        self.plan
            .iter()
            .filter(|entry| entry.skip.is_none())
            .count()
    }

    /// How many it will pass over, and therefore how many will still be sitting
    /// there in the morning.
    pub fn blocked(&self) -> usize {
        self.plan.len() - self.startable()
    }
}

/// What `schedule_id` would do, against the board as it is right now.
pub async fn preview(ctx: &ServiceContext, schedule_id: &str) -> Result<PreflightSummary> {
    let schedule = schedule::get(ctx, schedule_id).await?;
    let now = ctx.clock.now();

    let next_fire_at = fire::next_fire_at(&schedule, now)?;
    // Measured from the occurrence the schedule would honour, not from `now`,
    // so a preview at 18:00 of a 22:00 window says 06:00 rather than "eight
    // hours from now".
    let closes_at = match next_fire_at {
        Some(occurrence) => {
            let zone = schedule::cron::zone(schedule.timezone.as_deref().unwrap_or_default())?;
            fire::closes_at(&schedule, zone, occurrence)?
        }
        None => None,
    };

    Ok(PreflightSummary {
        schedule_id: schedule.id.clone(),
        schedule_name: schedule.name.clone(),
        next_fire_at,
        closes_at,
        mode: schedule.mode,
        max_concurrency: schedule.max_concurrency,
        plan: selection::plan(ctx).await?,
    })
}

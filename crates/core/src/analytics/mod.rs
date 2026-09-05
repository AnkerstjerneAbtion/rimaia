//! What the queue has actually done (task 024, ADR-0022).
//!
//! Read-only, computed at read time. **No aggregates table, no rollup, and no
//! write of any kind** — ADR-0022 part 3. Every figure below is a fold over
//! `runs` rows the query just fetched, which is what makes "pruning run logs
//! changes nothing on this page" true by construction: task 015 deletes the
//! JSONL file and leaves the row.
//!
//! # A NULL is never a zero (seam-contract D18)
//!
//! The capture columns are nullable and NULL means *not recorded*. Every
//! aggregate here therefore carries the count of rows it could not include, and
//! the page labels a period that predates the capture migration as **partly
//! unrecorded** rather than reporting a smaller total as if it were the whole
//! one. `SUM` over a column with NULLs is a sum over the rows that have values,
//! which is a different quantity from the total; saying so is the whole of D18
//! point 1.
//!
//! # The one number that is wrong if nobody thinks about it
//!
//! **Cost per completed task divides total spend by completed tasks**, counting
//! every failed attempt. A task with four failures and one success cost all
//! five runs. The flattering version — the cost of the successful run — hides
//! exactly the thing worth knowing (task 024's Notes).
//!
//! # Where the planner's spend comes from
//!
//! Not from `runs`: a strategy run deliberately has no row there (seam-contract
//! D17), and its cost is stamped onto the proposal it wrote. So planner spend is
//! summed off `tasks.strategy_plan`'s envelope, and implementation spend is the
//! `runs` total. The difference between them is the overhead of deciding versus
//! doing, which is the only reason either number is on the page.

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use serde::Serialize;
use sqlx::sqlite::SqliteRow;
use sqlx::{FromRow, Row, SqlitePool};

use crate::db::settings;
use crate::db::{BoardColumn, RunStatus, StrategyMode};
use crate::error::Result;
use crate::tasks::strategy::StrategyPlan;

/// The window every figure on the page is scoped to.
///
/// Absolute instants rather than "this week", because a week's boundaries are a
/// question about the *user's* calendar and timezone, and core has no business
/// having an opinion about either. The board computes them; an agent asking
/// over MCP passes them explicitly or omits them for all time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Period {
    /// Inclusive. `None` is "since the beginning".
    pub from: Option<DateTime<Utc>>,
    /// Exclusive, so two adjacent periods cannot both claim a run.
    pub to: Option<DateTime<Utc>>,
}

/// One run, reduced to what the page reads.
///
/// A projection rather than [`Run`](crate::db::Run) because that row carries
/// the composed prompt verbatim — several kilobytes each, and a few thousand
/// runs of it is tens of megabytes fetched to compute a sum of `cost_usd`.
#[derive(Debug, Clone)]
struct AnalyticsRun {
    id: String,
    task_id: String,
    status: RunStatus,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    cost_usd: Option<f64>,
    model: Option<String>,
    /// The task's *current* column, joined — which is how "reached `in_review`
    /// or `done`" is answered. Deliberately the present tense: a task that has
    /// since been moved back is not a task that completed.
    task_column: BoardColumn,
    task_title: String,
    task_strategy_mode: StrategyMode,
}

impl FromRow<'_, SqliteRow> for AnalyticsRun {
    fn from_row(row: &SqliteRow) -> sqlx::Result<Self> {
        Ok(Self {
            id: row.try_get("id")?,
            task_id: row.try_get("task_id")?,
            status: row.try_get("status")?,
            started_at: row.try_get("started_at")?,
            ended_at: row.try_get("ended_at")?,
            cost_usd: row.try_get("cost_usd")?,
            model: row.try_get("model")?,
            task_column: row.try_get("task_column")?,
            task_title: row.try_get("task_title")?,
            task_strategy_mode: row.try_get("task_strategy_mode")?,
        })
    }
}

/// How many runs ended each way, and how many are still going.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunOutcomes {
    pub succeeded: usize,
    pub failed: usize,
    pub cancelled: usize,
    pub interrupted: usize,
    pub running: usize,
}

impl RunOutcomes {
    pub fn total(&self) -> usize {
        self.succeeded + self.failed + self.cancelled + self.interrupted + self.running
    }

    /// Of the runs that *ended*. A run still going has not failed yet, and
    /// counting it as a denominator would make the rate drift down every time
    /// one starts.
    pub fn failure_rate(&self) -> Option<f64> {
        let finished = self.succeeded + self.failed + self.cancelled + self.interrupted;
        (finished > 0).then(|| self.failed as f64 / finished as f64)
    }
}

/// What one day cost, for the small chart.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DaySpend {
    /// `YYYY-MM-DD`, UTC. The chart is a shape, not an audit.
    pub day: String,
    pub spend_usd: f64,
    pub runs: usize,
}

/// One model's share, by both measures — they rank differently, and the
/// difference is the interesting part.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUse {
    pub model: String,
    pub runs: usize,
    pub spend_usd: f64,
}

/// How many runs each strategy mode produced.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StrategyUse {
    pub mode: StrategyMode,
    pub runs: usize,
    pub spend_usd: f64,
}

/// The single longest run, named and linkable.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LongestRun {
    pub run_id: String,
    pub task_id: String,
    pub title: String,
    pub seconds: i64,
}

/// Everything the page renders, in one read.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Analytics {
    pub period: Period,

    pub outcomes: RunOutcomes,

    /// Summed over the rows that **have** a cost. See `runsWithoutCost`.
    pub spend_usd: f64,
    pub spend_by_day: Vec<DaySpend>,
    /// How many runs in this period recorded no `cost_usd` at all — a run that
    /// died before its terminal `result` event, or one from before the capture
    /// columns existed. The page says "partly unrecorded" off this rather than
    /// reporting the smaller total as if it were the whole one (D18).
    pub runs_without_cost: usize,
    /// The same, for the model mix.
    pub runs_without_model: usize,

    /// Distinct tasks with at least one run in this period.
    pub tasks_attempted: usize,
    /// Of those, the ones now in `in_review` or `done`.
    pub tasks_completed: usize,
    /// Total spend divided by completed tasks — every failed attempt included.
    /// `None` when nothing completed, because a division by zero is not a
    /// number and a dash is not a lie.
    pub cost_per_completed_task_usd: Option<f64>,

    pub median_duration_seconds: Option<i64>,
    pub longest_run: Option<LongestRun>,
    /// Summed run duration. **Not wall-clock**: two runs in parallel (ADR-0010)
    /// contribute their own hours each, which is the honest reading of "how
    /// much work the queue did while nobody watched" and the reason this is not
    /// called elapsed time.
    pub unattended_hours: f64,

    pub models: Vec<ModelUse>,
    pub strategies: Vec<StrategyUse>,

    /// Off `tasks.strategy_plan`, because a planner has no `runs` row (D17).
    pub planner_spend_usd: f64,
    /// The `runs` total, restated beside the planner's so the two can be read
    /// as a ratio without the reader doing arithmetic.
    pub implementation_spend_usd: f64,

    /// What the user says they pay per month. **Absent, not zero**, until they
    /// enter one — and presented as *theirs*, because Rimaia cannot verify it.
    pub subscription_monthly_usd: Option<f64>,
}

/// Everything on the page, computed from `runs` at read time.
pub async fn analytics(pool: &SqlitePool, period: Period) -> Result<Analytics> {
    let runs = runs_in(pool, period).await?;

    let mut outcomes = RunOutcomes::default();
    let mut spend_usd = 0.0;
    let mut runs_without_cost = 0;
    let mut runs_without_model = 0;
    let mut durations: Vec<i64> = Vec::new();
    let mut longest: Option<LongestRun> = None;
    let mut by_day: Vec<DaySpend> = Vec::new();
    let mut by_model: Vec<ModelUse> = Vec::new();
    let mut by_strategy: Vec<StrategyUse> = Vec::new();
    let mut attempted: Vec<&str> = Vec::new();
    let mut completed: Vec<&str> = Vec::new();

    for run in &runs {
        match run.status {
            RunStatus::Succeeded => outcomes.succeeded += 1,
            RunStatus::Failed => outcomes.failed += 1,
            RunStatus::Cancelled => outcomes.cancelled += 1,
            RunStatus::Interrupted => outcomes.interrupted += 1,
            RunStatus::Running => outcomes.running += 1,
        }

        match run.cost_usd {
            Some(cost) => spend_usd += cost,
            None => runs_without_cost += 1,
        }

        let day = run.started_at.date_naive().to_string();
        match by_day.iter_mut().find(|entry| entry.day == day) {
            Some(entry) => {
                entry.spend_usd += run.cost_usd.unwrap_or(0.0);
                entry.runs += 1;
            }
            None => by_day.push(DaySpend {
                day,
                spend_usd: run.cost_usd.unwrap_or(0.0),
                runs: 1,
            }),
        }

        match &run.model {
            Some(model) => match by_model.iter_mut().find(|entry| &entry.model == model) {
                Some(entry) => {
                    entry.runs += 1;
                    entry.spend_usd += run.cost_usd.unwrap_or(0.0);
                }
                None => by_model.push(ModelUse {
                    model: model.clone(),
                    runs: 1,
                    spend_usd: run.cost_usd.unwrap_or(0.0),
                }),
            },
            // Not folded into an "unknown" bucket: a bucket in the chart would
            // read as a model somebody chose (D18 point 1).
            None => runs_without_model += 1,
        }

        match by_strategy
            .iter_mut()
            .find(|entry| entry.mode == run.task_strategy_mode)
        {
            Some(entry) => {
                entry.runs += 1;
                entry.spend_usd += run.cost_usd.unwrap_or(0.0);
            }
            None => by_strategy.push(StrategyUse {
                mode: run.task_strategy_mode,
                runs: 1,
                spend_usd: run.cost_usd.unwrap_or(0.0),
            }),
        }

        if let Some(ended_at) = run.ended_at {
            let seconds = (ended_at - run.started_at).num_seconds().max(0);
            durations.push(seconds);
            if longest.as_ref().is_none_or(|held| seconds > held.seconds) {
                longest = Some(LongestRun {
                    run_id: run.id.clone(),
                    task_id: run.task_id.clone(),
                    title: run.task_title.clone(),
                    seconds,
                });
            }
        }

        if !attempted.contains(&run.task_id.as_str()) {
            attempted.push(&run.task_id);
            if matches!(run.task_column, BoardColumn::InReview | BoardColumn::Done) {
                completed.push(&run.task_id);
            }
        }
    }

    by_day.sort_by(|a, b| a.day.cmp(&b.day));
    // Most-used first: the mix is read as a ranking, and the point is that the
    // two orderings disagree.
    by_model.sort_by(|a, b| b.runs.cmp(&a.runs).then(a.model.cmp(&b.model)));
    by_strategy.sort_by_key(|entry| std::cmp::Reverse(entry.runs));
    durations.sort_unstable();

    let unattended_hours = durations.iter().sum::<i64>() as f64 / 3600.0;
    let tasks_completed = completed.len();

    Ok(Analytics {
        period,
        outcomes,
        spend_usd,
        spend_by_day: by_day,
        runs_without_cost,
        runs_without_model,
        tasks_attempted: attempted.len(),
        tasks_completed,
        // Divided by *completed tasks*, not by successful runs, and the
        // numerator is every run in the period. See this module's header.
        cost_per_completed_task_usd: (tasks_completed > 0)
            .then(|| spend_usd / tasks_completed as f64),
        median_duration_seconds: median(&durations),
        longest_run: longest,
        unattended_hours,
        models: by_model,
        strategies: by_strategy,
        planner_spend_usd: planner_spend(pool, period).await?,
        implementation_spend_usd: spend_usd,
        subscription_monthly_usd: settings::subscription_monthly_usd(pool).await?,
    })
}

/// The lower median of a sorted list, or `None` when it is empty.
///
/// Lower rather than interpolated: these are durations in whole seconds, and
/// half of a real run is a more honest answer than the average of two.
fn median(sorted: &[i64]) -> Option<i64> {
    sorted.get(sorted.len().saturating_sub(1) / 2).copied()
}

/// The runs in the period, joined to what their task is now.
///
/// Hand-built SQL rather than `query_as!` for `runs/mod.rs`'s stated reason —
/// the bounds are optional, so the predicate is not fixed at compile time — and
/// the projection is narrow on purpose: `runs.prompt` is kilobytes a sum has no
/// use for.
async fn runs_in(pool: &SqlitePool, period: Period) -> Result<Vec<AnalyticsRun>> {
    let mut sql = String::from(
        "SELECT r.id, r.task_id, r.status, r.started_at, r.ended_at, r.cost_usd, r.model,
                t.board_column AS task_column, t.title AS task_title,
                t.strategy_mode AS task_strategy_mode
         FROM runs r
         JOIN tasks t ON t.id = r.task_id
         WHERE 1 = 1",
    );
    if period.from.is_some() {
        sql.push_str(" AND r.started_at >= ?");
    }
    if period.to.is_some() {
        sql.push_str(" AND r.started_at < ?");
    }
    sql.push_str(" ORDER BY r.started_at ASC");

    let mut query = sqlx::query_as::<_, AnalyticsRun>(&sql);
    if let Some(from) = period.from {
        query = query.bind(from);
    }
    if let Some(to) = period.to {
        query = query.bind(to);
    }

    Ok(query.fetch_all(pool).await?)
}

/// What the planners cost, summed off the proposals they wrote.
///
/// A planner has no `runs` row (D17), so this is the only record of what
/// deciding cost. Scoped by `strategy_updated_at`, which is when the envelope
/// was written — the closest thing a proposal has to a run's `started_at`.
///
/// A proposal whose envelope will not parse contributes nothing rather than
/// failing the page: the same tolerance every other reader of that column
/// applies, and a hand-edited row is not a reason a chart cannot be drawn.
async fn planner_spend(pool: &SqlitePool, period: Period) -> Result<f64> {
    let mut sql = String::from(
        "SELECT strategy_plan FROM tasks
         WHERE strategy_plan IS NOT NULL AND strategy_updated_at IS NOT NULL",
    );
    if period.from.is_some() {
        sql.push_str(" AND strategy_updated_at >= ?");
    }
    if period.to.is_some() {
        sql.push_str(" AND strategy_updated_at < ?");
    }

    let mut query = sqlx::query_scalar::<_, String>(&sql);
    if let Some(from) = period.from {
        query = query.bind(from);
    }
    if let Some(to) = period.to {
        query = query.bind(to);
    }

    Ok(query
        .fetch_all(pool)
        .await?
        .into_iter()
        .filter_map(|stored| StrategyPlan::from_stored(Some(&stored)))
        .filter_map(|plan| plan.run.and_then(|run| run.cost_usd))
        .sum())
}

/// The first instant of the calendar month `at` falls in, UTC.
///
/// Exported because the MCP tool has no browser to compute a month boundary in,
/// and an agent asked for "this month" should get the same answer the window
/// would give rather than a range it invented.
pub fn start_of_month(at: DateTime<Utc>) -> Option<DateTime<Utc>> {
    NaiveDate::from_ymd_opt(at.year(), at.month(), 1)?
        .and_hms_opt(0, 0, 0)
        .map(|naive| naive.and_utc())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn the_median_of_an_even_list_is_a_real_measurement_rather_than_an_average() {
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[7]), Some(7));
        assert_eq!(median(&[2, 4]), Some(2));
        assert_eq!(median(&[1, 2, 3]), Some(2));
        assert_eq!(median(&[1, 2, 3, 4]), Some(2));
    }

    #[test]
    fn a_run_still_going_is_not_in_the_failure_rate_denominator() {
        // Otherwise the rate would drift down every time a run started, which
        // is the opposite of what "a failure rate that is climbing" is for.
        let outcomes = RunOutcomes {
            succeeded: 3,
            failed: 1,
            running: 6,
            ..RunOutcomes::default()
        };

        assert_eq!(outcomes.total(), 10);
        assert_eq!(outcomes.failure_rate(), Some(0.25));
    }

    #[test]
    fn nothing_finished_has_no_failure_rate_rather_than_a_zero_one() {
        assert_eq!(
            RunOutcomes {
                running: 2,
                ..RunOutcomes::default()
            }
            .failure_rate(),
            None
        );
    }

    #[test]
    fn a_month_starts_at_midnight_on_the_first() {
        let at = "2026-09-04T22:15:00Z"
            .parse::<DateTime<Utc>>()
            .expect("an instant");

        assert_eq!(
            start_of_month(at).map(|start| start.to_rfc3339()),
            Some("2026-09-01T00:00:00+00:00".to_string())
        );
    }
}

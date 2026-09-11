//! The analytics page (task 024, ADR-0022).
//!
//! Thin over `rimaia_core::analytics`, which does every `SELECT` and takes
//! every decision about what a NULL means (seam-contract D18). **Nothing here
//! writes a run row, and nothing anywhere caches an aggregate** — ADR-0022
//! part 3 makes the page a pure read, and the cheapest way to keep that true is
//! for there to be no other kind of function in this file.

use chrono::{DateTime, Utc};
use rimaia_core::analytics::{self, Analytics, Period};
use rimaia_core::db::settings;
use rimaia_core::Result;
use tauri::State;

use crate::state::AppState;

/// Every figure the page renders, for one period.
///
/// The bounds arrive already resolved, because "this week" is a question about
/// the *user's* calendar and timezone and the window is the only thing that
/// knows either. Omitting both is all time.
#[tauri::command]
pub async fn get_analytics(
    state: State<'_, AppState>,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> Result<Analytics> {
    analytics::analytics(&state.context.pool, Period { from, to }).await
}

/// What the user says they pay per month, or `null` when they have not said.
///
/// Its own command rather than a field on [`get_analytics`], because the two
/// change on completely different clocks: this one when a human types in a box,
/// the other every time a run ends.
#[tauri::command]
pub async fn get_subscription_cost(state: State<'_, AppState>) -> Result<Option<f64>> {
    settings::subscription_monthly_usd(&state.context.pool).await
}

/// Stores it, or clears it with `null`. Refuses a negative figure at the field
/// rather than storing one the page would have to ignore.
#[tauri::command]
pub async fn set_subscription_cost(state: State<'_, AppState>, value: Option<f64>) -> Result<()> {
    settings::set_subscription_monthly_usd(&state.context, value).await
}

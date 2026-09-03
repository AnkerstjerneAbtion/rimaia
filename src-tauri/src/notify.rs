//! The optional OS notification (task 013's Scope).
//!
//! Three moments, and every one of them happens with nobody at the machine:
//! the queue starting at 22:00, the run window closing at 06:00, and a
//! scheduled start the preflight doctor refused. A desktop notification is the
//! only surface that reaches somebody who is not looking at the window, which
//! is the entire point of a product whose premise is that the user has left.
//!
//! # It lives in the shell, and it has to
//!
//! `rimaia-core` must not depend on `tauri` (ADR-0015), and a notification is a
//! `tauri` call. So core publishes the facts — an open window is a `settings`
//! key, and writing it announces itself on ADR-0018's channel — and this module
//! turns a *change* in them into a sentence.
//!
//! # It diffs rather than listening for an event that says "fired"
//!
//! There is deliberately no `ChangeEvent::ScheduleFired`. ADR-0018's whole
//! argument is that events carry ids and never payloads, because an event with
//! a payload is a second source of truth that is wrong the moment the next
//! writer commits — and "the window opened" is a fact about stored state, which
//! is exactly the kind of thing this channel says to go and re-read. So the
//! notifier holds the last window it saw and compares, which is also what makes
//! it correct across a missed event: a `Lagged` receiver re-reads and still
//! notices the transition, one notification late rather than never.
//!
//! # A failure here is never anything
//!
//! Notifications can be refused by the operating system, by a user who denied
//! permission, or by a headless session. Every failure is logged at `debug` and
//! dropped. Nothing about whether the queue runs may depend on whether a toast
//! was drawn.

use rimaia_core::schedule::window::{self, RunWindow};
use rimaia_core::scheduler::{QueueHandle, QueueState};
use rimaia_core::{ChangeEvent, ServiceContext};
use tauri::AppHandle;
use tauri_plugin_notification::NotificationExt;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;

/// Watches for a run window opening or closing, and says so.
///
/// Spawned once in `setup()`, beside the two forwarders, and subscribed before
/// anything can publish — the same "subscribe first, then let writers start"
/// order everything else in that hook keeps.
pub async fn announce_run_windows(
    app: AppHandle,
    ctx: ServiceContext,
    queue: QueueHandle,
    mut events: broadcast::Receiver<ChangeEvent>,
) {
    // What the last launch left behind, read once. A crash does not close a
    // window — only quitting does (seam-contract D15's amendment) — so this is
    // occasionally `Some` at startup, and seeding from it is what stops the
    // first settings change of the session announcing a window that has been
    // open since last night.
    let mut seen = current(&ctx).await;
    let mut announced_error: Option<String> = queue.last_step_error();

    loop {
        match events.recv().await {
            // Only `Settings` can carry either of these: the window and
            // `queue_state` are both `settings` rows. A task moving or a run
            // ending cannot open a window, so there is nothing to re-read.
            Ok(ChangeEvent::Settings) => {}
            Ok(_) => continue,
            // A dropped event costs one late comparison, never a missed one:
            // the next event re-reads the same rows and still finds the
            // transition.
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => break,
        }

        let now = current(&ctx).await;
        match (&seen, &now) {
            (None, Some(open)) => notify(&app, "Rimaia is working", &opened(open)),
            (Some(closed), None) => notify(&app, "Rimaia has stopped", &closed_message(closed)),
            _ => {}
        }
        seen = now;

        // The doctor's refusal, which has no window to compare — it is the case
        // where one deliberately was *not* opened. Compared by value so the
        // same refusal is announced once rather than on every later settings
        // write.
        let error = queue.last_step_error();
        if error != announced_error {
            if let Some(reason) = &error {
                notify(&app, "Rimaia did not start", reason);
            }
            announced_error = error;
        }
    }
}

/// The open window, or `None` — plus the switch, because a window that is open
/// while the queue is paused is not a queue that is working.
async fn current(ctx: &ServiceContext) -> Option<RunWindow> {
    let window = window::active(&ctx.pool).await.unwrap_or_else(|error| {
        tracing::debug!(%error, "could not read the run window for a notification");
        None
    });
    let running = rimaia_core::scheduler::queue_state(&ctx.pool)
        .await
        .map(|state| state == QueueState::Running)
        .unwrap_or(false);

    window.filter(|_| running)
}

fn opened(window: &RunWindow) -> String {
    match window.closes_at {
        Some(closes_at) => format!(
            "{} started the run queue. It will stop starting new tasks at {}.",
            window.schedule_name,
            local_time(closes_at),
        ),
        None => format!(
            "{} started the run queue. It has no stop time.",
            window.schedule_name
        ),
    }
}

fn closed_message(window: &RunWindow) -> String {
    format!(
        "{}'s run window has closed. Nothing new will start; any run still going will finish.",
        window.schedule_name,
    )
}

fn local_time(at: chrono::DateTime<chrono::Utc>) -> String {
    at.with_timezone(&chrono::Local).format("%H:%M").to_string()
}

fn notify(app: &AppHandle, title: &str, body: &str) {
    if let Err(error) = app.notification().builder().title(title).body(body).show() {
        // Denied permission, a headless session, or an OS that refused. None of
        // them is a reason for anything else to behave differently.
        tracing::debug!(%error, "could not show a desktop notification");
    }
}

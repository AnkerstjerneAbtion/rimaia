//! `GET /api/v1/events`: one Server-Sent Events stream per client (ADR-0034
//! point 3, task 048).
//!
//! Everything a stream decides is `rimaia_core::api::events::Subscription`'s:
//! the opening re-read, admission by the caller's teams, lag recovery and
//! re-checking the caller. This module adds HTTP framing and nothing else.
//!
//! - **Refusals come before the stream.** The caller, the protocol header and
//!   `?tail=` are settled before the first byte, so a client's `onopen` sees a
//!   `401` or a `400` in the error shape, never an event.
//! - **`?tail=<run id>` may repeat**, up to
//!   [`MAX_TAILS_PER_STREAM`](rimaia_core::api::events::MAX_TAILS_PER_STREAM).
//!   Changing the set means reconnecting, and the opening re-read comes with
//!   it, so the server keeps no per-stream state another request could reach.
//! - **One pump task per stream** moves `Subscription::next()` into a bounded
//!   channel that `Sse` reads. It exits when the client goes, dropping the
//!   subscription and its receivers, so a closed tab leaks nothing.
//! - **Nothing here logs the query or an event body** (ADR-0037 point 6).

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use rimaia_core::api::events::Revalidate;
use rimaia_core::api::{protocol, Caller, Effect, Subscription, Tails, WireEvent};
use rimaia_core::Error;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::caller::Presented;
use crate::{ApiError, ProtocolHeader, ServerState};

/// How many frames a stream's pump may hold for a client that is slow to
/// read, before it stops reading the subscription. Past it the subscription
/// falls behind its channels, and recovers by its own re-read.
const PUMP_CAPACITY: usize = 16;

/// The route.
///
/// [`Caller`] first, as on every board route; then what was presented,
/// kept to resolve the caller again for as long as the stream is open.
pub async fn stream(
    caller: Caller,
    presented: Presented,
    ProtocolHeader(header): ProtocolHeader,
    State(state): State<ServerState>,
    query: Result<Query<Vec<(String, String)>>, QueryRejection>,
) -> Result<Response, ApiError> {
    protocol::check(header.as_deref(), Effect::Read)?;
    let Query(pairs) = query.map_err(|rejection| Error::invalid(rejection.body_text()))?;
    let tails = Tails::requested(
        pairs
            .into_iter()
            .filter(|(key, _)| key == "tail")
            .map(|(_, run_id)| run_id),
    )?;

    let subscription = Subscription::open(
        &state.host,
        caller,
        tails,
        Some(revalidate(&state, presented)),
    );
    let (frames, receiver) = mpsc::channel(PUMP_CAPACITY);
    tokio::spawn(pump(subscription, frames));
    Ok(Sse::new(ReceiverStream::new(receiver))
        .keep_alive(KeepAlive::default())
        .into_response())
}

/// Resolves the stream's caller again exactly as the route resolved it: the
/// same credential through the same `Authenticate` and door check. Task
/// 050's edge steps, such as narrowing to one team, compose here.
fn revalidate(state: &ServerState, presented: Presented) -> Revalidate {
    let auth = Arc::clone(&state.auth);
    let presented = Arc::new(presented);
    Box::new(move || {
        let auth = Arc::clone(&auth);
        let presented = Arc::clone(&presented);
        Box::pin(async move { presented.resolve(auth.as_ref()).await })
    })
}

/// Moves the subscription's events into `frames` until the stream ends or
/// the client goes. Waiting on `closed` beside `next` is what lets a stream
/// with nothing to say notice a closed tab.
async fn pump(mut subscription: Subscription, frames: mpsc::Sender<Result<Event, Infallible>>) {
    loop {
        let event = tokio::select! {
            _ = frames.closed() => break,
            event = subscription.next() => event,
        };
        let Some(event) = event else {
            break;
        };
        if frames.send(Ok(frame(event))).await.is_err() {
            break;
        }
    }
}

/// `event: <name>` and `data: <json>`, and no `id:` line: there is no log to
/// resume from (ADR-0034 point 3 chose the re-read).
fn frame(event: WireEvent) -> Event {
    Event::default()
        .event(event.name)
        .data(event.data.to_string())
}

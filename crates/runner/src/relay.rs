//! Solo's relay: Run now, Retry now and Cancel, answered on this machine
//! (task 052, ADR-0031 point 7).
//!
//! In solo the board and its one runner are one process, so a person's Run
//! now does not wait for a claim to collect it: it runs the manual starter
//! here and then and spawns the run, beside the loop that spawns queued ones,
//! and answers with the starter's own refusals synchronously, byte for byte
//! the sentences the desktop commands always gave. Task 008's and ADR-0026's
//! "refused before any run state is written" hold, because they are the
//! starter's (`runner::start::claim_manual_start`). A solo Run now must never
//! go through `RelayedRequests`: a refusal would become a card that silently
//! does nothing until the queue next asked.
//!
//! It holds no Tauri type, so its tests run here; `src-tauri` builds it in
//! `setup()` and puts it on the solo board host.

use std::sync::Arc;

use rimaia_core::board::{BoardFuture, BoardPort, LeaseRef, RunNowRequest, RunnerRelay};
use rimaia_core::machine::MachineContext;
use rimaia_core::runner::{
    claim_manual_start, run_task, ManualStart, RunRequest, RunnerConfig, Started, Starter,
};
use rimaia_core::scheduler::InFlight;
use rimaia_core::{AppPaths, ServiceContext};

/// The shell's relay, over the same values the loop runs with.
pub struct ShellRelay {
    /// The board port every start reaches the board through.
    pub board: Arc<dyn BoardPort>,
    pub machine: MachineContext,
    pub paths: AppPaths,
    /// The one config every starter spawns from, so a Run now and a queued run
    /// of one card are the same process.
    pub config: RunnerConfig,
    /// D19's slot registry, shared with the loop: a second click, the queue
    /// and Plan now all fail against one map.
    pub in_flight: InFlight,
}

impl RunnerRelay for ShellRelay {
    /// The starter, then the run, spawned: it answers as soon as the run is
    /// under way, never once it finishes. Everything a click can be refused
    /// for is settled before it answers: the owner rule (as the owner at the
    /// runner, so `acceptEdits`), the slot, the opt-in, what the provider can
    /// honour, the `claude` prerequisite and the claim itself.
    fn run_now<'a>(
        &'a self,
        ctx: &'a ServiceContext,
        request: RunNowRequest,
    ) -> BoardFuture<'a, ()> {
        Box::pin(async move {
            let task_id = request.task_id.clone();
            let Started { slot, claim } = claim_manual_start(
                Starter::at_runner(ctx, &request.runner_id),
                self.board.as_ref(),
                &self.machine,
                &self.paths,
                &self.config,
                &self.in_flight,
                ManualStart {
                    task_id: request.task_id,
                    continue_session: request.continue_session,
                },
            )
            .await?;

            let run = RunRequest {
                cancel: slot.cancel_signal(),
                // The same registry the loop hands its own runs, so a Run now
                // and a queued run in one repository take turns creating their
                // worktrees rather than racing `git worktree add` against one
                // `.git` (`InFlight::preparation_lock`).
                in_flight: Some(self.in_flight.clone()),
            };
            let board = Arc::clone(&self.board);
            let machine = self.machine.clone();
            let paths = self.paths.clone();
            let config = self.config.clone();
            tokio::spawn(async move {
                // Held until the run has finished: the slot is what a second
                // click, the queue and Plan now all fail against.
                let _slot = slot;
                if let Err(error) =
                    run_task(board.as_ref(), &machine, &paths, &config, claim, run).await
                {
                    tracing::error!(%task_id, %error, "a run could not be started or supervised");
                }
            });
            Ok(())
        })
    }

    /// Stops whatever this machine is running or starting for `task_id`,
    /// whatever `lease` says: that includes a manual start holding its slot in
    /// the CLI probe, which has no lease yet. A no-op when nothing is.
    fn cancel<'a>(
        &'a self,
        _ctx: &'a ServiceContext,
        task_id: &'a str,
        _lease: Option<LeaseRef>,
    ) -> BoardFuture<'a, ()> {
        Box::pin(async move {
            self.in_flight.cancel(task_id);
            Ok(())
        })
    }
}

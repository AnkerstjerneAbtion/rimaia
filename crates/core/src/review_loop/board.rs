//! Every card's loop summary, from a fixed number of reads (task 037,
//! seam-contract D12's "the summary carries the review loop").
//!
//! [`summaries`] is [`super::summary_for`] over a whole board. The verdict is
//! a Rust function over rows, findings and configuration, so it cannot be one
//! correlated subquery; and a SQL copy of it would be a second
//! implementation free to drift from the one `get_task` calls. So the board
//! loads what [`history::summary`] reads, once for every listed task, and
//! calls that same function per task in memory.
//!
//! Five statements per call, whatever the number of cards and whatever the
//! number of repositories: the global configuration, the tasks' own, the
//! repositories', the runs, the findings. **Not** the shape of
//! `tasks::service::apply_effective_strategy`, which reads once per distinct
//! repository and so is N+1 over repositories.

use std::collections::HashMap;

use super::{config, history, ReviewLoopSummary};
use crate::context::ServiceContext;
use crate::error::Result;
use crate::review::findings;

/// The task a summary is wanted for, and the repository whose configuration
/// sits between the task's and the global one.
pub struct BoardTask<'a> {
    pub task_id: &'a str,
    pub repository_id: &'a str,
}

/// `tasks`' summaries, keyed by task id. A task whose verdict is
/// [`history::Verdict::None`] has no entry, as it has none on `get_task`.
pub async fn summaries(
    ctx: &ServiceContext,
    tasks: &[BoardTask<'_>],
) -> Result<HashMap<String, ReviewLoopSummary>> {
    if tasks.is_empty() {
        return Ok(HashMap::new());
    }
    let task_ids: Vec<String> = tasks.iter().map(|task| task.task_id.to_string()).collect();
    let mut repository_ids: Vec<String> = tasks
        .iter()
        .map(|task| task.repository_id.to_string())
        .collect();
    repository_ids.sort();
    repository_ids.dedup();

    let global = config::global_config(&ctx.pool).await?;
    let task_configs = config::task_configs_for(&ctx.pool, &task_ids).await?;
    let repository_configs = config::repository_configs_for(&ctx.pool, &repository_ids).await?;
    let mut rows = findings::loop_rows_for(ctx, &task_ids).await?;
    let mut found = findings::list_for(ctx, &task_ids).await?;

    let nothing = config::ReviewConfig::default();
    let mut summaries = HashMap::new();
    for task in tasks {
        let effective = config::effective(
            task_configs.get(task.task_id).unwrap_or(&nothing),
            repository_configs
                .get(task.repository_id)
                .unwrap_or(&nothing),
            &global,
        );
        let rows = rows.remove(task.task_id).unwrap_or_default();
        let found = found.remove(task.task_id).unwrap_or_default();
        if let Some(summary) = history::summary(&rows, &found, &effective) {
            summaries.insert(task.task_id.to_string(), summary);
        }
    }
    Ok(summaries)
}

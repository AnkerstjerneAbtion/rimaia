//! Copying a task to another team (task 051 Scope 9, ADR-0029 point 5).
//!
//! A task never moves between teams: its runs were paid for with one team's
//! subscriptions and its transcripts were seen by that team. A copy is a new
//! task carrying the plan and nothing else.

use serde::Serialize;

use super::refuse_solo;
use crate::context::ServiceContext;
use crate::db::{BoardColumn, Task};
use crate::error::Result;
use crate::events::{RepositoryId, TeamId};
use crate::tasks::{self, NewTask};

/// A team a task can be copied into, with its repositories.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyTarget {
    pub team_id: TeamId,
    pub team_name: String,
    pub repositories: Vec<CopyTargetRepository>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyTargetRepository {
    pub id: RepositoryId,
    pub name: String,
}

/// Copies `task_id` into the team that owns `repository_id`, as a new task in
/// that repository. The target team is the repository's, so there is no team
/// argument to get wrong (039).
///
/// The copy is made by `tasks::create_task`, so every creation rule applies:
/// the copier is its author, its plan revision is 1 and its source is the
/// door's. It carries the title, the plan and the extra instructions, which
/// 045's plan revision covers together, and lands in `not_ready`, at the
/// bottom: plan text from another team has been accepted by nobody in the new
/// one, so it does not join the run queue by itself. Nothing else comes with
/// it. The source task is unchanged, and copying within one team is a
/// duplicate.
#[tracing::instrument(skip_all, fields(user_id = ctx.actor.as_str(), task_id = %task_id))]
pub async fn copy_task_to_team(
    ctx: &ServiceContext,
    task_id: &str,
    repository_id: &str,
) -> Result<Task> {
    {
        let mut conn = ctx.pool.acquire().await?;
        refuse_solo(&mut conn).await?;
    }
    let source = tasks::service::task_row(ctx, task_id).await?;
    tasks::create_task(
        ctx,
        NewTask {
            repository_id: repository_id.to_string(),
            title: source.title,
            plan: source.plan,
            extra_instructions: source.extra_instructions,
            column: Some(BoardColumn::NotReady),
            links: vec![],
        },
    )
    .await
}

/// Every team in the scope with its repositories, the caller's personal team
/// first, then by name: what the copy's picker offers.
pub async fn list_copy_targets(ctx: &ServiceContext) -> Result<Vec<CopyTarget>> {
    let scope = ctx.scope.json();
    let teams = sqlx::query!(
        r#"SELECT t.id, t.name, COALESCE(t.personal_user_id = ?2, 0) AS "personal!: bool"
             FROM teams t
            WHERE t.id IN (SELECT value FROM json_each(?1))
            ORDER BY 3 DESC, t.name, t.id"#,
        scope,
        ctx.actor,
    )
    .fetch_all(&ctx.pool)
    .await?;
    let repositories = sqlx::query!(
        "SELECT id, team_id, name FROM repositories
          WHERE team_id IN (SELECT value FROM json_each(?1))
          ORDER BY name, id",
        scope,
    )
    .fetch_all(&ctx.pool)
    .await?;
    Ok(teams
        .into_iter()
        .map(|team| CopyTarget {
            repositories: repositories
                .iter()
                .filter(|repository| repository.team_id == team.id)
                .map(|repository| CopyTargetRepository {
                    id: repository.id.clone(),
                    name: repository.name.clone(),
                })
                .collect(),
            team_id: team.id,
            team_name: team.name,
        })
        .collect())
}

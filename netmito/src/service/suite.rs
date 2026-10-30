use std::collections::HashMap;

use sea_orm::sea_query::extension::postgres::PgExpr;
use sea_orm::sea_query::{
    Alias, CommonTableExpression, DeleteStatement, Func, InsertStatement, PgFunc, Query, WithClause,
};
use sea_orm::{prelude::*, ConnectionTrait, FromQueryResult, Set, TransactionTrait};
use uuid::Uuid;

use super::suite_agent::{apply_suite_agent_override, authorize_suite, resolve_agent};
use crate::config::InfraPool;
use crate::entity::role::UserGroupRole;
use crate::entity::state::{SuiteJobState, TaskState, TaskSuiteState};
use crate::entity::{
    active_tasks as ActiveTasks, agents as Agent, archived_tasks as ArchivedTasks, groups as Group,
    hook_tasks as HookTasks, suite_agent_jobs as SuiteAgentJobs, task_suites as TaskSuites,
    user_group as UserGroup, users as User,
};
use crate::error::{ApiError, Error, ResolveError, Result};
use crate::schema::{
    AgentNotification, CancelTaskSuiteOp, ChangeTaskSuiteReq, CountQuery, CreateTaskSuiteReq,
    CreateTaskSuiteResp, ExecHooks, HookTaskInfo, ParsedTaskSuiteInfo, StopAgentJobResp, StopJobOp,
    SuiteAgentOverrideReq, SuiteAgentOverrideResp, SuiteJobInfo, SuiteJobQueryResp,
    SuiteJobsQueryReq, SuiteJobsQueryResp, TaskResultMessage, TaskResultSpec, TaskSuiteInfo,
    TaskSuiteQueryResp, TaskSuitesQueryReq, TaskSuitesQueryResp, UpdateOp, WorkerSchedulePlan,
};
use crate::service::agent;
use crate::service::task::{check_exec_spec, parse_operators_with_number, OperatorWithNumber};
use crate::ws::AgentWsRouter;

#[derive(FromQueryResult)]
struct GroupIdResult {
    id: i64,
}

#[derive(FromQueryResult)]
struct TaskUuidResult {
    uuid: Uuid,
}

/// Move the suites without a new task out of `Open`.
///
/// A suite stays `Open` for `timeout` after its last submission, also when it has no more tasks.
/// During this time, the agent that runs it keeps its job and can run a late task without a new
/// provision. Then:
///
/// | from | condition | to |
/// |---|---|---|
/// | `Open` | idle, tasks remain | `Closed` |
/// | `Open` | idle, no task remains | `Complete` |
/// | `Closed` | no task remains | `Complete` |
///
/// The last rule is a backup: `decrement_incomplete_tasks` makes a `Closed` suite `Complete` when its
/// last task commits. This rule catches the suites that were closed after their last task.
///
/// The idle time starts at the last submission, or at the creation if the suite has no task. Thus a
/// suite that never got a task also closes.
pub async fn sweep_inactive_suites(
    db: &DatabaseConnection,
    timeout: std::time::Duration,
) -> Result<u64> {
    let now = TimeDateTimeWithTimeZone::now_utc();
    let threshold = now - time::Duration::seconds(timeout.as_secs() as i64);
    let idle_since = Expr::expr(Func::coalesce([
        Expr::col(TaskSuites::Column::LastTaskSubmittedAt).into(),
        Expr::col(TaskSuites::Column::CreatedAt).into(),
    ]));

    let completed = TaskSuites::Entity::update_many()
        .col_expr(
            TaskSuites::Column::State,
            Expr::value(TaskSuiteState::Complete),
        )
        .col_expr(TaskSuites::Column::CompletedAt, Expr::value(Some(now)))
        .col_expr(TaskSuites::Column::UpdatedAt, Expr::value(now))
        .filter(TaskSuites::Column::IncompleteTasks.eq(0))
        .filter(
            // A `Closed` suite is already idle. An `Open` suite must be idle for the full window.
            TaskSuites::Column::State
                .eq(TaskSuiteState::Closed)
                .or(TaskSuites::Column::State
                    .eq(TaskSuiteState::Open)
                    .and(idle_since.clone().lt(threshold))),
        )
        .exec(db)
        .await?
        .rows_affected;

    let closed = TaskSuites::Entity::update_many()
        .col_expr(
            TaskSuites::Column::State,
            Expr::value(TaskSuiteState::Closed),
        )
        .col_expr(TaskSuites::Column::UpdatedAt, Expr::value(now))
        .filter(TaskSuites::Column::State.eq(TaskSuiteState::Open))
        .filter(TaskSuites::Column::IncompleteTasks.gt(0))
        .filter(idle_since.lt(threshold))
        .exec(db)
        .await?
        .rows_affected;

    if completed + closed > 0 {
        tracing::debug!(
            completed,
            closed,
            "Swept suites idle for more than {timeout:?}"
        );
    }
    Ok(completed + closed)
}

/// Decrease `incomplete_tasks` of a suite by `count`, because `count` of its tasks are finished.
/// If this makes the counter of a `Closed` suite zero, the suite becomes `Complete`.
///
/// Each path that removes an active task from a suite must call this function. If a path archives
/// a suite task and does not call it, the suite cannot become `Complete`. Agents then continue to
/// look for tasks that do not exist.
///
/// The subtraction has no lower limit, and this is intentional. Each caller removed exactly `count`
/// rows from `active_tasks` immediately before the call. Each of these rows increased the counter
/// by one when it was submitted. Thus, the counter cannot correctly go below zero. A lower limit
/// would hide a bug in a different part of the code. Instead, the function logs an error for a
/// negative counter.
///
/// When an `Open` suite has no more tasks, it does not become `Complete` here. It stays `Open`
/// until it also gets no new task for `suite_auto_close_timeout`. During this time, the agent can
/// keep its job and its provisioned environment for a task that arrives late. Then
/// [`sweep_inactive_suites`] changes the state of the suite. A `Closed` suite does not wait, so
/// this function makes it `Complete` immediately.
///
/// The suite row must be locked before all task rows, by this call or by the caller before it.
/// Refer to the lock-order note on [`crate::service`].
pub(crate) async fn decrement_incomplete_tasks<C: ConnectionTrait>(
    db: &C,
    suite_id: i64,
    count: i32,
    now: TimeDateTimeWithTimeZone,
) -> Result<()> {
    if count <= 0 {
        return Ok(());
    }

    let updated = TaskSuites::Entity::update_many()
        .col_expr(
            TaskSuites::Column::IncompleteTasks,
            Expr::col(TaskSuites::Column::IncompleteTasks).sub(count),
        )
        .col_expr(TaskSuites::Column::UpdatedAt, Expr::value(now))
        .filter(TaskSuites::Column::Id.eq(suite_id))
        .exec_with_returning(db)
        .await?;

    // This function cannot repair a negative counter. It does not set the counter to zero, so
    // that the bug is easier to find.
    for suite in updated.iter().filter(|s| s.incomplete_tasks < 0) {
        tracing::error!(
            suite_id = suite.id,
            incomplete_tasks = suite.incomplete_tasks,
            decremented = count,
            "A suite's incomplete_tasks went negative: more tasks were taken off it than were ever counted"
        );
    }

    // This statement has conditions on the state and on the counter. Because of this, it is safe
    // as a second statement without a transaction around the two statements. A submit that occurs
    // at the same time either comes first and makes the counter more than zero, or comes after and
    // sets the state to `Open` itself.
    TaskSuites::Entity::update_many()
        .col_expr(
            TaskSuites::Column::State,
            Expr::value(TaskSuiteState::Complete),
        )
        .col_expr(TaskSuites::Column::CompletedAt, Expr::value(Some(now)))
        .col_expr(TaskSuites::Column::UpdatedAt, Expr::value(now))
        .filter(TaskSuites::Column::Id.eq(suite_id))
        .filter(TaskSuites::Column::State.eq(TaskSuiteState::Closed))
        .filter(TaskSuites::Column::IncompleteTasks.eq(0))
        .exec(db)
        .await?;

    Ok(())
}

/// The maximum number of workers that a plan can request from an agent.
const MAX_WORKER_COUNT: u32 = 256;

/// Make sure that the parameters of a plan are in the permitted range. The create path and the
/// change path both use this function.
// TODO: change this check when the plan has a more flexible definition.
fn validate_worker_schedule(plan: &WorkerSchedulePlan) -> Result<()> {
    match plan {
        WorkerSchedulePlan::FixedWorkers { worker_count, .. } => {
            if *worker_count == 0 || *worker_count > MAX_WORKER_COUNT {
                return Err(Error::ApiError(ApiError::InvalidRequest(format!(
                    "worker_count must be between 1 and {MAX_WORKER_COUNT}, got {worker_count}"
                ))));
            }
        }
    }
    Ok(())
}

/// Hooks use the same executor as tasks, so their specs get the same check as task specs.
fn validate_exec_hooks(hooks: &ExecHooks) -> Result<()> {
    for spec in [&hooks.provision, &hooks.cleanup, &hooks.background]
        .into_iter()
        .flatten()
    {
        check_exec_spec(spec)?;
    }
    Ok(())
}

pub async fn user_create_task_suite(
    user_id: i64,
    pool: &InfraPool,
    CreateTaskSuiteReq {
        name,
        description,
        group_name,
        tags,
        labels,
        priority,
        worker_schedule,
        exec_hooks,
    }: CreateTaskSuiteReq,
) -> crate::error::Result<CreateTaskSuiteResp> {
    validate_worker_schedule(&worker_schedule)?;
    if let Some(hooks) = &exec_hooks {
        validate_exec_hooks(hooks)?;
    }

    if group_name.is_empty() {
        return Err(Error::ApiError(ApiError::InvalidRequest(
            "Group name cannot be empty".to_string(),
        )));
    }

    let tags = Vec::from_iter(tags);
    let labels = Vec::from_iter(labels);
    let now = TimeDateTimeWithTimeZone::now_utc();

    // Resolve the owning group and authorize the caller's Write role.
    let builder = pool.db.get_database_backend();
    let group_stmt = Query::select()
        .column((Group::Entity, Group::Column::Id))
        .from(Group::Entity)
        .join(
            sea_orm::JoinType::Join,
            UserGroup::Entity,
            Expr::col((UserGroup::Entity, UserGroup::Column::GroupId))
                .eq(Expr::col((Group::Entity, Group::Column::Id))),
        )
        .and_where(Expr::col((Group::Entity, Group::Column::GroupName)).eq(group_name.clone()))
        .and_where(Expr::col((UserGroup::Entity, UserGroup::Column::UserId)).eq(user_id))
        .and_where(
            Expr::col((UserGroup::Entity, UserGroup::Column::Role)).gte(UserGroupRole::Write),
        )
        .to_owned();
    let group = GroupIdResult::find_by_statement(builder.build(&group_stmt))
        .one(&pool.db)
        .await?
        .ok_or(Error::ApiError(ApiError::NotFound(format!(
            "User doesn't have permission or group with name {group_name}"
        ))))?;

    let worker_schedule_json = serde_json::to_value(&worker_schedule)?;
    let exec_hooks_json = exec_hooks.map(|h| serde_json::to_value(&h)).transpose()?;

    let suite_uuid = Uuid::new_v4();
    let suite = TaskSuites::ActiveModel {
        uuid: Set(suite_uuid),
        name: Set(name),
        description: Set(description),
        group_id: Set(group.id),
        creator_id: Set(user_id),
        tags: Set(tags),
        labels: Set(labels),
        priority: Set(priority),
        worker_schedule: Set(worker_schedule_json),
        exec_hooks: Set(exec_hooks_json),
        state: Set(TaskSuiteState::Open),
        total_tasks: Set(0),
        incomplete_tasks: Set(0),
        last_task_submitted_at: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        completed_at: Set(None),
        ..Default::default()
    };
    let suite = suite.insert(&pool.db).await?;

    Ok(CreateTaskSuiteResp { uuid: suite.uuid })
}

/// Validate a suite query and resolve/authorize its target group.
///
/// The function rejects a filter set that is present and empty. If `group_name` is absent, it
/// uses the username of the caller. The caller must have at least `Read` in the group.
pub(crate) async fn check_task_suites_query(
    user_id: i64,
    pool: &InfraPool,
    query: &mut TaskSuitesQueryReq,
) -> Result<()> {
    if let Some(ref name) = query.name {
        if name.is_empty() {
            return Err(Error::ApiError(ApiError::InvalidRequest(
                "Suite name cannot be empty if specified".to_string(),
            )));
        }
    }
    if let Some(ref description) = query.description {
        if description.is_empty() {
            return Err(Error::ApiError(ApiError::InvalidRequest(
                "Description cannot be empty if specified".to_string(),
            )));
        }
    }
    if let Some(ref tags) = query.tags {
        if tags.is_empty() {
            return Err(Error::ApiError(ApiError::InvalidRequest(
                "Tags cannot be empty if specified".to_string(),
            )));
        }
    }
    if let Some(ref labels) = query.labels {
        if labels.is_empty() {
            return Err(Error::ApiError(ApiError::InvalidRequest(
                "Labels cannot be empty if specified".to_string(),
            )));
        }
    }
    if let Some(ref creator_usernames) = query.creator_usernames {
        if creator_usernames.is_empty() {
            return Err(Error::ApiError(ApiError::InvalidRequest(
                "Creator username cannot be empty if specified".to_string(),
            )));
        }
    }
    if let Some(ref states) = query.states {
        if states.is_empty() {
            return Err(Error::ApiError(ApiError::InvalidRequest(
                "State cannot be empty if specified".to_string(),
            )));
        }
    }
    if query.group_name.is_none() {
        let username = User::Entity::find()
            .filter(User::Column::Id.eq(user_id))
            .one(&pool.db)
            .await?
            .ok_or(Error::ApiError(ApiError::NotFound("User".to_string())))?
            .username;
        tracing::debug!("No group name specified, use username {} instead", username);
        query.group_name = Some(username);
    }
    if let Some(ref group_name) = query.group_name {
        // Check permission of user to group
        let builder = pool.db.get_database_backend();
        let stmt = Query::select()
            .column((Group::Entity, Group::Column::Id))
            .from(Group::Entity)
            .join(
                sea_orm::JoinType::Join,
                UserGroup::Entity,
                Expr::col((UserGroup::Entity, UserGroup::Column::GroupId))
                    .eq(Expr::col((Group::Entity, Group::Column::Id))),
            )
            .and_where(Expr::col((Group::Entity, Group::Column::GroupName)).eq(group_name.clone()))
            .and_where(Expr::col((UserGroup::Entity, UserGroup::Column::UserId)).eq(user_id))
            .and_where(
                Expr::col((UserGroup::Entity, UserGroup::Column::Role)).gte(UserGroupRole::Read),
            )
            .to_owned();
        let authorized = GroupIdResult::find_by_statement(builder.build(&stmt))
            .one(&pool.db)
            .await?
            .is_some();
        if !authorized {
            return Err(Error::ApiError(ApiError::NotFound(format!(
                "User doesn't have permission or group with name {group_name}"
            ))));
        }
    }
    Ok(())
}

/// Change the properties of a suite. The caller must have `Write` in the group of the suite.
///
/// You cannot change a `Cancelled` suite. You can change a suite in all other states.
///
/// The change applies to the jobs that start after it, because a job copies the worker schedule
/// and the hooks when it starts.
pub async fn user_change_task_suite(
    user_id: i64,
    pool: &InfraPool,
    suite_uuid: Uuid,
    req: ChangeTaskSuiteReq,
) -> Result<()> {
    if req.is_empty() {
        return Err(Error::ApiError(ApiError::InvalidRequest(
            "No change specified".to_string(),
        )));
    }
    if let Some(plan) = &req.worker_schedule {
        validate_worker_schedule(plan)?;
    }
    // Check only the new specs. The server checked the other hooks when it wrote them.
    if let Some(update) = &req.exec_hooks {
        for op in [&update.provision, &update.cleanup, &update.background]
            .into_iter()
            .flatten()
        {
            if let UpdateOp::SetTo(spec) = op {
                check_exec_spec(spec)?;
            }
        }
    }
    // If the change has new tags or a new priority, tell the agents that the suite is available.
    // New tags can let an idle agent run this suite. A new priority can change which suite an idle
    // agent selects or preempt an active agent.
    let needs_notify = req.affects_scheduling();
    let now = TimeDateTimeWithTimeZone::now_utc();

    let suite_id = pool
        .db
        .transaction::<_, i64, Error>(|txn| {
            Box::pin(async move {
                let suite =
                    match authorize_suite(txn, user_id, suite_uuid, UserGroupRole::Write).await {
                        Ok(suite) => suite,
                        Err(ResolveError::Item(e)) => {
                            return Err(Error::ApiError(ApiError::NotFound(e.msg)))
                        }
                        Err(ResolveError::Fatal(e)) => return Err(e),
                    };
                let suite_id = suite.id;

                // `authorize_suite` does not lock the row, so this check only puts the state in
                // the error message. The `WHERE` clause below rejects a suite that a different
                // request cancels after this check.
                if suite.state == TaskSuiteState::Cancelled {
                    return Err(Error::ApiError(ApiError::InvalidRequest(format!(
                        "Cannot change a {} suite",
                        suite.state
                    ))));
                }

                let mut change = TaskSuites::ActiveModel {
                    updated_at: Set(now),
                    ..Default::default()
                };
                if let Some(op) = req.name {
                    change.name = Set(op.into_value());
                }
                if let Some(op) = req.description {
                    change.description = Set(op.into_value());
                }
                if let Some(tags) = req.tags {
                    change.tags = Set(Vec::from_iter(tags));
                }
                if let Some(labels) = req.labels {
                    change.labels = Set(Vec::from_iter(labels));
                }
                if let Some(priority) = req.priority {
                    change.priority = Set(priority);
                }
                if let Some(plan) = req.worker_schedule {
                    change.worker_schedule = Set(serde_json::to_value(plan)?);
                }
                // The request can set, clear or keep each hook, so the new value depends on the
                // current value. If the code reads the row and merges in Rust, two changes at the
                // same time can overwrite the hooks of each other. Thus, the column gets an SQL
                // expression of its current value, and the merge occurs in the `UPDATE`.
                let hooks_expr = match req.exec_hooks {
                    Some(update) => {
                        // A suite with no hooks stores NULL. Use `{}` for NULL, so that the
                        // first hook can be added.
                        let mut hooks = Expr::cust_with_expr(
                            "coalesce($1, '{}'::jsonb)",
                            Expr::col(TaskSuites::Column::ExecHooks),
                        );
                        for (key, op) in [
                            ("provision", update.provision),
                            ("cleanup", update.cleanup),
                            ("background", update.background),
                        ] {
                            hooks = match op {
                                // `||` sets this key and keeps the other keys. `-` removes it.
                                //
                                // Each step has parentheses, because `-` has a higher precedence
                                // than `||`. Without parentheses, PostgreSQL reads the chain as
                                // `x || (obj - 'cleanup')`.
                                Some(UpdateOp::SetTo(spec)) => Expr::cust_with_exprs(
                                    "($1 || jsonb_build_object($2::text, $3::jsonb))",
                                    [
                                        hooks,
                                        Expr::val(key).into(),
                                        Expr::val(serde_json::to_value(spec)?).into(),
                                    ],
                                ),
                                Some(UpdateOp::SetNull) => Expr::cust_with_exprs(
                                    "($1 - $2::text)",
                                    [hooks, Expr::val(key).into()],
                                ),
                                None => hooks,
                            };
                        }
                        // If no hook stays, store NULL, not an empty object.
                        Some(Expr::cust_with_expr("nullif($1, '{}'::jsonb)", hooks))
                    }
                    None => None,
                };

                // `exec_hooks` is not in `change`, so this statement sets it only one time.
                let mut stmt = TaskSuites::Entity::update_many()
                    .set(change)
                    .filter(TaskSuites::Column::Id.eq(suite_id))
                    .filter(TaskSuites::Column::State.ne(TaskSuiteState::Cancelled));
                if let Some(expr) = hooks_expr {
                    stmt = stmt.col_expr(TaskSuites::Column::ExecHooks, expr);
                }
                let changed = stmt.exec(txn).await?;
                if changed.rows_affected == 0 {
                    return Err(Error::ApiError(ApiError::InvalidRequest(format!(
                        "Suite {suite_uuid} was cancelled"
                    ))));
                }
                Ok(suite_id)
            })
        })
        .await?;

    // Each query reads the eligibility and the pick order from the row, so there is nothing to
    // rebuild. A notified agent does a new fetch and gets the latest values.
    if needs_notify {
        agent::notify_suite_available(pool, suite_id).await;
    }
    Ok(())
}

/// Query suites subject to a filter. Only returns suites in a group the caller can read.
pub async fn user_query_task_suites(
    user_id: i64,
    pool: &InfraPool,
    mut query: TaskSuitesQueryReq,
) -> Result<TaskSuitesQueryResp> {
    check_task_suites_query(user_id, pool, &mut query).await?;

    let group_name = query.group_name.clone().unwrap();

    let mut stmt = Query::select();
    if query.count {
        stmt.expr(Expr::col((TaskSuites::Entity, TaskSuites::Column::Uuid)).count());
    } else {
        stmt.columns([
            (TaskSuites::Entity, TaskSuites::Column::Uuid),
            (TaskSuites::Entity, TaskSuites::Column::Name),
            (TaskSuites::Entity, TaskSuites::Column::Description),
            (TaskSuites::Entity, TaskSuites::Column::Tags),
            (TaskSuites::Entity, TaskSuites::Column::Labels),
            (TaskSuites::Entity, TaskSuites::Column::Priority),
            (TaskSuites::Entity, TaskSuites::Column::WorkerSchedule),
            (TaskSuites::Entity, TaskSuites::Column::ExecHooks),
            (TaskSuites::Entity, TaskSuites::Column::State),
            (TaskSuites::Entity, TaskSuites::Column::LastTaskSubmittedAt),
            (TaskSuites::Entity, TaskSuites::Column::TotalTasks),
            (TaskSuites::Entity, TaskSuites::Column::IncompleteTasks),
            (TaskSuites::Entity, TaskSuites::Column::CreatedAt),
            (TaskSuites::Entity, TaskSuites::Column::UpdatedAt),
            (TaskSuites::Entity, TaskSuites::Column::CompletedAt),
        ])
        .expr_as(
            Expr::col((User::Entity, User::Column::Username)),
            Alias::new("creator_username"),
        )
        .expr_as(
            Expr::col((Group::Entity, Group::Column::GroupName)),
            Alias::new("group_name"),
        );
    }

    stmt.from(TaskSuites::Entity)
        .join(
            sea_orm::JoinType::Join,
            User::Entity,
            Expr::col((User::Entity, User::Column::Id)).eq(Expr::col((
                TaskSuites::Entity,
                TaskSuites::Column::CreatorId,
            ))),
        )
        .join(
            sea_orm::JoinType::Join,
            Group::Entity,
            Expr::col((TaskSuites::Entity, TaskSuites::Column::GroupId))
                .eq(Expr::col((Group::Entity, Group::Column::Id))),
        );

    if let Some(ref name) = query.name {
        stmt.and_where(Expr::col((TaskSuites::Entity, TaskSuites::Column::Name)).eq(name.clone()));
    }
    if let Some(ref description) = query.description {
        stmt.and_where(
            Expr::col((TaskSuites::Entity, TaskSuites::Column::Description))
                .like(format!("%{description}%")),
        );
    }
    if let Some(ref creator_usernames) = query.creator_usernames {
        let usernames: Vec<String> = creator_usernames.iter().cloned().collect();
        stmt.and_where(
            Expr::col((User::Entity, User::Column::Username)).eq(PgFunc::any(usernames)),
        );
    }
    if let Some(ref group_name) = query.group_name {
        stmt.and_where(Expr::col((Group::Entity, Group::Column::GroupName)).eq(group_name.clone()));
    }
    if let Some(ref tags) = query.tags {
        let tags_vec: Vec<String> = tags.iter().cloned().collect();
        stmt.and_where(
            Expr::col((TaskSuites::Entity, TaskSuites::Column::Tags)).contains(tags_vec),
        );
    }
    if let Some(ref labels) = query.labels {
        let labels_vec: Vec<String> = labels.iter().cloned().collect();
        stmt.and_where(
            Expr::col((TaskSuites::Entity, TaskSuites::Column::Labels)).contains(labels_vec),
        );
    }
    if let Some(ref states) = query.states {
        let states_vec: Vec<TaskSuiteState> = states.iter().copied().collect();
        stmt.and_where(
            Expr::col((TaskSuites::Entity, TaskSuites::Column::State)).eq(PgFunc::any(states_vec)),
        );
    }
    if let Some(ref priority) = query.priority {
        match parse_operators_with_number(priority)? {
            OperatorWithNumber::Eq(p) => {
                stmt.and_where(Expr::col((TaskSuites::Entity, TaskSuites::Column::Priority)).eq(p));
            }
            OperatorWithNumber::Neq(p) => {
                stmt.and_where(Expr::col((TaskSuites::Entity, TaskSuites::Column::Priority)).ne(p));
            }
            OperatorWithNumber::Gt(p) => {
                stmt.and_where(Expr::col((TaskSuites::Entity, TaskSuites::Column::Priority)).gt(p));
            }
            OperatorWithNumber::Gte(p) => {
                stmt.and_where(
                    Expr::col((TaskSuites::Entity, TaskSuites::Column::Priority)).gte(p),
                );
            }
            OperatorWithNumber::Lt(p) => {
                stmt.and_where(Expr::col((TaskSuites::Entity, TaskSuites::Column::Priority)).lt(p));
            }
            OperatorWithNumber::Lte(p) => {
                stmt.and_where(
                    Expr::col((TaskSuites::Entity, TaskSuites::Column::Priority)).lte(p),
                );
            }
        }
    }
    if !query.count {
        stmt.order_by_expr(
            Expr::col((TaskSuites::Entity, TaskSuites::Column::Priority)).into(),
            sea_orm::Order::Desc,
        )
        .order_by_expr(
            Expr::col((TaskSuites::Entity, TaskSuites::Column::CreatedAt)).into(),
            sea_orm::Order::Asc,
        );
    }
    if let Some(limit) = query.limit {
        stmt.limit(limit);
    }
    if let Some(offset) = query.offset {
        stmt.offset(offset);
    }

    let builder = pool.db.get_database_backend();
    if query.count {
        let count = CountQuery::find_by_statement(builder.build(&stmt))
            .one(&pool.db)
            .await?
            .map(|c| c.count as u64)
            .unwrap_or(0);
        Ok(TaskSuitesQueryResp {
            count,
            suites: vec![],
            group_name,
        })
    } else {
        let suites = TaskSuiteInfo::find_by_statement(builder.build(&stmt))
            .all(&pool.db)
            .await?;
        Ok(TaskSuitesQueryResp {
            count: suites.len() as u64,
            suites,
            group_name,
        })
    }
}

#[derive(FromQueryResult)]
struct SuiteDetailResult {
    id: i64,
    uuid: Uuid,
    name: Option<String>,
    description: Option<String>,
    creator_username: String,
    group_name: String,
    tags: Vec<String>,
    labels: Vec<String>,
    priority: i32,
    worker_schedule: serde_json::Value,
    exec_hooks: Option<serde_json::Value>,
    state: TaskSuiteState,
    last_task_submitted_at: Option<TimeDateTimeWithTimeZone>,
    total_tasks: i32,
    incomplete_tasks: i32,
    created_at: TimeDateTimeWithTimeZone,
    updated_at: TimeDateTimeWithTimeZone,
    completed_at: Option<TimeDateTimeWithTimeZone>,
}

/// Get the details of one suite, with the UUIDs of the agents that can run it and the jobs that run
/// it now. The caller must have at least `Read` in the group of the suite.
pub async fn user_get_task_suite_by_uuid(
    user_id: i64,
    pool: &InfraPool,
    suite_uuid: Uuid,
) -> Result<TaskSuiteQueryResp> {
    let builder = pool.db.get_database_backend();

    let suite_stmt = Query::select()
        .columns([
            (TaskSuites::Entity, TaskSuites::Column::Id),
            (TaskSuites::Entity, TaskSuites::Column::Uuid),
            (TaskSuites::Entity, TaskSuites::Column::Name),
            (TaskSuites::Entity, TaskSuites::Column::Description),
            (TaskSuites::Entity, TaskSuites::Column::Tags),
            (TaskSuites::Entity, TaskSuites::Column::Labels),
            (TaskSuites::Entity, TaskSuites::Column::Priority),
            (TaskSuites::Entity, TaskSuites::Column::WorkerSchedule),
            (TaskSuites::Entity, TaskSuites::Column::ExecHooks),
            (TaskSuites::Entity, TaskSuites::Column::State),
            (TaskSuites::Entity, TaskSuites::Column::LastTaskSubmittedAt),
            (TaskSuites::Entity, TaskSuites::Column::TotalTasks),
            (TaskSuites::Entity, TaskSuites::Column::IncompleteTasks),
            (TaskSuites::Entity, TaskSuites::Column::CreatedAt),
            (TaskSuites::Entity, TaskSuites::Column::UpdatedAt),
            (TaskSuites::Entity, TaskSuites::Column::CompletedAt),
        ])
        .expr_as(
            Expr::col((User::Entity, User::Column::Username)),
            Alias::new("creator_username"),
        )
        .expr_as(
            Expr::col((Group::Entity, Group::Column::GroupName)),
            Alias::new("group_name"),
        )
        .from(TaskSuites::Entity)
        .join(
            sea_orm::JoinType::Join,
            User::Entity,
            Expr::col((User::Entity, User::Column::Id)).eq(Expr::col((
                TaskSuites::Entity,
                TaskSuites::Column::CreatorId,
            ))),
        )
        .join(
            sea_orm::JoinType::Join,
            Group::Entity,
            Expr::col((TaskSuites::Entity, TaskSuites::Column::GroupId))
                .eq(Expr::col((Group::Entity, Group::Column::Id))),
        )
        .join(
            sea_orm::JoinType::Join,
            UserGroup::Entity,
            Expr::col((UserGroup::Entity, UserGroup::Column::GroupId))
                .eq(Expr::col((TaskSuites::Entity, TaskSuites::Column::GroupId))),
        )
        .and_where(Expr::col((UserGroup::Entity, UserGroup::Column::UserId)).eq(user_id))
        .and_where(Expr::col((UserGroup::Entity, UserGroup::Column::Role)).gte(UserGroupRole::Read))
        .and_where(Expr::col((TaskSuites::Entity, TaskSuites::Column::Uuid)).eq(suite_uuid))
        .to_owned();

    let suite = SuiteDetailResult::find_by_statement(builder.build(&suite_stmt))
        .one(&pool.db)
        .await?
        .ok_or(Error::ApiError(ApiError::NotFound(format!(
            "User doesn't have permission or suite with uuid {suite_uuid}"
        ))))?;

    // The tag-matched agents and the agents included by hand, without the agents excluded by hand.
    // Refer to `service::agent::matching`.
    let eligible_agents = agent::matching::eligible_agent_uuids(&pool.db, suite.id).await?;

    // The jobs of the suite that are not terminal: the agents that run it now.
    let jobs_stmt = Query::select()
        .columns([
            (SuiteAgentJobs::Entity, SuiteAgentJobs::Column::JobId),
            (SuiteAgentJobs::Entity, SuiteAgentJobs::Column::State),
            (SuiteAgentJobs::Entity, SuiteAgentJobs::Column::CreatedAt),
            (SuiteAgentJobs::Entity, SuiteAgentJobs::Column::UpdatedAt),
        ])
        .expr_as(
            Expr::col((Agent::Entity, Agent::Column::Uuid)),
            Alias::new("agent_uuid"),
        )
        .from(SuiteAgentJobs::Entity)
        // `agent_id` is nullable, thus a left join.
        .join(
            sea_orm::JoinType::LeftJoin,
            Agent::Entity,
            Expr::col((Agent::Entity, Agent::Column::Id)).eq(Expr::col((
                SuiteAgentJobs::Entity,
                SuiteAgentJobs::Column::AgentId,
            ))),
        )
        .and_where(
            Expr::col((SuiteAgentJobs::Entity, SuiteAgentJobs::Column::TaskSuiteId)).eq(suite.id),
        )
        .and_where(
            Expr::col((SuiteAgentJobs::Entity, SuiteAgentJobs::Column::State))
                .is_in(agent::job::IN_FLIGHT),
        )
        .order_by_expr(
            Expr::col((SuiteAgentJobs::Entity, SuiteAgentJobs::Column::JobId)).into(),
            sea_orm::Order::Asc,
        )
        .to_owned();
    let active_jobs = SuiteJobInfo::find_by_statement(builder.build(&jobs_stmt))
        .all(&pool.db)
        .await?;

    let worker_schedule: WorkerSchedulePlan = serde_json::from_value(suite.worker_schedule)?;
    let exec_hooks: Option<ExecHooks> = suite.exec_hooks.map(serde_json::from_value).transpose()?;

    Ok(TaskSuiteQueryResp {
        info: ParsedTaskSuiteInfo {
            uuid: suite.uuid,
            name: suite.name,
            description: suite.description,
            group_name: suite.group_name,
            creator_username: suite.creator_username,
            tags: suite.tags,
            labels: suite.labels,
            priority: suite.priority,
            worker_schedule,
            exec_hooks,
            state: suite.state,
            last_task_submitted_at: suite.last_task_submitted_at,
            total_tasks: suite.total_tasks,
            incomplete_tasks: suite.incomplete_tasks,
            created_at: suite.created_at,
            updated_at: suite.updated_at,
            completed_at: suite.completed_at,
        },
        eligible_agents,
        active_jobs,
    })
}

/// Close a suite (`Open → Closed`).
pub async fn user_close_task_suite(user_id: i64, pool: &InfraPool, suite_uuid: Uuid) -> Result<()> {
    let now = TimeDateTimeWithTimeZone::now_utc();

    pool.db
        .transaction::<_, (), Error>(|txn| {
            Box::pin(async move {
                // Resolve the suite and authorize the caller's Write role
                let suite =
                    match authorize_suite(txn, user_id, suite_uuid, UserGroupRole::Write).await {
                        Ok(suite) => suite,
                        Err(ResolveError::Item(e)) => {
                            return Err(Error::ApiError(ApiError::NotFound(e.msg)))
                        }
                        Err(ResolveError::Fatal(e)) => return Err(e),
                    };

                // `authorize_suite` does not lock the row, so this check only puts the state in
                // the error message. The `WHERE` clause below rejects a second close.
                if suite.state != TaskSuiteState::Open {
                    return Err(Error::ApiError(ApiError::InvalidRequest(format!(
                        "Cannot transition from {} to Closed",
                        suite.state
                    ))));
                }

                let closed = TaskSuites::Entity::update_many()
                    .col_expr(
                        TaskSuites::Column::State,
                        Expr::value(TaskSuiteState::Closed),
                    )
                    .col_expr(TaskSuites::Column::UpdatedAt, Expr::value(now))
                    .filter(TaskSuites::Column::Id.eq(suite.id))
                    .filter(TaskSuites::Column::State.eq(TaskSuiteState::Open))
                    .exec(txn)
                    .await?;
                if closed.rows_affected == 0 {
                    return Err(Error::ApiError(ApiError::InvalidRequest(format!(
                        "Suite {suite_uuid} is no longer Open"
                    ))));
                }
                Ok(())
            })
        })
        .await?;

    Ok(())
}

/// Cancel a suite (`* → Cancelled`). `Cancelled` is a terminal state. The function archives all
/// tasks of the suite that are not in a terminal state, with the state `Cancelled`. The caller
/// must have `Write` or `Admin` in the group of the suite.
///
/// `op` sets what occurs to the jobs that run the suite:
/// - `Graceful`: the server tells each agent that the suite is cancelled, and which of its tasks
///   are cancelled. The agent stops taking tasks, runs the cleanup hook, and ends the job as
///   `Completed`.
/// - `Force`: the server marks the jobs `Killed` immediately, and the cleanup hook does not run.
///   The server tells the agents to stop.
pub async fn user_cancel_task_suite(
    user_id: i64,
    pool: &InfraPool,
    suite_uuid: Uuid,
    op: CancelTaskSuiteOp,
) -> Result<()> {
    let now = TimeDateTimeWithTimeZone::now_utc();
    let builder = pool.db.get_database_backend();

    let (suite_id, cancelled_task_uuids) = pool
        .db
        .transaction::<_, (i64, Vec<Uuid>), Error>(|txn| {
            Box::pin(async move {
                // Resolve the suite and authorize the caller's Write role in one join.
                let suite =
                    match authorize_suite(txn, user_id, suite_uuid, UserGroupRole::Write).await {
                        Ok(suite) => suite,
                        Err(ResolveError::Item(e)) => {
                            return Err(Error::ApiError(ApiError::NotFound(e.msg)))
                        }
                        Err(ResolveError::Fatal(e)) => return Err(e),
                    };

                if matches!(suite.state, TaskSuiteState::Cancelled) {
                    return Err(Error::ApiError(ApiError::InvalidRequest(format!(
                        "Suite {suite_uuid} is already in cancelled state"
                    ))));
                }

                // Cancel the suite *before* the archive of its tasks, not after. This statement
                // locks the suite row. Each submit updates the same row with the condition
                // `state <> Cancelled`. Thus, after this commit, a submit at the same time either
                // came before the delete below, or the server rejects it. If the code writes the
                // suite last, a submit between the delete and the write can add a task to a
                // cancelled suite. No agent will claim that task, and `incomplete_tasks` is
                // already zero.
                //
                // For the same reason, zero is the correct counter value. The delete below
                // removes all active rows of the suite, and no request can add a new row.
                let fenced = TaskSuites::Entity::update_many()
                    .col_expr(
                        TaskSuites::Column::State,
                        Expr::value(TaskSuiteState::Cancelled),
                    )
                    .col_expr(TaskSuites::Column::IncompleteTasks, Expr::value(0))
                    .col_expr(TaskSuites::Column::UpdatedAt, Expr::value(now))
                    .col_expr(TaskSuites::Column::CompletedAt, Expr::value(Some(now)))
                    .filter(TaskSuites::Column::Id.eq(suite.id))
                    .filter(TaskSuites::Column::State.ne(TaskSuiteState::Cancelled))
                    .exec(txn)
                    .await?;
                if fenced.rows_affected == 0 {
                    return Err(Error::ApiError(ApiError::InvalidRequest(format!(
                        "Suite {suite_uuid} is already in cancelled state"
                    ))));
                }

                // All cancelled tasks share this result.
                let result = serde_json::to_value(TaskResultSpec {
                    exit_status: 0,
                    msg: Some(TaskResultMessage::UserCancellation),
                })
                .inspect_err(|e| tracing::error!("{}", e))?;

                // Do not return the rows to Rust. With many tasks, the rows cause the PostgreSQL
                // error for too many arguments in the archive statement.
                //
                // The delete also removes `Cancelled` rows, and this is intentional. This state
                // means that an agent reported `Cancel`, but did not send the `Commit` that
                // archives the task. If these rows stay, they stay in `active_tasks` permanently.
                // The terminal job will reject the `Commit`, no other agent can claim a task that
                // is not `Ready`, and no process recovers this state.
                let delete_stmt = DeleteStatement::new()
                    .from_table(ActiveTasks::Entity)
                    .and_where(Expr::col(ActiveTasks::Column::TaskSuiteId).eq(suite.id))
                    .and_where(
                        Expr::col(ActiveTasks::Column::State).is_not_in([TaskState::Unknown]),
                    )
                    .returning_all()
                    .to_owned();
                let deleted_cte = CommonTableExpression::new()
                    .query(delete_stmt)
                    .table_name(Alias::new("deleted"))
                    .to_owned();

                // The archived copy. It reads each column from `deleted` by name, and replaces
                // `updated_at`, `state` and `result` with three values. The statement binds each of
                // these values one time.
                let select_from_deleted = Query::select()
                    .expr(Expr::col(Alias::new("id")))
                    .expr(Expr::col(Alias::new("creator_id")))
                    .expr(Expr::col(Alias::new("group_id")))
                    .expr(Expr::col(Alias::new("task_id")))
                    .expr(Expr::col(Alias::new("uuid")))
                    .expr(Expr::col(Alias::new("tags")))
                    .expr(Expr::col(Alias::new("labels")))
                    .expr(Expr::col(Alias::new("created_at")))
                    .expr(Expr::value(now))
                    .expr(Expr::value(TaskState::Cancelled))
                    .expr(Expr::col(Alias::new("runner_uuid")))
                    .expr(Expr::col(Alias::new("exec_options")))
                    .expr(Expr::col(Alias::new("priority")))
                    .expr(Expr::col(Alias::new("spec")))
                    .expr(Expr::value(result))
                    .expr(Expr::col(Alias::new("upstream_task_uuid")))
                    .expr(Expr::col(Alias::new("downstream_task_uuid")))
                    .expr(Expr::col(Alias::new("task_suite_id")))
                    .from(Alias::new("deleted"))
                    .to_owned();

                let mut insert_stmt = InsertStatement::new()
                    .into_table(ArchivedTasks::Entity)
                    .columns([
                        ArchivedTasks::Column::Id,
                        ArchivedTasks::Column::CreatorId,
                        ArchivedTasks::Column::GroupId,
                        ArchivedTasks::Column::TaskId,
                        ArchivedTasks::Column::Uuid,
                        ArchivedTasks::Column::Tags,
                        ArchivedTasks::Column::Labels,
                        ArchivedTasks::Column::CreatedAt,
                        ArchivedTasks::Column::UpdatedAt,
                        ArchivedTasks::Column::State,
                        ArchivedTasks::Column::RunnerUuid,
                        ArchivedTasks::Column::ExecOptions,
                        ArchivedTasks::Column::Priority,
                        ArchivedTasks::Column::Spec,
                        ArchivedTasks::Column::Result,
                        ArchivedTasks::Column::UpstreamTaskUuid,
                        ArchivedTasks::Column::DownstreamTaskUuid,
                        ArchivedTasks::Column::TaskSuiteId,
                    ])
                    .to_owned();
                // This cannot fail: the column list and the select have the same number of
                // columns. This is a property of the code, not of the request.
                insert_stmt.select_from(select_from_deleted).unwrap();
                let archived_cte = CommonTableExpression::new()
                    .query(insert_stmt)
                    .table_name(Alias::new("archived"))
                    .to_owned();

                // Agents hold only the tasks in this subset. The server tells these agents the
                // UUIDs of the removed tasks. The agents then stop, and do not report on rows
                // that this statement archived.
                //
                // This reads `deleted`, not `archived`, because it needs the state of each task
                // *before* the archive changed it to `Cancelled`. The query does not use
                // `archived`, and it does not have to: PostgreSQL runs a data-modifying CTE one
                // time and completely, also when the main query does not read its output.
                let in_flight_stmt = Query::select()
                    .expr(Expr::col(Alias::new("uuid")))
                    .from(Alias::new("deleted"))
                    .and_where(Expr::col(Alias::new("state")).is_in([
                        TaskState::Running,
                        TaskState::Finished,
                        TaskState::Cancelled,
                    ]))
                    .to_owned();

                let with_ctes = WithClause::new()
                    .cte(deleted_cte)
                    .cte(archived_cte)
                    .to_owned();
                let cancelled_task_uuids: Vec<Uuid> = TaskUuidResult::find_by_statement(
                    builder.build(&in_flight_stmt.with(with_ctes)),
                )
                .all(txn)
                .await?
                .into_iter()
                .map(|t| t.uuid)
                .collect();

                let suite_id = suite.id;
                Ok((suite_id, cancelled_task_uuids))
            })
        })
        .await?;

    // Stop the jobs that still run. `Force` writes the terminal state, thus the coordinator rejects
    // later reports of the agents. `Graceful` does not change the jobs: each agent runs the cleanup
    // hook and moves its job to `Completed`.
    let graceful = matches!(op, CancelTaskSuiteOp::Graceful);
    let running_agents = match op {
        CancelTaskSuiteOp::Force => {
            pool.db
                .transaction::<_, Vec<Uuid>, Error>(move |txn| {
                    Box::pin(async move { agent::job::kill_suite_jobs(txn, suite_id, now).await })
                })
                .await?
        }
        CancelTaskSuiteOp::Graceful => agent::job::agents_running_suite(&pool.db, suite_id).await?,
    };

    for agent_uuid in running_agents {
        AgentWsRouter::notify(
            &pool.ws_router_tx,
            agent_uuid,
            AgentNotification::SuiteCancelled {
                suite_uuid,
                reason: "Suite was cancelled by a user".to_string(),
                graceful,
            },
        );
        if !cancelled_task_uuids.is_empty() {
            AgentWsRouter::notify(
                &pool.ws_router_tx,
                agent_uuid,
                AgentNotification::TasksCancelled {
                    task_uuids: cancelled_task_uuids.clone(),
                },
            );
        }
    }

    Ok(())
}

/// Batch-apply agent overrides for a single suite. The suite is resolved once
/// (requiring the caller's Write role on its group); each agent is then resolved and
/// applied independently. Per-agent failures are collected into the response rather than
/// aborting the batch; only genuine DB errors roll the whole transaction back.
pub async fn user_override_agents_for_suite(
    user_id: i64,
    pool: &InfraPool,
    suite_uuid: Uuid,
    req: SuiteAgentOverrideReq,
) -> Result<SuiteAgentOverrideResp> {
    let now = TimeDateTimeWithTimeZone::now_utc();

    let (suite_id, errors) = pool
        .db
        .transaction::<_, (i64, HashMap<Uuid, crate::error::ErrorMsg>), Error>(|txn| {
            Box::pin(async move {
                // The suite is fixed for the whole batch, so a suite-level failure is request-level
                let suite =
                    match authorize_suite(txn, user_id, suite_uuid, UserGroupRole::Write).await {
                        Ok(suite) => suite,
                        Err(ResolveError::Item(e)) => {
                            return Err(Error::ApiError(ApiError::NotFound(e.msg)))
                        }
                        Err(ResolveError::Fatal(e)) => return Err(e),
                    };

                let mut errors = HashMap::new();
                for (agent_uuid, action) in req.overrides {
                    let outcome = async {
                        let agent = resolve_agent(txn, agent_uuid).await?;
                        apply_suite_agent_override(txn, user_id, &suite, &agent, action, now).await
                    }
                    .await;
                    match outcome {
                        Ok(()) => {}
                        Err(ResolveError::Item(e)) => {
                            errors.insert(agent_uuid, e);
                        }
                        Err(ResolveError::Fatal(e)) => {
                            // Some db error occurred, roll back and error
                            return Err(e);
                        }
                    }
                }

                Ok((suite.id, errors))
            })
        })
        .await?;

    // The overrides changed the set of agents that can run the suite.
    agent::notify_suite_available(pool, suite_id).await;

    Ok(SuiteAgentOverrideResp { errors })
}

/// Query the jobs of a suite, newest first. The caller must have at least `Read` in the group of
/// the suite.
pub async fn user_query_suite_jobs(
    user_id: i64,
    pool: &InfraPool,
    suite_uuid: Uuid,
    query: SuiteJobsQueryReq,
) -> Result<SuiteJobsQueryResp> {
    let suite = match authorize_suite(&pool.db, user_id, suite_uuid, UserGroupRole::Read).await {
        Ok(suite) => suite,
        Err(ResolveError::Item(e)) => return Err(Error::ApiError(ApiError::NotFound(e.msg))),
        Err(ResolveError::Fatal(e)) => return Err(e),
    };

    let mut stmt = Query::select();
    if query.count {
        stmt.expr(Expr::col((SuiteAgentJobs::Entity, SuiteAgentJobs::Column::Id)).count());
    } else {
        stmt.columns([
            (SuiteAgentJobs::Entity, SuiteAgentJobs::Column::JobId),
            (SuiteAgentJobs::Entity, SuiteAgentJobs::Column::State),
            (SuiteAgentJobs::Entity, SuiteAgentJobs::Column::CreatedAt),
            (SuiteAgentJobs::Entity, SuiteAgentJobs::Column::UpdatedAt),
        ])
        .expr_as(
            Expr::col((Agent::Entity, Agent::Column::Uuid)),
            Alias::new("agent_uuid"),
        );
    }

    stmt.from(SuiteAgentJobs::Entity)
        // `agent_id` is nullable, thus a left join.
        .join(
            sea_orm::JoinType::LeftJoin,
            Agent::Entity,
            Expr::col((Agent::Entity, Agent::Column::Id)).eq(Expr::col((
                SuiteAgentJobs::Entity,
                SuiteAgentJobs::Column::AgentId,
            ))),
        )
        .and_where(
            Expr::col((SuiteAgentJobs::Entity, SuiteAgentJobs::Column::TaskSuiteId)).eq(suite.id),
        );

    if let Some(ref states) = query.states {
        let states_vec: Vec<SuiteJobState> = states.iter().copied().collect();
        stmt.and_where(
            Expr::col((SuiteAgentJobs::Entity, SuiteAgentJobs::Column::State))
                .eq(PgFunc::any(states_vec)),
        );
    }
    if let Some(agent_uuid) = query.agent_uuid {
        stmt.and_where(Expr::col((Agent::Entity, Agent::Column::Uuid)).eq(agent_uuid));
    }
    if let Some(limit) = query.limit {
        stmt.limit(limit);
    }
    if let Some(offset) = query.offset {
        stmt.offset(offset);
    }

    let builder = pool.db.get_database_backend();
    if query.count {
        let count = CountQuery::find_by_statement(builder.build(&stmt))
            .one(&pool.db)
            .await?
            .map(|c| c.count as u64)
            .unwrap_or(0);
        Ok(SuiteJobsQueryResp {
            count,
            jobs: vec![],
        })
    } else {
        stmt.order_by_expr(
            Expr::col((SuiteAgentJobs::Entity, SuiteAgentJobs::Column::JobId)).into(),
            sea_orm::Order::Desc,
        );
        let jobs = SuiteJobInfo::find_by_statement(builder.build(&stmt))
            .all(&pool.db)
            .await?;
        Ok(SuiteJobsQueryResp {
            count: jobs.len() as u64,
            jobs,
        })
    }
}

/// Get one job of a suite with its hooks. The artifacts of a hook use the hook uuid on the artifact
/// endpoints. The caller must have at least `Read` in the group of the suite.
pub async fn user_get_suite_job(
    user_id: i64,
    pool: &InfraPool,
    suite_uuid: Uuid,
    job_id: i32,
) -> Result<SuiteJobQueryResp> {
    let suite = match authorize_suite(&pool.db, user_id, suite_uuid, UserGroupRole::Read).await {
        Ok(suite) => suite,
        Err(ResolveError::Item(e)) => return Err(Error::ApiError(ApiError::NotFound(e.msg))),
        Err(ResolveError::Fatal(e)) => return Err(e),
    };

    let job = SuiteAgentJobs::Entity::find()
        .filter(SuiteAgentJobs::Column::TaskSuiteId.eq(suite.id))
        .filter(SuiteAgentJobs::Column::JobId.eq(job_id))
        .one(&pool.db)
        .await?
        .ok_or(Error::ApiError(ApiError::NotFound(format!(
            "Job {job_id} of suite {suite_uuid}"
        ))))?;

    let agent_uuid = match job.agent_id {
        Some(agent_id) => Agent::Entity::find_by_id(agent_id)
            .one(&pool.db)
            .await?
            .map(|a| a.uuid),
        None => None,
    };

    let hooks = HookTasks::Entity::find()
        .filter(HookTasks::Column::SuiteAgentJobId.eq(job.id))
        .all(&pool.db)
        .await?
        .into_iter()
        .map(|hook| {
            Ok(HookTaskInfo {
                uuid: hook.uuid,
                hook_type: hook.hook_type,
                state: hook.state,
                result: hook.result.map(serde_json::from_value).transpose()?,
                started_at: hook.started_at,
                completed_at: hook.completed_at,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(SuiteJobQueryResp {
        info: SuiteJobInfo {
            job_id: job.job_id,
            state: job.state,
            agent_uuid,
            created_at: job.created_at,
            updated_at: job.updated_at,
        },
        hooks,
    })
}

/// Stop one job of a suite, by its number in the suite. The caller must have at least `Write` in
/// the group of the suite.
///
/// The result is the same as the stop on the agent: the agent stays up and picks a suite again,
/// possibly this suite.
pub async fn user_stop_suite_job(
    user_id: i64,
    pool: &InfraPool,
    suite_uuid: Uuid,
    job_id: i32,
    op: StopJobOp,
) -> Result<StopAgentJobResp> {
    let suite = match authorize_suite(&pool.db, user_id, suite_uuid, UserGroupRole::Write).await {
        Ok(suite) => suite,
        Err(ResolveError::Item(e)) => return Err(Error::ApiError(ApiError::NotFound(e.msg))),
        Err(ResolveError::Fatal(e)) => return Err(e),
    };

    let job = SuiteAgentJobs::Entity::find()
        .filter(SuiteAgentJobs::Column::TaskSuiteId.eq(suite.id))
        .filter(SuiteAgentJobs::Column::JobId.eq(job_id))
        .one(&pool.db)
        .await?
        .ok_or(Error::ApiError(ApiError::NotFound(format!(
            "Job {job_id} of suite {suite_uuid}"
        ))))?;

    // A terminal job, or a job without an agent, has nothing to stop.
    let agent = match (job.state.is_terminal(), job.agent_id) {
        (false, Some(agent_id)) => Agent::Entity::find_by_id(agent_id).one(&pool.db).await?,
        _ => None,
    };
    let Some(agent) = agent else {
        return Ok(StopAgentJobResp {
            stopped: false,
            suite_uuid: None,
            job_id: None,
        });
    };

    agent::stop_agent_job(pool, &agent, &job, suite_uuid, op).await
}

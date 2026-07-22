use std::collections::HashMap;

use sea_orm::sea_query::extension::postgres::PgExpr;
use sea_orm::sea_query::{Alias, PgFunc, Query};
use sea_orm::{prelude::*, ConnectionTrait, FromQueryResult, Set, TransactionTrait};
use uuid::Uuid;

use super::suite_agent::{
    apply_suite_agent_selection, authorize_suite, resolve_agent, SelectionItemError,
};
use crate::config::InfraPool;
use crate::entity::role::UserGroupRole;
use crate::entity::state::{TaskState, TaskSuiteState};
use crate::entity::task_suite_agent::SuiteAgentSelectionType;
use crate::entity::{
    active_tasks as ActiveTasks, agents as Agent, archived_tasks as ArchivedTasks, groups as Group,
    task_suite_agent as TaskSuiteAgent, task_suites as TaskSuites, user_group as UserGroup,
    users as User,
};
use crate::error::{ApiError, AuthError, Error, Result};
use crate::schema::{
    CancelTaskSuiteOp, CountQuery, CreateTaskSuiteReq, CreateTaskSuiteResp, ExecHooks,
    ParsedTaskSuiteInfo, SuiteAgentSelectionError, SuiteAgentSelectionReq, SuiteAgentSelectionResp,
    TaskResultMessage, TaskResultSpec, TaskSuiteInfo, TaskSuiteQueryResp, TaskSuitesQueryReq,
    TaskSuitesQueryResp, WorkerSchedulePlan,
};
use crate::service::task::{parse_operators_with_number, OperatorWithNumber};

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
    // Validate the worker schedule based on the policy variant.
    match &worker_schedule {
        // TODO: this should finally be adjusted to some more flexible definitions
        WorkerSchedulePlan::FixedWorkers {
            worker_count,
            task_prefetch_count,
            ..
        } => {
            if *worker_count < 1 || *worker_count > 256 {
                return Err(Error::ApiError(ApiError::InvalidRequest(
                    "worker_count must be between 1 and 256".to_string(),
                )));
            }
            if *task_prefetch_count == 0 {
                return Err(Error::ApiError(ApiError::InvalidRequest(
                    "task_prefetch_count must be > 0".to_string(),
                )));
            }
        }
    }

    if group_name.is_empty() {
        return Err(Error::ApiError(ApiError::InvalidRequest(
            "group_name is required".to_string(),
        )));
    }

    // A suite may be unnamed (None), but a present name must not be blank.
    if let Some(ref name) = name {
        if name.trim().is_empty() {
            return Err(Error::ApiError(ApiError::InvalidRequest(
                "name cannot be empty if specified".to_string(),
            )));
        }
    }

    let tags = Vec::from_iter(tags);
    let labels = Vec::from_iter(labels);
    let now = TimeDateTimeWithTimeZone::now_utc();

    // Resolve the owning group.
    let group = Group::Entity::find()
        .filter(Group::Column::GroupName.eq(&group_name))
        .one(&pool.db)
        .await?
        .ok_or(Error::ApiError(ApiError::NotFound(format!(
            "Group with name {group_name}"
        ))))?;

    // Permission check: the user must have Write/Admin in the group.
    UserGroup::Entity::find()
        .filter(UserGroup::Column::UserId.eq(user_id))
        .filter(UserGroup::Column::GroupId.eq(group.id))
        .one(&pool.db)
        .await?
        .and_then(|ug| (ug.role >= UserGroupRole::Write).then_some(()))
        .ok_or(Error::AuthError(AuthError::PermissionDenied))?;

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

#[derive(FromQueryResult)]
struct UserGroupRoleQueryRes {
    role: UserGroupRole,
}

/// Validate a suite query and resolve/authorize its target group.
///
/// Rejects empty-but-present filter sets, defaults `group_name` to the caller's
/// username when omitted, and requires the caller to have at least `Read` in the
/// resolved group.
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
        let builder = pool.db.get_database_backend();
        let role_stmt = Query::select()
            .column((UserGroup::Entity, UserGroup::Column::Role))
            .from(UserGroup::Entity)
            .join(
                sea_orm::JoinType::Join,
                Group::Entity,
                Expr::col((Group::Entity, Group::Column::Id))
                    .eq(Expr::col((UserGroup::Entity, UserGroup::Column::GroupId))),
            )
            .and_where(Expr::col((UserGroup::Entity, UserGroup::Column::UserId)).eq(user_id))
            .and_where(Expr::col((Group::Entity, Group::Column::GroupName)).eq(group_name.clone()))
            .to_owned();
        let query_role = UserGroupRoleQueryRes::find_by_statement(builder.build(&role_stmt))
            .one(&pool.db)
            .await?
            .map(|r| r.role);
        match query_role {
            Some(r) if r >= UserGroupRole::Read => {}
            Some(_) => {
                return Err(Error::AuthError(AuthError::PermissionDenied));
            }
            None => {
                return Err(Error::ApiError(ApiError::InvalidRequest(format!(
                    "Group with name {group_name} not found or user is not in the group"
                ))));
            }
        }
    }
    Ok(())
}

/// Apply the `TaskSuitesQueryReq` filters to a suite select statement.
fn apply_suite_filters(
    stmt: &mut sea_orm::sea_query::SelectStatement,
    query: &TaskSuitesQueryReq,
) -> Result<()> {
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
    if let Some(limit) = query.limit {
        stmt.limit(limit);
    }
    if let Some(offset) = query.offset {
        stmt.offset(offset);
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

    apply_suite_filters(&mut stmt, &query)?;

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

#[derive(FromQueryResult)]
struct AgentUuidResult {
    uuid: Uuid,
}

/// Get a single suite's details, including the uuids of its currently-assigned agents.
///
/// User must be a member of the suite's group.
///
/// "Assigned" here means the rows persisted in `task_suite_agent` (manual
/// includes/excludes); tag-matched agents are computed in-memory by the agent
/// scheduler and are not part of this read.
pub async fn user_get_task_suite_by_uuid(
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
        .and_where(Expr::col((TaskSuites::Entity, TaskSuites::Column::Uuid)).eq(suite_uuid))
        .to_owned();

    let suite = SuiteDetailResult::find_by_statement(builder.build(&suite_stmt))
        .one(&pool.db)
        .await?
        .ok_or(Error::ApiError(ApiError::NotFound(format!(
            "Task suite with uuid {suite_uuid}"
        ))))?;

    // Only the manually-included agents are persisted; excluded rows must not be
    // reported as assigned.
    //
    // TODO: this is only the manual half. The *effective* assigned set is
    // `(in-memory tag-matched ∪ UserIncluded) − UserExcluded`. Once the agent scheduler
    // exists, merge its in-memory tag-matched set here (and subtract UserExcluded) to
    // produce the real assigned-agent list.
    let agent_stmt = Query::select()
        .column((Agent::Entity, Agent::Column::Uuid))
        .from(Agent::Entity)
        .join(
            sea_orm::JoinType::Join,
            TaskSuiteAgent::Entity,
            Expr::col((TaskSuiteAgent::Entity, TaskSuiteAgent::Column::AgentId))
                .eq(Expr::col((Agent::Entity, Agent::Column::Id))),
        )
        .and_where(
            Expr::col((TaskSuiteAgent::Entity, TaskSuiteAgent::Column::TaskSuiteId)).eq(suite.id),
        )
        .and_where(
            Expr::col((
                TaskSuiteAgent::Entity,
                TaskSuiteAgent::Column::SelectionType,
            ))
            .eq(SuiteAgentSelectionType::UserIncluded),
        )
        .to_owned();

    let eligible_agents = AgentUuidResult::find_by_statement(builder.build(&agent_stmt))
        .all(&pool.db)
        .await?
        .into_iter()
        .map(|m| m.uuid)
        .collect();

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
    })
}

/// Set a suite `Open → Closed`. Idempotency is enforced by the `state = Open` filter.
pub(crate) async fn close_task_suite<C>(
    db: &C,
    task_suite_id: i64,
    now: TimeDateTimeWithTimeZone,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let updated = TaskSuites::Entity::update_many()
        .col_expr(
            TaskSuites::Column::State,
            Expr::value(TaskSuiteState::Closed),
        )
        .col_expr(TaskSuites::Column::UpdatedAt, Expr::value(now))
        .filter(TaskSuites::Column::Id.eq(task_suite_id))
        .filter(TaskSuites::Column::State.eq(TaskSuiteState::Open))
        .exec(db)
        .await?;

    if updated.rows_affected != 1 {
        return Err(Error::ApiError(ApiError::NotFound(
            "Task suite not found or already closed".to_string(),
        )));
    }
    Ok(())
}

/// Close a suite (`Open → Closed`). `Closed` is only an idle marker — the suite
/// still accepts and runs tasks, and a new task reopens it. Requires Write/Admin
/// in the suite's group.
pub async fn user_close_task_suite(user_id: i64, pool: &InfraPool, suite_uuid: Uuid) -> Result<()> {
    let now = TimeDateTimeWithTimeZone::now_utc();

    pool.db
        .transaction::<_, (), Error>(|txn| {
            Box::pin(async move {
                let suite = TaskSuites::Entity::find()
                    .filter(TaskSuites::Column::Uuid.eq(suite_uuid))
                    .one(txn)
                    .await?
                    .ok_or(Error::ApiError(ApiError::NotFound(format!(
                        "Task suite with uuid {suite_uuid}"
                    ))))?;

                UserGroup::Entity::find()
                    .filter(UserGroup::Column::UserId.eq(user_id))
                    .filter(UserGroup::Column::GroupId.eq(suite.group_id))
                    .one(txn)
                    .await?
                    .and_then(|ug| (ug.role >= UserGroupRole::Write).then_some(()))
                    .ok_or(Error::AuthError(AuthError::PermissionDenied))?;

                if suite.state != TaskSuiteState::Open {
                    return Err(Error::ApiError(ApiError::InvalidRequest(format!(
                        "Cannot transition from {} to Closed",
                        suite.state
                    ))));
                }

                close_task_suite(txn, suite.id, now).await
            })
        })
        .await?;

    Ok(())
}

/// Cancel a suite (`* → Cancelled`, terminal). Archives every non-terminal task
/// of the suite as `Cancelled`. Requires Write/Admin in the suite's group.
///
/// `op` (`Graceful`/`Force`) currently has no differentiated effect: its only
/// distinction is how running agents/jobs tear down and get notified, which lives
/// in the agent layer that is not ported yet. The parameter is accepted now so the
/// API stays stable; wire its behavior in with the agent work (see the seams below).
pub async fn user_cancel_task_suite(
    user_id: i64,
    pool: &InfraPool,
    suite_uuid: Uuid,
    _op: CancelTaskSuiteOp,
) -> Result<()> {
    let now = TimeDateTimeWithTimeZone::now_utc();

    pool.db
        .transaction::<_, u64, Error>(|txn| {
            Box::pin(async move {
                let suite = TaskSuites::Entity::find()
                    .filter(TaskSuites::Column::Uuid.eq(suite_uuid))
                    .one(txn)
                    .await?
                    .ok_or(Error::ApiError(ApiError::NotFound(format!(
                        "Task suite with uuid {suite_uuid}"
                    ))))?;

                UserGroup::Entity::find()
                    .filter(UserGroup::Column::UserId.eq(user_id))
                    .filter(UserGroup::Column::GroupId.eq(suite.group_id))
                    .one(txn)
                    .await?
                    .and_then(|ug| (ug.role >= UserGroupRole::Write).then_some(()))
                    .ok_or(Error::AuthError(AuthError::PermissionDenied))?;

                if matches!(suite.state, TaskSuiteState::Cancelled) {
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

                // Archive every non-terminal task of the suite as Cancelled. With no
                // agent layer yet, a suite's tasks only ever reach Ready/Pending; the
                // Running/Finished states are included for forward-compatibility once
                // agents execute them.
                let tasks = ActiveTasks::Entity::find()
                    .filter(ActiveTasks::Column::TaskSuiteId.eq(suite.id))
                    .filter(ActiveTasks::Column::State.is_in([
                        TaskState::Pending,
                        TaskState::Ready,
                        TaskState::Running,
                        TaskState::Finished,
                    ]))
                    .all(txn)
                    .await?;

                // The inflight subset is the only set tied to an executing agent: each
                // carries the `runner_uuid` of the agent running it. A Force cancel must
                // signal those agents to stop so they don't commit tasks we just archived.
                // Collected now and intentionally left unused.
                // TODO: wire this to a per-agent shutdown/TasksCancelled
                // push once the agent/notification layer exists
                let _inflight_signals: Vec<(Option<Uuid>, Uuid)> = tasks
                    .iter()
                    .filter(|t| matches!(t.state, TaskState::Running | TaskState::Finished))
                    .map(|t| (t.runner_uuid, t.uuid))
                    .collect();

                let cancelled_count = tasks.len() as u64;
                if !tasks.is_empty() {
                    let task_ids: Vec<i64> = tasks.iter().map(|t| t.id).collect();
                    let archived: Vec<ArchivedTasks::ActiveModel> = tasks
                        .into_iter()
                        .map(|task| ArchivedTasks::ActiveModel {
                            id: Set(task.id),
                            creator_id: Set(task.creator_id),
                            group_id: Set(task.group_id),
                            task_id: Set(task.task_id),
                            uuid: Set(task.uuid),
                            tags: Set(task.tags),
                            labels: Set(task.labels),
                            created_at: Set(task.created_at),
                            updated_at: Set(now),
                            state: Set(TaskState::Cancelled),
                            runner_uuid: Set(task.runner_uuid),
                            priority: Set(task.priority),
                            spec: Set(task.spec),
                            exec_options: Set(task.exec_options),
                            result: Set(Some(result.clone())),
                            upstream_task_uuid: Set(task.upstream_task_uuid),
                            downstream_task_uuid: Set(task.downstream_task_uuid),
                            task_suite_id: Set(task.task_suite_id),
                        })
                        .collect();

                    ActiveTasks::Entity::delete_many()
                        .filter(ActiveTasks::Column::Id.is_in(task_ids))
                        .exec(txn)
                        .await?;
                    ArchivedTasks::Entity::insert_many(archived)
                        .exec(txn)
                        .await?;
                }

                let updated = TaskSuites::Entity::update_many()
                    .col_expr(
                        TaskSuites::Column::State,
                        Expr::value(TaskSuiteState::Cancelled),
                    )
                    .col_expr(TaskSuites::Column::UpdatedAt, Expr::value(now))
                    .col_expr(TaskSuites::Column::CompletedAt, Expr::value(now))
                    .filter(TaskSuites::Column::Id.eq(suite.id))
                    .exec(txn)
                    .await?;

                if updated.rows_affected != 1 {
                    return Err(Error::ApiError(ApiError::InvalidRequest(
                        "Failed to update task suite state. Maybe due to concurrent state update"
                            .to_string(),
                    )));
                }

                Ok(cancelled_count)
            })
        })
        .await?;

    // TODO: notify assigned/executing agents that this suite was cancelled (and, for
    // Force, push per-agent TasksCancelled built from `_running_signals` above), then
    // drop the suite's in-memory task buffer in the dispatcher.

    Ok(())
}

/// Batch-apply agent-selection overrides for a single suite. The suite is resolved once
/// (requiring the caller's Write role on its group); each agent is then resolved and
/// applied independently. Per-agent failures are collected into the response rather than
/// aborting the batch; only genuine DB errors roll the whole transaction back.
pub async fn user_add_agents_to_suite(
    user_id: i64,
    pool: &InfraPool,
    suite_uuid: Uuid,
    req: SuiteAgentSelectionReq,
) -> Result<SuiteAgentSelectionResp> {
    let now = TimeDateTimeWithTimeZone::now_utc();

    let failed = pool
        .db
        .transaction::<_, HashMap<Uuid, SuiteAgentSelectionError>, Error>(|txn| {
            Box::pin(async move {
                // The suite is fixed for the whole batch, so a suite-level failure is request-level
                let suite = match authorize_suite(txn, user_id, suite_uuid).await {
                    Ok(suite) => suite,
                    Err(SelectionItemError::Item(SuiteAgentSelectionError::SuiteNotFound)) => {
                        return Err(Error::ApiError(ApiError::NotFound(format!(
                            "Task suite with uuid {suite_uuid} not found"
                        ))));
                    }
                    Err(SelectionItemError::Item(
                        SuiteAgentSelectionError::NoWriteAccessOnSuite,
                    )) => {
                        return Err(Error::AuthError(AuthError::PermissionDenied));
                    }
                    Err(SelectionItemError::Item(_)) => {
                        // The rest two failure should not happen here
                        return Err(Error::ApiError(ApiError::InternalServerError));
                    }
                    Err(SelectionItemError::Fatal(e)) => return Err(e),
                };

                let mut failed = HashMap::new();
                for (agent_uuid, action) in req.selection {
                    let outcome = async {
                        let agent = resolve_agent(txn, agent_uuid).await?;
                        apply_suite_agent_selection(txn, user_id, &suite, &agent, action, now).await
                    }
                    .await;
                    match outcome {
                        Ok(()) => {}
                        Err(SelectionItemError::Item(e)) => {
                            failed.insert(agent_uuid, e);
                        }
                        Err(SelectionItemError::Fatal(e)) => {
                            // Some db error occurred, roll back and error
                            return Err(e);
                        }
                    }
                }

                Ok(failed)
            })
        })
        .await?;

    // TODO: the manual overrides just changed the effective agent set. Once the
    // scheduler exists, trigger a recompute for this suite.

    Ok(SuiteAgentSelectionResp { failed })
}

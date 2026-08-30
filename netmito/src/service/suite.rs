use std::collections::HashMap;

use sea_orm::sea_query::extension::postgres::PgExpr;
use sea_orm::sea_query::{Alias, Func, PgFunc, Query};
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
    AgentNotification, CancelTaskSuiteOp, CountQuery, CreateTaskSuiteReq, CreateTaskSuiteResp,
    ExecHooks, HookTaskInfo, ParsedTaskSuiteInfo, StopAgentJobResp, StopJobOp,
    SuiteAgentOverrideReq, SuiteAgentOverrideResp, SuiteJobInfo, SuiteJobQueryResp,
    SuiteJobsQueryReq, SuiteJobsQueryResp, TaskResultMessage, TaskResultSpec, TaskSuiteInfo,
    TaskSuiteQueryResp, TaskSuitesQueryReq, TaskSuitesQueryResp, WorkerSchedulePlan,
};
use crate::service::task::{parse_operators_with_number, OperatorWithNumber};

#[derive(FromQueryResult)]
struct GroupIdResult {
    id: i64,
}

/// Sweep suites that have gone quiet out of `Open`.
///
/// A suite stays `Open` from its last submission until `timeout` has passed,
/// even after its tasks have all drained, which is what lets the agent running
/// it hold its job open and pick up a late arrival without provisioning again.
/// The states this settles it into:
///
/// | from | condition | to |
/// |---|---|---|
/// | `Open` | idle, work remains | `Closed` |
/// | `Open` | idle, nothing left | `Complete` |
/// | `Closed` | nothing left | `Complete` |
///
/// The last rule is a backstop: `decrement_incomplete_tasks` completes a `Closed`
/// suite as its last task commits, leaving this to catch the ones closed after
/// their work had already drained.
///
/// Idleness is measured from the last submission, falling back to creation, so a
/// suite that never received a task closes too rather than sitting `Open`
/// forever.
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
            // A `Closed` suite is already known to be idle; an `Open` one has to
            // have gone quiet for the whole window first.
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

/// Hand `count` finished tasks back to a suite: tick `incomplete_tasks` down,
/// and complete the suite if that empties an already-`Closed` one.
///
/// Every path that takes an active task out of a suite ends here. A path that
/// archives a suite task without calling this parks the
/// suite short of `Complete` and leaves agents cycling over work that is no
/// longer there.
///
/// The subtraction is deliberately unguarded. Every caller has just removed
/// exactly `count` rows from `active_tasks`, and each of those rows added one to
/// the counter when it was submitted, so the arithmetic cannot legitimately go
/// below zero. Clamping it would only turn a bug elsewhere into a suite that
/// looks fine; a negative counter is reported instead, loudly.
///
/// Draining an `Open` suite does not complete it: it stays `Open` until it has
/// also been quiet for `suite_auto_close_timeout`, which is what lets the agent
/// hold its job — and its provisioned environment — open for a late arrival.
/// [`sweep_inactive_suites`] settles it afterwards. A `Closed` suite has no such
/// window to wait out, so it is completed here.
///
/// The suite row must be taken before any task row, either by this call or by
/// the caller before it — see the lock-order note on [`crate::service`].
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

    // Nothing here can repair it, and swallowing it would leave a suite that
    // never completes and never gets claimed, with no trace of why.
    for suite in updated.iter().filter(|s| s.incomplete_tasks < 0) {
        tracing::error!(
            suite_id = suite.id,
            incomplete_tasks = suite.incomplete_tasks,
            decremented = count,
            "A suite's incomplete_tasks went negative: more tasks were taken off it than were ever counted"
        );
    }

    // Conditional on the state and the count, which is what makes a second
    // statement safe without a transaction around the pair: a submission racing
    // it either lands first and leaves a non-zero count, or lands after and sets
    // `Open` itself.
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
            worker_count: _worker_count,
            prefetch: _prefetch,
            ..
        } => {
            // TODO: validate the request
        }
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

/// Get a single suite's details, including the uuids of agents allowed to execute this suite.
/// The caller must have at least `Read` in the suite's group.
///
/// Currently we only report agents that are UserIncluded to the suite. In the future we
/// shall read the in-memory list for actual tag matching results.
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

    // The effective set: `(tag-matched ∪ UserIncluded) − UserExcluded`, gated by
    // the suite group's access to each agent. See `service::agent::matching`.
    let eligible_agents =
        crate::service::agent::matching::eligible_agent_uuids(&pool.db, suite.id).await?;

    // The jobs holding the suite right now — the same shape `user_query_suite_jobs`
    // lists, narrowed to the non-terminal states so this answers "who is running
    // it", not "who ever ran it".
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
        // Left join: `agent_id` is nullable to leave room for a future detach.
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
                .is_in(crate::service::agent::job::IN_FLIGHT),
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

                // `authorize_suite` takes no lock, so this only names the state
                // in the error; the `WHERE` below is what actually rejects a
                // second close.
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

/// Cancel a suite (`* → Cancelled`, terminal). Archives every non-terminal task
/// of the suite as `Cancelled`. Requires Write/Admin in the suite's group.
///
/// `op` decides what happens to agents mid-run:
/// - `Graceful` — in-flight jobs keep their state and run their cleanup hook;
///   each agent is told the suite was cancelled and drives its own job to
///   `Completed`. No task the agent is holding is archived out from under it
///   without notice: it also gets the list of task uuids that were cancelled.
/// - `Force` — in-flight jobs are written `Killed` on the spot (no cleanup) and
///   their agents are told to stop.
pub async fn user_cancel_task_suite(
    user_id: i64,
    pool: &InfraPool,
    suite_uuid: Uuid,
    op: CancelTaskSuiteOp,
) -> Result<()> {
    let now = TimeDateTimeWithTimeZone::now_utc();

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

                // The suite is cancelled *before* its tasks are archived, not
                // after. This statement locks the suite row, and every submit
                // bumps that same row under a `state <> Cancelled` predicate — so
                // once this commits, a concurrent submit either got in ahead of
                // the delete below or is rejected outright. Write the suite last
                // instead and a submit landing between the delete and the write
                // leaves a live task in a cancelled suite that nothing will ever
                // claim, with `incomplete_tasks` already zeroed.
                //
                // Zero is exact for the same reason: the delete below takes every
                // active row of the suite, and nothing can add another.
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

                // Archive every task of the suite as Cancelled. `Cancelled` rows
                // are included deliberately: that state means an agent reported
                // `Cancel` and has not yet sent the `Commit` that would archive
                // the task. Leaving them behind stalls them in `active_tasks`
                // forever — the `Commit` is about to be refused by a terminal
                // job, no other agent can claim a non-`Ready` task, and nothing
                // reclaims that state. Taking them here is what lets the counter
                // below be exact.
                let tasks = ActiveTasks::Entity::delete_many()
                    .filter(ActiveTasks::Column::TaskSuiteId.eq(suite.id))
                    .filter(ActiveTasks::Column::State.is_not_in([TaskState::Unknown]))
                    .exec_with_returning(txn)
                    .await?;

                // The in-flight subset is the only one an agent is holding right
                // now. Those agents are told which task uuids vanished, so they
                // stop rather than reporting against rows we just archived.
                let cancelled_task_uuids: Vec<Uuid> = tasks
                    .iter()
                    .filter(|t| {
                        matches!(
                            t.state,
                            TaskState::Running | TaskState::Finished | TaskState::Cancelled
                        )
                    })
                    .map(|t| t.uuid)
                    .collect();

                let suite_id = suite.id;
                if !tasks.is_empty() {
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

                    ArchivedTasks::Entity::insert_many(archived)
                        .exec(txn)
                        .await?;
                }

                Ok((suite_id, cancelled_task_uuids))
            })
        })
        .await?;

    // Tear down whatever is still running. `Force` writes the terminal itself so
    // no further agent report is accepted; `Graceful` leaves the job alone and
    // lets the agent walk it through cleanup to `Completed`.
    let running_agents = match op {
        CancelTaskSuiteOp::Force => {
            let killed =
                crate::service::agent::job::kill_suite_jobs(&pool.db, suite_id, now).await?;
            // Every job of the suite is over at once, and the suite itself has
            // nothing left to hand out.
            pool.suite_queues.close_suite(suite_id);
            killed
        }
        CancelTaskSuiteOp::Graceful => {
            crate::service::agent::job::agents_running_suite(&pool.db, suite_id).await?
        }
    };

    crate::service::agent::notify_agents_by_id(
        pool,
        &running_agents,
        AgentNotification::SuiteCancelled {
            suite_uuid,
            reason: "Suite was cancelled by a user".to_string(),
        },
    )
    .await;
    if !cancelled_task_uuids.is_empty() {
        crate::service::agent::notify_agents_by_id(
            pool,
            &running_agents,
            AgentNotification::TasksCancelled {
                task_uuids: cancelled_task_uuids,
            },
        )
        .await;
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
        .transaction::<_, (Option<i64>, HashMap<Uuid, crate::error::ErrorMsg>), Error>(|txn| {
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
                let suite_id = suite.id;

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

                Ok((Some(suite_id), errors))
            })
        })
        .await?;

    // The effective agent set just changed. Notify the agents
    if let Some(suite_id) = suite_id {
        crate::service::agent::notify_suite_available(pool, suite_id).await;
    }

    Ok(SuiteAgentOverrideResp { errors })
}

/// Query a suite's jobs, newest first.
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
        // Left join: `agent_id` is nullable to leave room for a future detach.
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

/// One job of a suite, with its hook executions embedded. A hook's artifacts
/// are downloaded through the existing artifact endpoints using its `uuid`.
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

/// Stop one of the suite's jobs, addressed by its per-suite job number.
///
/// Same behaviour as the agent-scoped route, reached through the suite's owner
/// (Write on its group) instead of the agent's admin: the agent stays up and
/// picks a suite again, which may well be this one.
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

    // A job that already ended, or one no agent owns, has nothing to stop.
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

    crate::service::agent::stop_agent_job(pool, &agent, &job, suite_uuid, op).await
}

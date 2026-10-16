use std::collections::HashMap;

use sea_orm::sea_query::extension::postgres::PgExpr;
use sea_orm::sea_query::{Alias, PgFunc, Query};
use sea_orm::{prelude::*, ConnectionTrait, FromQueryResult, Set, TransactionTrait};
use uuid::Uuid;

use super::suite_agent::{apply_suite_agent_override, authorize_suite, resolve_agent};
use crate::config::InfraPool;
use crate::entity::role::UserGroupRole;
use crate::entity::state::{TaskState, TaskSuiteState};
use crate::entity::task_suite_agent::SuiteAgentOverrideType;
use crate::entity::{
    active_tasks as ActiveTasks, agents as Agent, archived_tasks as ArchivedTasks, groups as Group,
    task_suite_agent as TaskSuiteAgent, task_suites as TaskSuites, user_group as UserGroup,
    users as User,
};
use crate::error::{ApiError, Error, ResolveError, Result};
use crate::schema::{
    CancelTaskSuiteOp, ChangeTaskSuiteReq, CountQuery, CreateTaskSuiteReq, CreateTaskSuiteResp,
    ExecHooks, ParsedTaskSuiteInfo, SuiteAgentOverrideReq, SuiteAgentOverrideResp,
    TaskResultMessage, TaskResultSpec, TaskSuiteInfo, TaskSuiteQueryResp, TaskSuitesQueryReq,
    TaskSuitesQueryResp, UpdateOp, WorkerSchedulePlan,
};
use crate::service::task::{check_exec_spec, parse_operators_with_number, OperatorWithNumber};

#[derive(FromQueryResult)]
struct GroupIdResult {
    id: i64,
}

/// The largest worker count a plan may ask an agent for.
const MAX_WORKER_COUNT: u32 = 256;

/// Reject a plan no agent should be handed. Shared with the change path so an
/// edit cannot reach a state creation refuses.
// TODO: this should finally be adjusted to some more flexible definitions
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

/// Hooks run through the same executor as tasks, so their specs face the same check.
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

/// Change a suite's properties. Requires Write in the suite's group.
///
/// A `Cancelled` suite is frozen — effectively archived, so even a rename is
/// rejected. Every other state is editable.
///
/// The change reaches the *next* job. Slot counts and hooks are snapshotted when
/// a job starts, and an agent that stops tag-matching continue to run the job.
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
    // Only the incoming specs need checking: the hooks left alone were checked
    // when they were written.
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
    let _needs_notify = req.affects_scheduling();
    let now = TimeDateTimeWithTimeZone::now_utc();

    let _suite_id = pool
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

                // `authorize_suite` takes no lock, so this only names the state
                // in the error; the `WHERE` below is what rejects a suite
                // cancelled in the meantime.
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
                // The hooks are a patch onto what the suite already carries,
                // and merging them in Rust would mean reading the row first —
                // two statements, no lock between them, and two concurrent
                // patches silently losing one edit. The column is written as an
                // expression over its own current value instead, so the merge
                // happens inside the `UPDATE` and there is nothing to read.
                let hooks_expr = match req.exec_hooks {
                    Some(update) => {
                        // A NULL column merges as `{}`, so a first hook lands on
                        // a suite that has none.
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
                                // `||` replaces this one key and leaves the rest.
                                // Each step parenthesises itself: `-` binds
                                // tighter than `||`, so an unwrapped chain would
                                // read as `x || (obj - 'cleanup')`.
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
                        // Every hook gone is no hooks at all — the shape `create`
                        // stores, rather than an object of three nulls.
                        Some(Expr::cust_with_expr("nullif($1, '{}'::jsonb)", hooks))
                    }
                    None => None,
                };

                // `exec_hooks` is not part of `change`, so it is assigned here
                // rather than twice: two assignments to one column is an error.
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

    // Eligibility and the pick order are read from the row on every query, so a
    // change leaves nothing to rebuild — the agents it lets in just have to hear
    // that the suite is there.
    // TODO: once agents can be notified, `if _needs_notify` call
    // `notify_suite_available(pool, _suite_id)` here.
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

#[derive(FromQueryResult)]
struct AgentUuidResult {
    uuid: Uuid,
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
            Expr::col((TaskSuiteAgent::Entity, TaskSuiteAgent::Column::OverrideType))
                .eq(SuiteAgentOverrideType::UserIncluded),
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

                if suite.state != TaskSuiteState::Open {
                    return Err(Error::ApiError(ApiError::InvalidRequest(format!(
                        "Cannot transition from {} to Closed",
                        suite.state
                    ))));
                }

                let mut suite: TaskSuites::ActiveModel = suite.into();
                suite.state = Set(TaskSuiteState::Closed);
                suite.updated_at = Set(now);
                suite.update(txn).await?;
                Ok(())
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

                // All cancelled tasks share this result.
                let result = serde_json::to_value(TaskResultSpec {
                    exit_status: 0,
                    msg: Some(TaskResultMessage::UserCancellation),
                })
                .inspect_err(|e| tracing::error!("{}", e))?;

                // Archive every non-terminal task of the suite as Cancelled:
                let tasks = ActiveTasks::Entity::delete_many()
                    .filter(ActiveTasks::Column::TaskSuiteId.eq(suite.id))
                    .filter(
                        ActiveTasks::Column::State
                            .is_not_in([TaskState::Cancelled, TaskState::Unknown]),
                    )
                    .exec_with_returning(txn)
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

                let mut suite: TaskSuites::ActiveModel = suite.into();
                suite.state = Set(TaskSuiteState::Cancelled);
                suite.updated_at = Set(now);
                suite.completed_at = Set(Some(now));
                suite.update(txn).await?;

                Ok(cancelled_count)
            })
        })
        .await?;

    // TODO: notify assigned/executing agents that this suite was cancelled (and, for
    // Force, push per-agent TasksCancelled built from `_running_signals` above), then
    // drop the suite's in-memory task buffer in the dispatcher.

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

    let errors = pool
        .db
        .transaction::<_, HashMap<Uuid, crate::error::ErrorMsg>, Error>(|txn| {
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

                Ok(errors)
            })
        })
        .await?;

    // TODO: the manual overrides just changed the effective agent set. Once the
    // scheduler exists, trigger a recompute for this suite.

    Ok(SuiteAgentOverrideResp { errors })
}

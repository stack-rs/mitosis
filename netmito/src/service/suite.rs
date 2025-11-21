//! Task Suite service for managing suite lifecycles and counters

use sea_orm::sea_query::{extension::postgres::PgExpr, Query};
use sea_orm::sea_query::{Alias, PgFunc};
use sea_orm::{prelude::*, QuerySelect};
use sea_orm::{FromQueryResult, Set, TransactionTrait};
use uuid::Uuid;

use crate::config::InfraPool;
use crate::entity::{
    active_tasks as ActiveTasks, archived_tasks as ArchivedTasks, groups as Group,
    node_managers as NodeManager,
    role::UserGroupRole,
    state::{TaskState, TaskSuiteState},
    task_suite_node_manager as TaskSuiteNodeManager,
    task_suites::{self as TaskSuites},
    user_group as UserGroup, users as User,
};
use crate::error::{ApiError, Error, Result};
use crate::schema::{
    CancelSuiteResp, CancelTaskSuiteOp, CountQuery, CreateTaskSuiteReq, CreateTaskSuiteResp,
    EnvHookSpec, ParsedTaskSuiteInfo, TaskResultSpec, TaskSuiteInfo, TaskSuiteQueryResp,
    TaskSuitesQueryReq, TaskSuitesQueryResp, WorkerSchedulePlan,
};
use crate::service::task::parse_operators_with_number;

/// Decrements pending_tasks when a task completes or is cancelled.
/// If pending_tasks reaches 0, transitions the suite to Complete state.
///
/// This function should be called within the same transaction as task archival.
pub async fn decrement_suite_task_counter<C>(
    db: &C,
    task_suite_id: i64,
    now: TimeDateTimeWithTimeZone,
) -> Result<()>
where
    C: ConnectionTrait,
{
    // First, decrement the pending_tasks counter
    // We only decrement if pending_tasks > 0 to prevent negative values
    let updated = TaskSuites::Entity::update_many()
        .col_expr(
            TaskSuites::Column::PendingTasks,
            Expr::col(TaskSuites::Column::PendingTasks).sub(1),
        )
        .col_expr(TaskSuites::Column::UpdatedAt, Expr::value(now))
        .filter(TaskSuites::Column::Id.eq(task_suite_id))
        .filter(TaskSuites::Column::PendingTasks.gt(0))
        .exec_with_returning(db)
        .await?;

    // Check if this was the last pending task and transition to Complete if needed
    if let Some(suite) = updated.into_iter().next() {
        if suite.pending_tasks == 0 && !suite.state.is_terminal() {
            // Passively transition to Complete state
            TaskSuites::Entity::update_many()
                .col_expr(
                    TaskSuites::Column::State,
                    Expr::value(TaskSuiteState::Complete),
                )
                .col_expr(TaskSuites::Column::CompletedAt, Expr::value(now))
                .col_expr(TaskSuites::Column::UpdatedAt, Expr::value(now))
                .filter(TaskSuites::Column::Id.eq(task_suite_id))
                .filter(TaskSuites::Column::PendingTasks.eq(0))
                .exec(db)
                .await?;

            tracing::debug!(
                task_suite_id = task_suite_id,
                "Task suite transitioned to Complete (all tasks finished)"
            );
        }
    }

    Ok(())
}

/// Manually closes a task suite, preventing new tasks from being added.
/// This transitions the suite from Open to Closed state.
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
        return Err(Error::ApiError(crate::error::ApiError::NotFound(
            "Task suite not found or already closed".to_string(),
        )));
    }
    Ok(())
}

/// Manually cancels a task suite, marking it as Cancelled.
/// This will not affect already-running tasks.
pub async fn cancel_task_suite(
    db: &DatabaseConnection,
    task_suite_id: i64,
) -> Result<TaskSuites::Model> {
    let now = TimeDateTimeWithTimeZone::now_utc();

    let updated = TaskSuites::Entity::update_many()
        .col_expr(
            TaskSuites::Column::State,
            Expr::value(TaskSuiteState::Cancelled),
        )
        .col_expr(TaskSuites::Column::UpdatedAt, Expr::value(now))
        .col_expr(TaskSuites::Column::CompletedAt, Expr::value(now))
        .filter(TaskSuites::Column::Id.eq(task_suite_id))
        .filter(TaskSuites::Column::State.ne(TaskSuiteState::Cancelled))
        .exec_with_returning(db)
        .await?;

    updated.into_iter().next().ok_or_else(|| {
        Error::ApiError(crate::error::ApiError::NotFound(
            "Task suite not found or already in terminal state".to_string(),
        ))
    })
}

/// Creates a new task suite in the Open state.
/// Validates group membership and worker configuration before creating the suite.
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
        env_preparation,
        env_cleanup,
    }: CreateTaskSuiteReq,
) -> Result<CreateTaskSuiteResp> {
    // Validate worker configuration based on the policy variant
    match &worker_schedule {
        // TODO: this should finnaly be adjusted
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

    // Convert HashSet to Vec
    let tags = Vec::from_iter(tags);
    let labels = Vec::from_iter(labels);

    let now = TimeDateTimeWithTimeZone::now_utc();

    if group_name.is_empty() {
        return Err(Error::ApiError(ApiError::InvalidRequest(
            "group_name is required".to_string(),
        )));
    }
    // Resolve group ID
    let group = Group::Entity::find()
        .filter(Group::Column::GroupName.eq(&group_name))
        .one(&pool.db)
        .await?
        .ok_or(Error::ApiError(ApiError::NotFound(format!(
            "Group with name {group_name}"
        ))))?;

    // Permission check: user must be member of the group
    UserGroup::Entity::find()
        .filter(UserGroup::Column::UserId.eq(user_id))
        .filter(UserGroup::Column::GroupId.eq(group.id))
        .one(&pool.db)
        .await?
        .and_then(|user_group_role| {
            // Check user has at least Write permission
            match user_group_role.role {
                UserGroupRole::Write | UserGroupRole::Admin => Some(()),
                _ => None,
            }
        })
        .ok_or(Error::AuthError(crate::error::AuthError::PermissionDenied))?;

    // Serialize JSON fields
    let worker_schedule_json = serde_json::to_value(&worker_schedule)?;
    let env_preparation_json = env_preparation
        .map(|h| serde_json::to_value(&h))
        .transpose()?;
    let env_cleanup_json = env_cleanup.map(|h| serde_json::to_value(&h)).transpose()?;

    // Generate UUID
    let suite_uuid = Uuid::new_v4();

    // Insert suite record
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
        env_preparation: Set(env_preparation_json),
        env_cleanup: Set(env_cleanup_json),
        state: Set(TaskSuiteState::Open),
        total_tasks: Set(0),
        pending_tasks: Set(0),
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

pub(crate) async fn check_task_suites_query(
    user_id: i64,
    pool: &InfraPool,
    query: &mut TaskSuitesQueryReq,
) -> Result<()> {
    if let Some(ref name) = query.tags {
        if name.is_empty() {
            return Err(Error::ApiError(crate::error::ApiError::InvalidRequest(
                "Name cannot be empty if specified".to_string(),
            )));
        }
    }
    if let Some(ref description) = query.tags {
        if description.is_empty() {
            return Err(Error::ApiError(crate::error::ApiError::InvalidRequest(
                "Description cannot be empty if specified".to_string(),
            )));
        }
    }
    if let Some(ref tags) = query.tags {
        if tags.is_empty() {
            return Err(Error::ApiError(crate::error::ApiError::InvalidRequest(
                "Tags cannot be empty if specified".to_string(),
            )));
        }
    }
    if let Some(ref labels) = query.labels {
        if labels.is_empty() {
            return Err(Error::ApiError(crate::error::ApiError::InvalidRequest(
                "Labels cannot be empty if specified".to_string(),
            )));
        }
    }
    if let Some(ref creator_usernames) = query.creator_usernames {
        if creator_usernames.is_empty() {
            return Err(Error::ApiError(crate::error::ApiError::InvalidRequest(
                "Creator username cannot be empty if specified".to_string(),
            )));
        }
    }
    if let Some(ref states) = query.states {
        if states.is_empty() {
            return Err(Error::ApiError(crate::error::ApiError::InvalidRequest(
                "State cannot be empty if specified".to_string(),
            )));
        }
    }
    if query.group_name.is_none() {
        let username = User::Entity::find()
            .filter(User::Column::Id.eq(user_id))
            .one(&pool.db)
            .await?
            .ok_or(Error::ApiError(crate::error::ApiError::NotFound(
                "User".to_string(),
            )))?
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
                return Err(Error::AuthError(crate::error::AuthError::PermissionDenied));
            }
            None => {
                return Err(Error::ApiError(crate::error::ApiError::InvalidRequest(
                    format!("Group with name {group_name} not found or user is not in the group"),
                )));
            }
        }
    }
    Ok(())
}

/// Helper function to apply suite query filters
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
        let op = parse_operators_with_number(priority)?;
        use crate::service::task::OperatorWithNumber;
        match op {
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

/// Query suites with filters and pagination.
/// Only returns suites from groups the user is a member of.
pub async fn user_query_task_suites(
    user_id: i64,
    pool: &InfraPool,
    mut query: TaskSuitesQueryReq,
) -> Result<TaskSuitesQueryResp> {
    check_task_suites_query(user_id, pool, &mut query).await?;

    let group_name = query.group_name.clone().unwrap();

    // Build query statement using raw SQL builder
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
            (TaskSuites::Entity, TaskSuites::Column::EnvPreparation),
            (TaskSuites::Entity, TaskSuites::Column::EnvCleanup),
            (TaskSuites::Entity, TaskSuites::Column::State),
            (TaskSuites::Entity, TaskSuites::Column::LastTaskSubmittedAt),
            (TaskSuites::Entity, TaskSuites::Column::TotalTasks),
            (TaskSuites::Entity, TaskSuites::Column::PendingTasks),
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

    // Apply filters using the shared helper function
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
        let suite_results = TaskSuiteInfo::find_by_statement(builder.build(&stmt))
            .all(&pool.db)
            .await?;
        Ok(TaskSuitesQueryResp {
            count: suite_results.len() as u64,
            suites: suite_results,
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
    env_preparation: Option<serde_json::Value>,
    env_cleanup: Option<serde_json::Value>,
    state: TaskSuiteState,
    last_task_submitted_at: Option<TimeDateTimeWithTimeZone>,
    total_tasks: i32,
    pending_tasks: i32,
    created_at: TimeDateTimeWithTimeZone,
    updated_at: TimeDateTimeWithTimeZone,
    completed_at: Option<TimeDateTimeWithTimeZone>,
}

#[derive(FromQueryResult)]
struct ManagerUuidResult {
    uuid: Uuid,
}

/// Get detailed information about a specific task suite by UUID.
/// User must be a member of the suite's group.
pub async fn user_get_task_suite_by_uuid(
    pool: &InfraPool,
    suite_uuid: Uuid,
) -> Result<TaskSuiteQueryResp> {
    // Build a single query to fetch suite with group, creator info, and permission check in one go
    let builder = pool.db.get_database_backend();

    // Single query that joins suite, user, group, and user_group for permission check
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
            (TaskSuites::Entity, TaskSuites::Column::EnvPreparation),
            (TaskSuites::Entity, TaskSuites::Column::EnvCleanup),
            (TaskSuites::Entity, TaskSuites::Column::State),
            (TaskSuites::Entity, TaskSuites::Column::LastTaskSubmittedAt),
            (TaskSuites::Entity, TaskSuites::Column::TotalTasks),
            (TaskSuites::Entity, TaskSuites::Column::PendingTasks),
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
            "Task suite with uuid {suite_uuid} or user does not have permission"
        ))))?;

    // Fetch assigned managers in a separate query
    let manager_stmt = Query::select()
        .column((NodeManager::Entity, NodeManager::Column::Uuid))
        .from(NodeManager::Entity)
        .join(
            sea_orm::JoinType::Join,
            TaskSuiteNodeManager::Entity,
            Expr::col((
                TaskSuiteNodeManager::Entity,
                TaskSuiteNodeManager::Column::ManagerId,
            ))
            .eq(Expr::col((NodeManager::Entity, NodeManager::Column::Id))),
        )
        .and_where(
            Expr::col((
                TaskSuiteNodeManager::Entity,
                TaskSuiteNodeManager::Column::TaskSuiteId,
            ))
            .eq(suite.id),
        )
        .to_owned();

    let manager_uuids = ManagerUuidResult::find_by_statement(builder.build(&manager_stmt))
        .all(&pool.db)
        .await?
        .into_iter()
        .map(|m| m.uuid)
        .collect();

    // Parse JSON fields
    let worker_schedule: WorkerSchedulePlan = serde_json::from_value(suite.worker_schedule)?;
    let env_preparation: Option<EnvHookSpec> = suite
        .env_preparation
        .map(serde_json::from_value)
        .transpose()?;
    let env_cleanup: Option<EnvHookSpec> =
        suite.env_cleanup.map(serde_json::from_value).transpose()?;

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
            env_preparation,
            env_cleanup,
            state: suite.state,
            last_task_submitted_at: suite.last_task_submitted_at,
            total_tasks: suite.total_tasks,
            pending_tasks: suite.pending_tasks,
            created_at: suite.created_at,
            updated_at: suite.updated_at,
            completed_at: suite.completed_at,
        },
        assigned_managers: manager_uuids,
    })
}

/// Close a task suite (high-level endpoint).
/// Transitions the suite from Open to Closed state.
/// User must have Write permission in the suite's group.
pub async fn user_close_task_suite(user_id: i64, pool: &InfraPool, suite_uuid: Uuid) -> Result<()> {
    let now = TimeDateTimeWithTimeZone::now_utc();

    // Fetch and validate suite, group membership, and permission in one transaction
    pool.db
        .transaction::<_, (), Error>(|txn| {
            Box::pin(async move {
                // Fetch suite
                let suite = TaskSuites::Entity::find()
                    .filter(TaskSuites::Column::Uuid.eq(suite_uuid))
                    .one(txn)
                    .await?
                    .ok_or(Error::ApiError(ApiError::NotFound(format!(
                        "Task suite with uuid {suite_uuid}"
                    ))))?;

                // Check permission
                UserGroup::Entity::find()
                    .filter(UserGroup::Column::UserId.eq(user_id))
                    .filter(UserGroup::Column::GroupId.eq(suite.group_id))
                    .one(txn)
                    .await?
                    .and_then(|user_group_role| match user_group_role.role {
                        UserGroupRole::Write | UserGroupRole::Admin => Some(()),
                        _ => None,
                    })
                    .ok_or(Error::AuthError(crate::error::AuthError::PermissionDenied))?;

                // Validate current state allows transition
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

/// Cancel a task suite, optionally cancelling all pending/ready tasks.
/// User must have Write permission in the suite's group.
pub async fn user_cancel_task_suite(
    user_id: i64,
    pool: &InfraPool,
    suite_uuid: Uuid,
    _op: CancelTaskSuiteOp,
) -> Result<CancelSuiteResp> {
    let now = TimeDateTimeWithTimeZone::now_utc();

    // Cancel suite and tasks in a single transaction
    let cancelled_count = pool
        .db
        .transaction::<_, u64, Error>(|txn| {
            Box::pin(async move {
                // Fetch suite
                let suite = TaskSuites::Entity::find()
                    .filter(TaskSuites::Column::Uuid.eq(suite_uuid))
                    .one(txn)
                    .await?
                    .ok_or(Error::ApiError(ApiError::NotFound(format!(
                        "Task suite with uuid {suite_uuid}"
                    ))))?;

                // Check permission
                UserGroup::Entity::find()
                    .filter(UserGroup::Column::UserId.eq(user_id))
                    .filter(UserGroup::Column::GroupId.eq(suite.group_id))
                    .one(txn)
                    .await?
                    .and_then(|user_group_role| match user_group_role.role {
                        UserGroupRole::Write | UserGroupRole::Admin => Some(()),
                        _ => None,
                    })
                    .ok_or(Error::AuthError(crate::error::AuthError::PermissionDenied))?;

                // Check suite isn't already cancelled
                if matches!(suite.state, TaskSuiteState::Cancelled) {
                    return Err(Error::ApiError(ApiError::InvalidRequest(format!(
                        "Suite {suite_uuid} is already in cancelled state",
                    ))));
                }

                // TODO: check if correctly inform the manager to cancel the task suite
                // TODO: currently we do not use 'force' to cancel running tasks
                // Cancel tasks if requested
                let mut cancelled_count = 0u64;
                // Find all Ready/Pending tasks for this suite
                let mut tasks_stmt = ActiveTasks::Entity::find()
                    .filter(ActiveTasks::Column::TaskSuiteId.eq(suite.id))
                    .to_owned();
                QuerySelect::query(&mut tasks_stmt).and_where(
                    Expr::col(ActiveTasks::Column::State)
                        .eq(PgFunc::any(vec![TaskState::Ready, TaskState::Pending])),
                );

                let tasks = tasks_stmt.all(txn).await?;

                if !tasks.is_empty() {
                    let task_ids: Vec<i64> = tasks.iter().map(|t| t.id).collect();
                    cancelled_count = tasks.len() as u64;

                    // Create cancellation result
                    let result = serde_json::to_value(TaskResultSpec {
                        exit_status: 0,
                        msg: Some(crate::schema::TaskResultMessage::UserCancellation),
                    })
                    .inspect_err(|e| tracing::error!("{}", e))?;

                    // Prepare archived tasks
                    let archived_tasks: Vec<ArchivedTasks::ActiveModel> = tasks
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
                            assigned_worker: Set(task.assigned_worker),
                            timeout: Set(task.timeout),
                            priority: Set(task.priority),
                            spec: Set(task.spec),
                            result: Set(Some(result.clone())),
                            upstream_task_uuid: Set(task.upstream_task_uuid),
                            downstream_task_uuid: Set(task.downstream_task_uuid),
                            reporter_uuid: Set(None),
                            task_suite_id: Set(task.task_suite_id),
                        })
                        .collect();

                    // Delete from active_tasks
                    ActiveTasks::Entity::delete_many()
                        .filter(ActiveTasks::Column::Id.is_in(task_ids.clone()))
                        .exec(txn)
                        .await?;

                    // Insert into archived_tasks
                    ArchivedTasks::Entity::insert_many(archived_tasks)
                        .exec(txn)
                        .await?;

                    // Note: We don't remove from task queues in transaction
                    // Will handle after commit
                }

                // Update suite state to Cancelled
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

    // TODO: Remove cancelled tasks from worker queues
    // This would require access to the worker_task_queue_tx channel
    // For now, tasks will be removed lazily when workers try to fetch them

    Ok(CancelSuiteResp {
        cancelled_task_count: cancelled_count,
    })
}

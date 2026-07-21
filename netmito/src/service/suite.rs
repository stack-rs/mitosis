use sea_orm::{prelude::*, Set};
use uuid::Uuid;

use crate::config::InfraPool;
use crate::entity::role::UserGroupRole;
use crate::entity::state::TaskSuiteState;
use crate::entity::{groups as Group, task_suites as TaskSuites, user_group as UserGroup};
use crate::error::{ApiError, AuthError, Error};
use crate::schema::{CreateTaskSuiteReq, CreateTaskSuiteResp, WorkerSchedulePlan};

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
        .and_then(|ug| match ug.role {
            UserGroupRole::Write | UserGroupRole::Admin => Some(()),
            _ => None,
        })
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

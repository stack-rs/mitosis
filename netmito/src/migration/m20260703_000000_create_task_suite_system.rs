//! Consolidated migration for the task-suite entity system (main → new design).
//!
//! Creates, in FK-dependency order: `task_suites`, `agents`, `machines`
//! (owned by its agent via `agent_id`, RESTRICT), the `group_agent` /
//! `task_suite_agent` join tables, `suite_agent_jobs`, and `hook_tasks`; then
//! links tasks to suites by adding `task_suite_id` to `active_tasks` /
//! `archived_tasks`.
//!
//! This intentionally does NOT include the separate exec-spec refactor
//! (timeout→spec, assigned_worker→runner_uuid, exec_options); tasks keep their
//! current shape aside from the new `task_suite_id` column.
//!
//! See docs/plans/2026-07-03-suite-entity-design.md.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // ── task_suites ─────────────────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(TaskSuites::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(TaskSuites::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(TaskSuites::Uuid)
                            .uuid()
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(TaskSuites::Name).text())
                    .col(ColumnDef::new(TaskSuites::Description).text())
                    .col(ColumnDef::new(TaskSuites::GroupId).big_integer().not_null())
                    .col(
                        ColumnDef::new(TaskSuites::CreatorId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TaskSuites::Tags)
                            .array(ColumnType::Text)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TaskSuites::Labels)
                            .array(ColumnType::Text)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TaskSuites::Priority)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(TaskSuites::WorkerSchedule)
                            .json_binary()
                            .not_null(),
                    )
                    .col(ColumnDef::new(TaskSuites::ExecHooks).json_binary())
                    .col(
                        ColumnDef::new(TaskSuites::State)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(ColumnDef::new(TaskSuites::LastTaskSubmittedAt).timestamp_with_time_zone())
                    .col(
                        ColumnDef::new(TaskSuites::TotalTasks)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(TaskSuites::IncompleteTasks)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(TaskSuites::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(TaskSuites::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(ColumnDef::new(TaskSuites::CompletedAt).timestamp_with_time_zone())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-task_suites-group")
                            .from(TaskSuites::Table, TaskSuites::GroupId)
                            .to(Groups::Table, Groups::Id)
                            .on_delete(ForeignKeyAction::Restrict)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-task_suites-creator")
                            .from(TaskSuites::Table, TaskSuites::CreatorId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Restrict)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        for (name, col) in [
            ("idx_task_suites-group_id", TaskSuites::GroupId),
            ("idx_task_suites-creator_id", TaskSuites::CreatorId),
            ("idx_task_suites-state", TaskSuites::State),
        ] {
            manager
                .create_index(
                    Index::create()
                        .name(name)
                        .table(TaskSuites::Table)
                        .col(col)
                        .to_owned(),
                )
                .await?;
        }
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_tasks_suites-tags_gin")
                    .table(TaskSuites::Table)
                    .col(TaskSuites::Tags)
                    .full_text()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_tasks_suites-labels_gin")
                    .table(TaskSuites::Table)
                    .col(TaskSuites::Labels)
                    .full_text()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_task_suites-auto_close")
                    .table(TaskSuites::Table)
                    .col(TaskSuites::LastTaskSubmittedAt)
                    .and_where(Expr::col(TaskSuites::State).eq(0))
                    .to_owned(),
            )
            .await?;

        // ── agents (durable; no machine_id column — see machines) ────────
        manager
            .create_table(
                Table::create()
                    .table(Agents::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Agents::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Agents::Uuid).uuid().not_null().unique_key())
                    .col(ColumnDef::new(Agents::CreatorId).big_integer().not_null())
                    .col(
                        ColumnDef::new(Agents::Tags)
                            .array(ColumnType::Text)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Agents::Labels)
                            .array(ColumnType::Text)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Agents::State)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(Agents::LastHeartbeat)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(ColumnDef::new(Agents::AssignedTaskSuiteId).big_integer())
                    .col(ColumnDef::new(Agents::Metadata).json_binary())
                    .col(
                        ColumnDef::new(Agents::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Agents::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-agents-creator_id")
                            .from(Agents::Table, Agents::CreatorId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Restrict)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-agents-assigned_task_suite_id")
                            .from(Agents::Table, Agents::AssignedTaskSuiteId)
                            .to(TaskSuites::Table, TaskSuites::Id)
                            .on_delete(ForeignKeyAction::SetNull)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        for (name, col) in [
            ("idx_agents-creator_id", Agents::CreatorId),
            ("idx_agents-state", Agents::State),
            ("idx_agents-heartbeat", Agents::LastHeartbeat),
        ] {
            manager
                .create_index(
                    Index::create()
                        .name(name)
                        .table(Agents::Table)
                        .col(col)
                        .to_owned(),
                )
                .await?;
        }
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_agents-tags_gin")
                    .table(Agents::Table)
                    .col(Agents::Tags)
                    .full_text()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_agents-labels_gin")
                    .table(Agents::Table)
                    .col(Agents::Labels)
                    .full_text()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_agents-assigned_task_suite")
                    .table(Agents::Table)
                    .col(Agents::AssignedTaskSuiteId)
                    .and_where(Expr::col(Agents::AssignedTaskSuiteId).is_not_null())
                    .to_owned(),
            )
            .await?;

        // ── machines (appendix of an agent; owns the FK back to agents) ──
        manager
            .create_table(
                Table::create()
                    .table(Machines::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Machines::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    // 1:1 owning agent; RESTRICT blocks deleting the agent while
                    // this machine row exists (machine must be deleted first).
                    .col(
                        ColumnDef::new(Machines::AgentId)
                            .big_integer()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(Machines::MachineCode)
                            .text()
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(Machines::Metadata).json_binary())
                    .col(
                        ColumnDef::new(Machines::FirstSeenAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Machines::LastSeenAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-machines-agent_id")
                            .from(Machines::Table, Machines::AgentId)
                            .to(Agents::Table, Agents::Id)
                            .on_delete(ForeignKeyAction::Restrict)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_machines-machine_code")
                    .table(Machines::Table)
                    .col(Machines::MachineCode)
                    .to_owned(),
            )
            .await?;

        // ── group_agent ─────────────────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(GroupAgent::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(GroupAgent::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(GroupAgent::GroupId).big_integer().not_null())
                    .col(ColumnDef::new(GroupAgent::AgentId).big_integer().not_null())
                    .col(ColumnDef::new(GroupAgent::Role).integer().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-group_agent-group_id")
                            .from(GroupAgent::Table, GroupAgent::GroupId)
                            .to(Groups::Table, Groups::Id)
                            .on_delete(ForeignKeyAction::Restrict)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-group_agent-agent_id")
                            .from(GroupAgent::Table, GroupAgent::AgentId)
                            .to(Agents::Table, Agents::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_group_agent-group_id-agent_id")
                    .table(GroupAgent::Table)
                    .col(GroupAgent::GroupId)
                    .col(GroupAgent::AgentId)
                    .unique()
                    .to_owned(),
            )
            .await?;
        for (name, col) in [
            ("idx_group_agent-group_id", GroupAgent::GroupId),
            ("idx_group_agent-agent_id", GroupAgent::AgentId),
        ] {
            manager
                .create_index(
                    Index::create()
                        .name(name)
                        .table(GroupAgent::Table)
                        .col(col)
                        .to_owned(),
                )
                .await?;
        }

        // ── task_suite_agent ────────────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(TaskSuiteAgent::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(TaskSuiteAgent::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(TaskSuiteAgent::TaskSuiteId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TaskSuiteAgent::AgentId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(TaskSuiteAgent::SelectionType)
                            .integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(TaskSuiteAgent::MatchedTags).array(ColumnType::Text))
                    .col(
                        ColumnDef::new(TaskSuiteAgent::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(ColumnDef::new(TaskSuiteAgent::CreatorId).big_integer())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-task_suite_agent-task_suite_id")
                            .from(TaskSuiteAgent::Table, TaskSuiteAgent::TaskSuiteId)
                            .to(TaskSuites::Table, TaskSuites::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-task_suite_agent-agent_id")
                            .from(TaskSuiteAgent::Table, TaskSuiteAgent::AgentId)
                            .to(Agents::Table, Agents::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-task_suite_agent-creator_id")
                            .from(TaskSuiteAgent::Table, TaskSuiteAgent::CreatorId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::SetNull)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_task_suite_agent-task_suite_id-agent_id")
                    .table(TaskSuiteAgent::Table)
                    .col(TaskSuiteAgent::TaskSuiteId)
                    .col(TaskSuiteAgent::AgentId)
                    .unique()
                    .to_owned(),
            )
            .await?;
        for (name, col) in [
            (
                "idx_task_suite_agent-task_suite_id",
                TaskSuiteAgent::TaskSuiteId,
            ),
            ("idx_task_suite_agent-agent_id", TaskSuiteAgent::AgentId),
            (
                "idx_task_suite_agent-selection_type",
                TaskSuiteAgent::SelectionType,
            ),
        ] {
            manager
                .create_index(
                    Index::create()
                        .name(name)
                        .table(TaskSuiteAgent::Table)
                        .col(col)
                        .to_owned(),
                )
                .await?;
        }

        // ── suite_agent_jobs ────────────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(SuiteAgentJobs::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SuiteAgentJobs::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(SuiteAgentJobs::TaskSuiteId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SuiteAgentJobs::JobNumber)
                            .integer()
                            .not_null(),
                    )
                    // Nullable + SetNull FK: agent rows aren't reaped by the
                    // system, but an admin may manually delete one via SQL;
                    // that nulls this rather than being blocked, keeping the
                    // job's history.
                    .col(ColumnDef::new(SuiteAgentJobs::AgentId).big_integer())
                    .col(
                        ColumnDef::new(SuiteAgentJobs::State)
                            .integer()
                            .not_null()
                            .default(0), // Provision
                    )
                    .col(
                        ColumnDef::new(SuiteAgentJobs::TasksCompleted)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(SuiteAgentJobs::TasksFailed)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(ColumnDef::new(SuiteAgentJobs::FailureReason).json_binary())
                    .col(
                        ColumnDef::new(SuiteAgentJobs::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(ColumnDef::new(SuiteAgentJobs::StartedAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(SuiteAgentJobs::FinishedAt).timestamp_with_time_zone())
                    .col(
                        ColumnDef::new(SuiteAgentJobs::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-suite_agent_jobs-task_suite_id")
                            .from(SuiteAgentJobs::Table, SuiteAgentJobs::TaskSuiteId)
                            .to(TaskSuites::Table, TaskSuites::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-suite_agent_jobs-agent_id")
                            .from(SuiteAgentJobs::Table, SuiteAgentJobs::AgentId)
                            .to(Agents::Table, Agents::Id)
                            .on_delete(ForeignKeyAction::SetNull)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        // User-facing key: job_number is ascending per suite (across agents).
        // The unique index is the safety net for max(job_number)+1 allocation.
        manager
            .create_index(
                Index::create()
                    .unique()
                    .name("idx_suite_agent_jobs-suite_job")
                    .table(SuiteAgentJobs::Table)
                    .col(SuiteAgentJobs::TaskSuiteId)
                    .col(SuiteAgentJobs::JobNumber)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_suite_agent_jobs-state_finished")
                    .table(SuiteAgentJobs::Table)
                    .col(SuiteAgentJobs::State)
                    .col(SuiteAgentJobs::FinishedAt)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_suite_agent_jobs-agent_id")
                    .table(SuiteAgentJobs::Table)
                    .col(SuiteAgentJobs::AgentId)
                    .to_owned(),
            )
            .await?;

        // ── hook_tasks ──────────────────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(HookTasks::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(HookTasks::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    // Handle for indexing this hook's logs in the shared
                    // `artifacts` table (artifacts.task_id = hook_tasks.uuid).
                    .col(
                        ColumnDef::new(HookTasks::Uuid)
                            .uuid()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(HookTasks::SuiteAgentJobId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(HookTasks::HookType).integer().not_null())
                    .col(ColumnDef::new(HookTasks::Spec).json_binary().not_null())
                    .col(
                        ColumnDef::new(HookTasks::State)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(ColumnDef::new(HookTasks::Result).json_binary())
                    .col(ColumnDef::new(HookTasks::StartedAt).timestamp_with_time_zone())
                    .col(ColumnDef::new(HookTasks::CompletedAt).timestamp_with_time_zone())
                    .col(
                        ColumnDef::new(HookTasks::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(HookTasks::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-hook_tasks-suite_agent_job_id")
                            .from(HookTasks::Table, HookTasks::SuiteAgentJobId)
                            .to(SuiteAgentJobs::Table, SuiteAgentJobs::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_hook_tasks-job_id")
                    .table(HookTasks::Table)
                    .col(HookTasks::SuiteAgentJobId)
                    .to_owned(),
            )
            .await?;
        // One hook execution per (job, hook_type); also the upsert key.
        manager
            .create_index(
                Index::create()
                    .unique()
                    .name("idx_hook_tasks-job_hook_type")
                    .table(HookTasks::Table)
                    .col(HookTasks::SuiteAgentJobId)
                    .col(HookTasks::HookType)
                    .to_owned(),
            )
            .await?;

        // ── link tasks to suites (task_suite_id only) ───────────────────
        for tbl in [Tasks::ActiveTasks, Tasks::ArchivedTasks] {
            manager
                .alter_table(
                    Table::alter()
                        .table(tbl)
                        .add_column_if_not_exists(ColumnDef::new(Tasks::TaskSuiteId).big_integer())
                        .to_owned(),
                )
                .await?;
            manager
                .alter_table(
                    Table::alter()
                        .table(tbl)
                        .add_foreign_key(
                            TableForeignKey::new()
                                .name(match tbl {
                                    Tasks::ActiveTasks => "fk-active_tasks-task_suite_id",
                                    _ => "fk-archived_tasks-task_suite_id",
                                })
                                .from_tbl(tbl)
                                .from_col(Tasks::TaskSuiteId)
                                .to_tbl(TaskSuites::Table)
                                .to_col(TaskSuites::Id)
                                .on_delete(ForeignKeyAction::SetNull)
                                .on_update(ForeignKeyAction::Cascade),
                        )
                        .to_owned(),
                )
                .await?;
            manager
                .create_index(
                    Index::create()
                        .if_not_exists()
                        .name(match tbl {
                            Tasks::ActiveTasks => "idx_active_tasks-task_suite_id",
                            _ => "idx_archived_tasks-task_suite_id",
                        })
                        .table(tbl)
                        .col(Tasks::TaskSuiteId)
                        .and_where(Expr::col(Tasks::TaskSuiteId).is_not_null())
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Unlink tasks from suites.
        for (tbl, fk, idx) in [
            (
                Tasks::ActiveTasks,
                "fk-active_tasks-task_suite_id",
                "idx_active_tasks-task_suite_id",
            ),
            (
                Tasks::ArchivedTasks,
                "fk-archived_tasks-task_suite_id",
                "idx_archived_tasks-task_suite_id",
            ),
        ] {
            manager
                .drop_index(Index::drop().name(idx).table(tbl).to_owned())
                .await?;
            manager
                .alter_table(
                    Table::alter()
                        .table(tbl)
                        .drop_foreign_key(Alias::new(fk))
                        .to_owned(),
                )
                .await?;
            manager
                .alter_table(
                    Table::alter()
                        .table(tbl)
                        .drop_column(Tasks::TaskSuiteId)
                        .to_owned(),
                )
                .await?;
        }

        // Drop tables in reverse FK-dependency order (indexes go with them).
        for tbl in [
            HookTasks::Table.into_iden(),
            SuiteAgentJobs::Table.into_iden(),
            TaskSuiteAgent::Table.into_iden(),
            GroupAgent::Table.into_iden(),
            Machines::Table.into_iden(),
            Agents::Table.into_iden(),
            TaskSuites::Table.into_iden(),
        ] {
            manager
                .drop_table(Table::drop().table(tbl).if_exists().to_owned())
                .await?;
        }

        Ok(())
    }
}

#[derive(DeriveIden)]
enum TaskSuites {
    Table,
    Id,
    Uuid,
    Name,
    Description,
    GroupId,
    CreatorId,
    Tags,
    Labels,
    Priority,
    WorkerSchedule,
    ExecHooks,
    State,
    LastTaskSubmittedAt,
    TotalTasks,
    IncompleteTasks,
    CreatedAt,
    UpdatedAt,
    CompletedAt,
}

#[derive(DeriveIden)]
enum Agents {
    Table,
    Id,
    Uuid,
    CreatorId,
    Tags,
    Labels,
    State,
    LastHeartbeat,
    AssignedTaskSuiteId,
    Metadata,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Machines {
    Table,
    Id,
    AgentId,
    MachineCode,
    Metadata,
    FirstSeenAt,
    LastSeenAt,
}

#[derive(DeriveIden)]
enum GroupAgent {
    Table,
    Id,
    GroupId,
    AgentId,
    Role,
}

#[derive(DeriveIden)]
enum TaskSuiteAgent {
    Table,
    Id,
    TaskSuiteId,
    AgentId,
    SelectionType,
    MatchedTags,
    CreatedAt,
    CreatorId,
}

#[derive(DeriveIden)]
enum SuiteAgentJobs {
    Table,
    Id,
    TaskSuiteId,
    JobNumber,
    AgentId,
    State,
    TasksCompleted,
    TasksFailed,
    FailureReason,
    CreatedAt,
    StartedAt,
    FinishedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum HookTasks {
    Table,
    Id,
    Uuid,
    SuiteAgentJobId,
    HookType,
    Spec,
    State,
    Result,
    StartedAt,
    CompletedAt,
    CreatedAt,
    UpdatedAt,
}

/// Existing task tables, referenced only to add `task_suite_id`. Variant names
/// map to the `active_tasks` / `archived_tasks` table names.
#[derive(DeriveIden, Clone, Copy)]
#[allow(clippy::enum_variant_names)]
enum Tasks {
    ActiveTasks,
    ArchivedTasks,
    TaskSuiteId,
}

#[derive(DeriveIden)]
enum Groups {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
}

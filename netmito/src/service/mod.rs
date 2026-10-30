//! # Lock order: suite → job → hook → agent → task
//!
//! A transaction that locks rows of more than one of `task_suites`, `suite_agent_jobs`,
//! `hook_tasks`, `agents` and `active_tasks` must lock them in this order. A transaction can skip
//! a table, but it must not go back to an earlier table.
//!
//! An `UPDATE` or `DELETE` keeps its row locks until the transaction commits, not until the
//! statement ends. Thus, a write in one statement does not solve the order problem. A
//! transaction keeps all of its locks. If two transactions lock the same two rows in opposite
//! order, they deadlock. Postgres finds the deadlock only after `deadlock_timeout`.
//!
//! The pairs that occur now:
//!
//! - suite → job → hook → agent: `accept` locks the suite in `create_job`, inserts the job and its
//!   hooks, then assigns the agent.
//! - job → hook → agent → task: `complete`, a force stop and a retirement end the job, close its
//!   hooks, release the agent, then put its tasks back to `Ready`. No transaction locks a task and
//!   then an agent. A commit that also changed its agent would deadlock with `complete`.
//! - suite → task: each commit and each cancel.
//!
//! **A foreign-key write locks the row that it refers to.** An insert of a job, or a change of
//! `agents.assigned_task_suite_id` to a suite, takes `FOR KEY SHARE` on the suite row. This lock
//! conflicts with the `FOR UPDATE` of `create_job`. Thus such a write locks the suite, and it must
//! come before all job, hook, agent and task locks. A change of a reference to NULL checks nothing.
//!
//! There are two correct ways to keep suite → task:
//!
//! - The suite id is known before the write, for example on each commit and on a single-task
//!   cancel. Write the suite row first, then the task rows.
//! - The suite ids come only from the task rows, for example on a batch cancel. Lock the suites
//!   in the same statement that removes the tasks, and remove a task only after its suite is
//!   locked.
//!
//! These do not need the order, because no transaction waits for their locks while they wait:
//!
//! - The task claim of `agent_fetch_tasks` locks only task rows with `FOR UPDATE SKIP LOCKED`.
//! - A single autocommit statement that locks one row, for example the agent update of a
//!   heartbeat.

pub mod agent;
pub mod auth;
pub mod group;
pub mod s3;
pub mod suite;
pub mod task;
pub mod user;
pub mod worker;

mod suite_agent;

pub fn name_validator(name: &str) -> bool {
    let l = name.len();
    l > 0
        && l < 256
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

//! # Lock order: suite → job → agent → task
//!
//! A transaction that locks rows from more than one of `task_suites`,
//! `suite_agent_jobs`, `agents` and `active_tasks` must take them in that order.
//! Skipping a table is fine; going back to an earlier one is not.
//!
//! Row locks taken by an `UPDATE` or `DELETE` live until the transaction
//! commits, not until the statement ends — so single-statement writes do not
//! make ordering someone else's problem. A transaction accumulates its locks,
//! and two of them taking the same pair in opposite orders deadlock, after a
//! full `deadlock_timeout` of waiting first.
//!
//! Where each pair comes from today:
//!
//! - suite → job → agent: `accept` locks the suite in `create_job`, inserts the
//!   job, then assigns the agent.
//! - job → agent → task: `complete`, a force stop and retirement end the job,
//!   release the agent, then reclaim its tasks. Nothing takes a task and then
//!   an agent; a commit that also touched its agent would deadlock against
//!   `complete`.
//! - suite → task: every commit and cancel.
//!
//! **A foreign-key write locks the row it points at.** Inserting a job, or
//! setting `agents.assigned_task_suite_id` to a suite, takes `FOR KEY SHARE` on
//! that suite row, which conflicts with `create_job`'s `FOR UPDATE`. So such a
//! write counts as locking the suite, and must come before any job, agent or
//! task lock. Setting a reference to NULL checks nothing.
//!
//! Two shapes satisfy suite → task:
//!
//! - The suite id is known up front, as on every commit and single-task
//!   cancel: write the suite row first and let the rest follow.
//! - The suite ids are only derivable from the task rows, as on a batch
//!   cancel: lock them in the same statement that removes the tasks, and gate
//!   the removal on the lock, so no task is taken before its suite is.
//!
//! Exempt, because they can never be waited on while waiting themselves:
//!
//! - `claim_candidates` holds `FOR UPDATE SKIP LOCKED` on tasks alone.
//! - A single autocommit statement that locks one row, like the heartbeat's
//!   agent update.

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

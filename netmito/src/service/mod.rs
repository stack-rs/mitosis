//! # Lock order: suite before task
//!
//! A transaction that writes both a `task_suites` row and `active_tasks` rows
//! must write the suite row first.
//!
//! Row locks taken by an `UPDATE` or `DELETE` live until the transaction
//! commits, not until the statement ends — so single-statement writes do not
//! make ordering someone else's problem. A transaction accumulates its locks,
//! and two of them taking the same pair in opposite orders deadlock, after a
//! full `deadlock_timeout` of waiting first.
//!
//! Two shapes satisfy it:
//!
//! - The suite id is known up front, as on every commit and single-task
//!   cancel: write the suite row first and let the rest follow.
//! - The suite ids are only derivable from the task rows, as on a batch
//!   cancel: lock them in the same statement that removes the tasks, and gate
//!   the removal on the lock, so no task is taken before its suite is.
//!
//! Touching task rows without writing a suite row is exempt: `claim_candidates`
//! holds `FOR UPDATE SKIP LOCKED` on tasks alone, and nothing waits on it.

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

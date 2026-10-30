//! # Lock order: suite before task
//!
//! A transaction that writes a `task_suites` row and `active_tasks` rows must write the suite
//! row first.
//!
//! An `UPDATE` or `DELETE` keeps its row locks until the transaction commits, not until the
//! statement ends. Thus, a write in one statement does not solve the order problem. A
//! transaction keeps all of its locks. If two transactions lock the same two rows in opposite
//! order, they deadlock. Postgres finds the deadlock only after `deadlock_timeout`.
//!
//! There are two correct ways to keep this order:
//!
//! - The suite id is known before the write, for example on each commit and on a single-task
//!   cancel. Write the suite row first, then the task rows.
//! - The suite ids come only from the task rows, for example on a batch cancel. Lock the suites
//!   in the same statement that removes the tasks, and remove a task only after its suite is
//!   locked.
//!
//! This rule does not apply to a transaction that locks task rows and does not write a suite
//! row. `claim_candidates` locks only task rows with `FOR UPDATE SKIP LOCKED`, and no
//! transaction waits for these locks.

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

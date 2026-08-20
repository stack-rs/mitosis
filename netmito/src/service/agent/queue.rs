//! What the coordinator keeps in memory about the suites that have a live job.
//!
//! One entry per suite with at least one in-flight job. Each holds
//!
//! - the jobs running it, keyed by agent — the map is the entry's refcount, so
//!   the last job to end takes the entry with it;
//! - the ids of tasks that became `Ready` while the entry existed, oldest first;
//! - an optional reservation window.
//!
//! The database stays the authority on every claim. This is a dispatch hint and
//! a policy knob, never a queue a task can be lost from: an entry dropped
//! because its last job ended — or because the coordinator restarted — costs the
//! next fetch one ordinary `ORDER BY priority` query and nothing else.
//!
//! ## The reservation window
//!
//! A task submitted into a suite that a job is already sitting idle on should be
//! run by that job, not by a second one provisioned from scratch to run one
//! task. The submission tells those jobs directly ([`AgentNotification::TasksAvailable`])
//! and reserves the suite for the agents already on it: while the window lasts,
//! no other agent may accept the suite or claim its tasks.
//!
//! **The window covers the round trip, not the backlog.** It ends as soon as
//! every job that was told has come back — or as soon as the queue is empty,
//! whichever happens first — so in the ordinary case it lasts one fetch. Work
//! still queued after the warm jobs have taken their fill is work more agents
//! genuinely help with, and they are not kept out of it. [`RESERVATION_WINDOW`]
//! is only the cap for the job that never comes back at all; past it the suite
//! is on offer to everyone again, one idle heartbeat later at worst.
//!
//! [`AgentNotification::TasksAvailable`]: crate::schema::AgentNotification::TasksAvailable

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use sea_orm::{prelude::*, QuerySelect};
use uuid::Uuid;

use crate::{
    config::InfraPool,
    entity::{agents as Agent, suite_agent_jobs as SuiteAgentJobs},
    service::agent::job,
};

/// The longest a suite stays reserved for the jobs already running it. Reached
/// only when a job that was told about a task never comes back for it, so this
/// is the delay a suite pays for an agent that died between the notification and
/// the fetch — not the normal cost of a window, which is one round trip.
pub const RESERVATION_WINDOW: Duration = Duration::from_secs(30);

/// Live dispatch state for every suite that has an in-flight job. Cloning shares
/// it; it lives in [`InfraPool`].
#[derive(Debug, Clone, Default)]
pub struct SuiteQueues {
    inner: Arc<Mutex<HashMap<i64, SuiteQueue>>>,
}

#[derive(Debug, Default)]
struct SuiteQueue {
    /// The in-flight jobs on this suite, by agent. Empty means the entry goes.
    jobs: HashMap<i64, JobState>,
    /// Task ids seen becoming `Ready`, oldest first.
    ready: VecDeque<i64>,
    reservation: Option<Reservation>,
}

#[derive(Debug)]
struct JobState {
    agent_uuid: Uuid,
    /// The job's last fetch came back empty, so it is sitting in its hold loop
    /// with room to run whatever lands next.
    waiting: bool,
}

#[derive(Debug)]
struct Reservation {
    /// The agents that may take from this suite. Every agent running it when the
    /// window opened, not only the ones told about the task: an incumbent that
    /// frees a slot a moment later is exactly who this is for.
    agents: Vec<i64>,
    /// Of those, the ones that were told and have not fetched since. The window
    /// is over when this empties — everyone it was held open for has had their
    /// turn.
    awaited: Vec<i64>,
    until: Instant,
}

impl SuiteQueue {
    /// Drop a reservation that has run out, so every read below can treat
    /// `Some` as "in force".
    fn expire(&mut self, now: Instant) {
        if self.reservation.as_ref().is_some_and(|r| r.until <= now) {
            self.reservation = None;
        }
    }

    fn reserved_against(&self, agent_id: i64) -> bool {
        self.reservation
            .as_ref()
            .is_some_and(|r| !r.agents.contains(&agent_id))
    }

    /// End the window once it has nothing left to protect: the queue is empty,
    /// or every job it was opened for has been back.
    fn settle_reservation(&mut self) {
        let done = self
            .reservation
            .as_ref()
            .is_some_and(|r| r.awaited.is_empty());
        if done || self.ready.is_empty() {
            self.reservation = None;
        }
    }
}

impl SuiteQueues {
    pub fn new() -> Self {
        Self::default()
    }

    /// A mutex this small is only ever held for a map lookup, so a poisoned one
    /// means a panic elsewhere, not corrupt state worth propagating.
    fn lock(&self) -> MutexGuard<'_, HashMap<i64, SuiteQueue>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A job started on this suite: create the entry, or join the one there.
    ///
    /// A job that opens during a reservation is deliberately *not* added to it —
    /// it is the newcomer the window exists to keep out, and it got in by racing
    /// the check in `accept`.
    pub fn open(&self, suite_id: i64, agent_id: i64, agent_uuid: Uuid) {
        let mut queues = self.lock();
        let queue = queues.entry(suite_id).or_default();
        queue.jobs.insert(
            agent_id,
            JobState {
                agent_uuid,
                // It fetches as its first act; nothing to offer it yet.
                waiting: false,
            },
        );
    }

    /// A job ended. The entry — queue and reservation with it — goes when the
    /// last one does.
    pub fn close(&self, suite_id: i64, agent_id: i64) {
        let mut queues = self.lock();
        let Some(queue) = queues.get_mut(&suite_id) else {
            return;
        };
        queue.jobs.remove(&agent_id);
        if let Some(reservation) = queue.reservation.as_mut() {
            reservation.agents.retain(|id| *id != agent_id);
            // It is never coming back for the task it was told about.
            reservation.awaited.retain(|id| *id != agent_id);
        }
        queue.settle_reservation();
        if queue.jobs.is_empty() {
            queues.remove(&suite_id);
        }
    }

    /// Every job on this suite ended at once (the suite was force-cancelled).
    pub fn close_suite(&self, suite_id: i64) {
        self.lock().remove(&suite_id);
    }

    /// A task of this suite is claimable. Ignored unless a job is running the
    /// suite: with nobody to hand it to, the next `accept` reads it from the
    /// database anyway.
    pub fn push_ready(&self, suite_id: i64, task_ids: impl IntoIterator<Item = i64>) {
        let mut queues = self.lock();
        let Some(queue) = queues.get_mut(&suite_id) else {
            return;
        };
        queue.ready.extend(task_ids);
    }

    /// The agents whose jobs on this suite are waiting for work, and so are the
    /// right home for a task that just landed.
    pub fn waiting_agents(&self, suite_id: i64) -> Vec<(i64, Uuid)> {
        let queues = self.lock();
        let Some(queue) = queues.get(&suite_id) else {
            return Vec::new();
        };
        queue
            .jobs
            .iter()
            .filter(|(_, job)| job.waiting)
            .map(|(agent_id, job)| (*agent_id, job.agent_uuid))
            .collect()
    }

    /// Hold the suite for the agents already running it until `notified` have
    /// each been back, or `window` runs out. No-op for a suite with no job,
    /// which has no incumbent to hold it for.
    pub fn reserve(&self, suite_id: i64, notified: Vec<i64>, window: Duration) {
        let mut queues = self.lock();
        let Some(queue) = queues.get_mut(&suite_id) else {
            return;
        };
        if queue.jobs.is_empty() {
            return;
        }
        queue.reservation = Some(Reservation {
            agents: queue.jobs.keys().copied().collect(),
            awaited: notified,
            until: Instant::now() + window,
        });
    }

    /// Is a window in force on this suite? Then it is nobody else's to be
    /// offered, and the idle tier is skipped altogether.
    pub fn is_reserved(&self, suite_id: i64) -> bool {
        let now = Instant::now();
        let mut queues = self.lock();
        let Some(queue) = queues.get_mut(&suite_id) else {
            return false;
        };
        queue.expire(now);
        queue.reservation.is_some()
    }

    /// The suites this agent must not be offered right now, for
    /// [`matching::best_available_suite_id`] to exclude.
    ///
    /// [`matching::best_available_suite_id`]: crate::service::agent::matching::best_available_suite_id
    pub fn blocked_for(&self, agent_id: i64) -> Vec<i64> {
        let now = Instant::now();
        let mut queues = self.lock();
        queues
            .iter_mut()
            .filter_map(|(suite_id, queue)| {
                queue.expire(now);
                queue.reserved_against(agent_id).then_some(*suite_id)
            })
            .collect()
    }

    /// May this agent claim from the suite, and which tasks should it be handed
    /// first?
    ///
    /// `None` is a refusal: the suite is reserved for someone else and this
    /// agent gets nothing, whatever the database holds. `Some` carries up to
    /// `want` task ids to try before falling back to a query — they are removed
    /// from the queue either way, since a stale id costs the caller nothing and
    /// leaving it in would hand it to the next agent too.
    pub fn take(&self, suite_id: i64, agent_id: i64, want: usize) -> Option<Vec<i64>> {
        let now = Instant::now();
        let mut queues = self.lock();
        let Some(queue) = queues.get_mut(&suite_id) else {
            return Some(Vec::new());
        };
        queue.expire(now);
        if queue.reserved_against(agent_id) {
            return None;
        }
        let taken = queue.ready.len().min(want);
        Some(queue.ready.drain(..taken).collect())
    }

    /// Record what a fetch came back with.
    ///
    /// A fetch that came back empty is what makes a job "waiting", and it is
    /// exact rather than a proxy: a job asks only when it has somewhere to put
    /// the answer, and an empty answer is precisely what parks it in its hold
    /// loop until something arrives. A job that got *anything* comes back on its
    /// own within a round trip and needs no telling. No slot accounting, no
    /// count of running tasks, nothing to keep in step with the agent.
    ///
    /// This is also what settles the window: the job has taken its turn, and
    /// once every job the window was opened for has had one — or once there is
    /// nothing queued left to protect — holding the suite shut only delays
    /// whoever else could help.
    pub fn served(&self, suite_id: i64, agent_id: i64, served: usize) {
        let mut queues = self.lock();
        let Some(queue) = queues.get_mut(&suite_id) else {
            return;
        };
        if let Some(job) = queue.jobs.get_mut(&agent_id) {
            job.waiting = served == 0;
        }
        if let Some(reservation) = queue.reservation.as_mut() {
            reservation.awaited.retain(|id| *id != agent_id);
        }
        queue.settle_reservation();
    }
}

/// Rebuild the entries for jobs that outlived the coordinator.
pub async fn restore(pool: &InfraPool) -> crate::error::Result<()> {
    let jobs: Vec<(i64, Option<i64>)> = SuiteAgentJobs::Entity::find()
        .select_only()
        .column(SuiteAgentJobs::Column::TaskSuiteId)
        .column(SuiteAgentJobs::Column::AgentId)
        .filter(SuiteAgentJobs::Column::State.is_in(job::IN_FLIGHT))
        .into_tuple()
        .all(&pool.db)
        .await?;
    if jobs.is_empty() {
        return Ok(());
    }

    let agent_ids: Vec<i64> = jobs.iter().filter_map(|(_, agent_id)| *agent_id).collect();
    let uuids: HashMap<i64, Uuid> = Agent::Entity::find()
        .select_only()
        .column(Agent::Column::Id)
        .column(Agent::Column::Uuid)
        .filter(Agent::Column::Id.is_in(agent_ids))
        .into_tuple::<(i64, Uuid)>()
        .all(&pool.db)
        .await?
        .into_iter()
        .collect();

    let mut restored = 0;
    for (suite_id, agent_id) in jobs {
        let Some(agent_id) = agent_id else { continue };
        let Some(uuid) = uuids.get(&agent_id) else {
            continue;
        };
        pool.suite_queues.open(suite_id, agent_id, *uuid);
        restored += 1;
    }
    tracing::info!(jobs = restored, "Restored the in-flight suite jobs");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    #[test]
    fn entry_lives_as_long_as_its_jobs() {
        let queues = SuiteQueues::new();
        queues.open(1, 10, agent(10));
        queues.open(1, 11, agent(11));
        queues.push_ready(1, [100, 101]);

        queues.close(1, 10);
        assert_eq!(queues.take(1, 11, 1), Some(vec![100]));

        // The last job out takes the queue with it, tail and all.
        queues.close(1, 11);
        assert_eq!(queues.take(1, 11, 1), Some(Vec::new()));
    }

    #[test]
    fn tasks_are_only_queued_for_a_suite_with_a_job() {
        let queues = SuiteQueues::new();
        queues.push_ready(1, [100]);
        assert_eq!(queues.take(1, 10, 1), Some(Vec::new()));
    }

    #[test]
    fn a_fetch_that_came_back_empty_marks_the_job_waiting() {
        let queues = SuiteQueues::new();
        queues.open(1, 10, agent(10));
        assert!(queues.waiting_agents(1).is_empty());

        queues.served(1, 10, 2);
        assert!(queues.waiting_agents(1).is_empty());

        queues.served(1, 10, 0);
        assert_eq!(queues.waiting_agents(1), vec![(10, agent(10))]);
    }

    #[test]
    fn a_reservation_holds_the_suite_for_its_incumbents() {
        let queues = SuiteQueues::new();
        queues.open(1, 10, agent(10));
        queues.push_ready(1, [100]);
        queues.reserve(1, vec![10], RESERVATION_WINDOW);

        assert_eq!(queues.blocked_for(11), vec![1]);
        assert!(queues.blocked_for(10).is_empty());
        assert_eq!(queues.take(1, 11, 1), None);
        assert_eq!(queues.take(1, 10, 1), Some(vec![100]));

        // Taken, so the window has served its purpose and ends early.
        queues.served(1, 10, 1);
        assert!(queues.blocked_for(11).is_empty());
    }

    #[test]
    fn a_reservation_ends_once_the_jobs_it_was_held_for_have_been_back() {
        let queues = SuiteQueues::new();
        queues.open(1, 10, agent(10));
        // More work than the one waiting job will take in a batch.
        queues.push_ready(1, [100, 101, 102]);
        queues.reserve(1, vec![10], RESERVATION_WINDOW);
        assert_eq!(queues.blocked_for(11), vec![1]);

        assert_eq!(queues.take(1, 10, 1), Some(vec![100]));
        queues.served(1, 10, 1);
        // Still two queued: work enough that a second agent is welcome to it.
        assert!(queues.blocked_for(11).is_empty());
    }

    #[test]
    fn a_reservation_nobody_acts_on_lapses() {
        let window = Duration::from_millis(20);
        let queues = SuiteQueues::new();
        queues.open(1, 10, agent(10));
        queues.push_ready(1, [100]);
        queues.reserve(1, vec![10], window);
        assert_eq!(queues.take(1, 11, 1), None);

        std::thread::sleep(window * 2);
        assert!(queues.blocked_for(11).is_empty());
        assert_eq!(queues.take(1, 11, 1), Some(vec![100]));
    }

    #[test]
    fn a_job_that_ends_stops_holding_the_reservation() {
        let queues = SuiteQueues::new();
        queues.open(1, 10, agent(10));
        queues.open(1, 11, agent(11));
        queues.push_ready(1, [100, 101]);
        queues.reserve(1, vec![10, 11], RESERVATION_WINDOW);
        assert_eq!(queues.blocked_for(12), vec![1]);

        queues.close(1, 10);
        queues.close(1, 11);
        // No incumbent left to hold it for.
        assert!(queues.blocked_for(12).is_empty());
    }
}

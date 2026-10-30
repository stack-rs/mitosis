//! The in-memory dispatch state of the coordinator. It has one entry for each suite that has a job
//! in flight.
//!
//! An entry has:
//!
//! - the jobs on the suite, by agent. For each job: its budget and the number of tasks that it
//!   holds. The entry goes away when its last job ends.
//! - the ids of the ready tasks of the suite, in the order that a claim takes them.
//! - an optional reservation. While the reservation is active, only the agents that run the suite
//!   can accept the suite or claim from it.
//!
//! The database decides each claim. This state only decides which agents get a notification, and
//! it counts the ready tasks of each suite.

use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use sea_orm::{prelude::*, QuerySelect};
use uuid::Uuid;

use crate::{
    config::InfraPool,
    entity::{
        active_tasks as ActiveTasks, agents as Agent, state::TaskState,
        suite_agent_jobs as SuiteAgentJobs, task_suites as TaskSuites,
    },
    schema::WorkerSchedulePlan,
    service::agent::job,
};

/// The maximum time that a suite stays reserved for its jobs. Usually a reservation ends earlier,
/// when the notified jobs fetch. This limit applies when a notified job does not fetch, for example
/// because its agent stopped.
pub const RESERVATION_WINDOW: Duration = Duration::from_secs(30);

/// The maximum number of tasks in one claim, so that a large `worker_count` cannot lock too many
/// rows.
const MAX_FETCH_BATCH: u32 = 256;

/// The maximum number of tasks that one job can hold: the tasks that its workers run, and with
/// `prefetch`, one more task for each worker. Then a worker that becomes free gets its next task
/// without a round trip.
pub fn task_budget(schedule: &WorkerSchedulePlan) -> u32 {
    match schedule {
        WorkerSchedulePlan::FixedWorkers {
            worker_count,
            prefetch,
            ..
        } => ((*worker_count).max(1))
            .saturating_mul(1 + u32::from(*prefetch))
            .min(MAX_FETCH_BATCH),
    }
}

/// The dispatch state of each suite that has a job in flight. All clones share one state.
#[derive(Debug, Clone, Default)]
pub struct SuiteQueues {
    inner: Arc<Mutex<HashMap<i64, SuiteQueue>>>,
}

#[derive(Debug, Default)]
struct SuiteQueue {
    /// The jobs in flight on this suite, by agent. When this is empty, the entry goes away.
    jobs: HashMap<i64, JobState>,
    /// The ready tasks of the suite, in the order that a claim takes them.
    ready: Vec<ReadyTask>,
    reservation: Option<Reservation>,
    /// Queued ids that the last reconcile did not find as ready in the database. The next
    /// reconcile removes these ids if it also does not find them.
    suspects: HashSet<i64>,
}

/// A ready task. The order is the order of the claim query: the highest priority first, then the
/// lowest id. Thus the queued ids are the ids that the claim query selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReadyTask {
    priority: i32,
    id: i64,
}

impl Ord for ReadyTask {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .priority
            .cmp(&self.priority)
            .then(self.id.cmp(&other.id))
    }
}

impl PartialOrd for ReadyTask {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Default)]
struct JobState {
    agent_uuid: Uuid,
    /// The job id in the suite. If two jobs hold the same part of their budget, the older job gets
    /// the offer first.
    job_id: i32,
    /// The maximum number of tasks that the job can hold ([`task_budget`]). The queue keeps a copy,
    /// because the offer does not read the database. Each fetch updates it.
    budget: u32,
    /// The number of tasks that the job holds: claimed and not committed. A task leaves a job only
    /// when the job commits it or when the job ends.
    outstanding: u32,
}

/// The jobs to notify about the ready tasks of a suite.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct FollowUpOffer {
    /// The jobs to notify. The job that holds the largest part of its budget is first. Empty if no
    /// job has free room.
    pub agents: Vec<(i64, Uuid)>,
    /// True if these jobs have room for all the ready tasks. Only then does the suite get a
    /// reservation.
    pub covered: bool,
}

#[derive(Debug)]
struct Reservation {
    /// The agents that can claim from the suite: all the agents on the suite when the reservation
    /// started. This includes agents that did not get a notification.
    agents: Vec<i64>,
    /// The notified agents that did not fetch yet. When this is empty, the reservation ends.
    awaited: Vec<i64>,
    until: Instant,
}

impl SuiteQueue {
    /// Remove a reservation after its end time. Then `Some` always means an active reservation.
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

    /// End the reservation when all the notified jobs fetched, or when the queue is empty. An empty
    /// queue does not prove that the suite has no ready task. But an early end only lets more
    /// agents in, and that is safe.
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

    /// The lock is held only for map operations. Thus a poisoned lock means a panic in a different
    /// thread, not bad data, and the data is safe to use.
    fn lock(&self) -> MutexGuard<'_, HashMap<i64, SuiteQueue>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Add a job to the entry of its suite. Create the entry if it does not exist.
    ///
    /// A job that starts during a reservation does not join the reservation, because the
    /// reservation is to keep such a job out.
    ///
    /// `outstanding` is the number of tasks that the job holds. It is zero for a new job.
    ///
    /// Returns true if this call created the entry. Then the caller must give the ready tasks of
    /// the suite to [`SuiteQueues::push_ready`]. The queue ignores tasks of a suite without an
    /// entry, thus the new entry does not have them.
    pub fn open(
        &self,
        suite_id: i64,
        agent_id: i64,
        agent_uuid: Uuid,
        job_id: i32,
        budget: u32,
        outstanding: u32,
    ) -> bool {
        let mut queues = self.lock();
        let opened = !queues.contains_key(&suite_id);
        let queue = queues.entry(suite_id).or_default();
        queue.jobs.insert(
            agent_id,
            JobState {
                agent_uuid,
                job_id,
                budget,
                outstanding,
            },
        );
        tracing::trace!(
            suite_id,
            agent_id,
            job_id,
            budget,
            outstanding,
            opened,
            jobs = queue.jobs.len(),
            ready = queue.ready.len(),
            "A job joined the suite queue"
        );
        opened
    }

    /// Remove a job that ended. When the last job of the suite ends, the entry goes away.
    ///
    /// `reclaimed` is the number of tasks that the database took back from the job. It must be
    /// equal to `outstanding`. A difference is a bug in the counts. This function only logs the
    /// difference, because the job state goes away.
    pub fn close(&self, suite_id: i64, agent_id: i64, reclaimed: usize) {
        let mut queues = self.lock();
        match queues
            .get_mut(&suite_id)
            .and_then(|queue| queue.jobs.remove(&agent_id))
        {
            Some(job) if job.outstanding as usize != reclaimed => tracing::warn!(
                suite_id,
                agent_id,
                job_id = job.job_id,
                counted = job.outstanding,
                reclaimed,
                "An ending job gave back a different number of tasks than the queue counted"
            ),
            None if reclaimed > 0 => tracing::warn!(
                suite_id,
                agent_id,
                reclaimed,
                "A job that the suite queue did not track gave back tasks"
            ),
            _ => {}
        }
        let Some(queue) = queues.get_mut(&suite_id) else {
            return;
        };
        if let Some(reservation) = queue.reservation.as_mut() {
            reservation.agents.retain(|id| *id != agent_id);
            reservation.awaited.retain(|id| *id != agent_id);
        }
        queue.settle_reservation();
        let (jobs, ready) = (queue.jobs.len(), queue.ready.len());
        if queue.jobs.is_empty() {
            queues.remove(&suite_id);
        }
        tracing::trace!(
            suite_id,
            agent_id,
            reclaimed,
            jobs,
            ready,
            "A job left the suite queue"
        );
    }

    /// Remove the entry of a suite when a cancel ends all its jobs at the same time. There is no
    /// count check, because the cancel archives the tasks itself.
    pub fn close_suite(&self, suite_id: i64) {
        if let Some(queue) = self.lock().remove(&suite_id) {
            tracing::trace!(
                suite_id,
                jobs = queue.jobs.len(),
                ready = queue.ready.len(),
                "Dropped the suite queue with all its jobs"
            );
        }
    }

    /// Add tasks of the suite that are now ready: new tasks, tasks that left `Pending`, or tasks
    /// that an agent gave back.
    ///
    /// **Call this in the transaction that makes the tasks ready, before the commit.** A claim
    /// cannot lock a row before the commit. Thus the [`SuiteQueues::served`] call that removes the
    /// task always occurs after this call. If the push occurs after the commit, a claim can take
    /// the task first. Then the push adds a running task, and nothing removes it.
    ///
    /// If the transaction rolls back, the ids stay in the queue until [`reconcile`] removes them.
    ///
    /// If no job runs the suite, this function does nothing. A new entry reads the ready tasks from
    /// the database. A task that is already in the queue does not get a second position.
    pub fn push_ready(&self, suite_id: i64, tasks: impl IntoIterator<Item = (i64, i32)>) {
        let mut queues = self.lock();
        let Some(queue) = queues.get_mut(&suite_id) else {
            return;
        };
        let before = queue.ready.len();
        queue.ready.extend(
            tasks
                .into_iter()
                .map(|(id, priority)| ReadyTask { id, priority }),
        );
        queue.ready.sort_unstable();
        queue.ready.dedup();
        tracing::trace!(
            suite_id,
            added = queue.ready.len().saturating_sub(before),
            ready = queue.ready.len(),
            "Queued ready tasks before the commit that makes them ready"
        );
    }

    /// Add the tasks of a reclaim, as [`job::reclaim_agent_tasks`] returns them. Tasks without a
    /// suite are for workers, thus this function skips them.
    ///
    /// Call this in the reclaim transaction, for the same reason as [`SuiteQueues::push_ready`].
    ///
    /// [`job::reclaim_agent_tasks`]: crate::service::agent::job::reclaim_agent_tasks
    pub fn push_reclaimed(&self, reclaimed: &[(i64, Option<i64>, i32)]) {
        let mut queues = self.lock();
        let mut touched: HashMap<i64, usize> = HashMap::new();
        for (task_id, suite_id, priority) in reclaimed {
            let Some(suite_id) = *suite_id else { continue };
            let Some(queue) = queues.get_mut(&suite_id) else {
                continue;
            };
            queue.ready.push(ReadyTask {
                id: *task_id,
                priority: *priority,
            });
            *touched.entry(suite_id).or_default() += 1;
        }
        for (suite_id, pushed) in touched {
            if let Some(queue) = queues.get_mut(&suite_id) {
                queue.ready.sort_unstable();
                queue.ready.dedup();
                tracing::trace!(
                    suite_id,
                    pushed,
                    ready = queue.ready.len(),
                    "Queued the tasks that a reclaim took from an agent"
                );
            }
        }
    }

    /// Remove tasks that a cancel archived. A claim removes its tasks with
    /// [`SuiteQueues::served`].
    pub fn remove_ready(&self, suite_id: i64, task_ids: impl IntoIterator<Item = i64>) {
        let gone: HashSet<i64> = task_ids.into_iter().collect();
        if gone.is_empty() {
            return;
        }
        let mut queues = self.lock();
        let Some(queue) = queues.get_mut(&suite_id) else {
            return;
        };
        let before = queue.ready.len();
        queue.ready.retain(|task| !gone.contains(&task.id));
        queue.settle_reservation();
        tracing::trace!(
            suite_id,
            removed = before.saturating_sub(queue.ready.len()),
            ready = queue.ready.len(),
            "Removed cancelled tasks from the queue"
        );
    }

    /// The suites that have an entry.
    pub fn tracked_suites(&self) -> Vec<i64> {
        self.lock().keys().copied().collect()
    }

    /// Remove the queued ids that the database does not show as ready.
    ///
    /// `claimable` has the ready tasks of each suite that was read. A suite with no ready task has
    /// an empty set. An entry that is not in the map does not change.
    ///
    /// **An id goes only when two reads in sequence do not show it.** The queue gets a task before
    /// the commit that makes it ready. Thus one read cannot find the difference between a
    /// transaction that did not commit yet and a transaction that rolled back. At the second read,
    /// the transaction has ended.
    ///
    /// This function does not add ids. A ready task that is not in the queue is a missing
    /// `push_ready`, and `served` logs it. An add here can race with the claim that takes the task.
    pub fn drop_unclaimable(&self, claimable: &HashMap<i64, HashSet<i64>>) {
        let mut queues = self.lock();
        for (suite_id, claimable) in claimable {
            let Some(queue) = queues.get_mut(suite_id) else {
                continue;
            };
            let missing: HashSet<i64> = queue
                .ready
                .iter()
                .map(|task| task.id)
                .filter(|id| !claimable.contains(id))
                .collect();
            let condemned: HashSet<i64> = missing.intersection(&queue.suspects).copied().collect();
            queue.suspects = missing.difference(&condemned).copied().collect();
            if condemned.is_empty() {
                if !queue.suspects.is_empty() {
                    tracing::trace!(
                        suite_id,
                        suspects = queue.suspects.len(),
                        ready = queue.ready.len(),
                        "Queued tasks are not ready in the database yet; one read is not proof"
                    );
                }
                continue;
            }
            queue.ready.retain(|task| !condemned.contains(&task.id));
            queue.settle_reservation();
            // Only a bug or a rollback causes this. Log the ids to find the tasks in
            // `archived_tasks`.
            tracing::warn!(
                suite_id,
                dropped = condemned.len(),
                ?condemned,
                suspects = queue.suspects.len(),
                ready = queue.ready.len(),
                "Removed queued tasks that two reads did not show as ready"
            );
        }
    }

    /// Select the jobs to notify about the ready tasks of the suite.
    ///
    /// Only jobs with free room. The job that holds the largest part of its budget is first. The
    /// list stops when the jobs have room for all the ready tasks. If all the jobs together do not
    /// have this room, `covered` is false. A suite with no ready task gives an empty offer.
    pub fn follow_up_offer(&self, suite_id: i64) -> FollowUpOffer {
        let queues = self.lock();
        let Some(queue) = queues.get(&suite_id) else {
            return FollowUpOffer::default();
        };
        let backlog = queue.ready.len();
        if backlog == 0 {
            return FollowUpOffer::default();
        }

        let mut ranked: Vec<(i64, &JobState)> = queue
            .jobs
            .iter()
            .filter(|(_, job)| job.outstanding < job.budget)
            .map(|(agent_id, job)| (*agent_id, job))
            .collect();
        // Compare `outstanding / budget` by cross-multiplication, because the jobs of one suite can
        // have different budgets.
        ranked.sort_by(|(_, a), (_, b)| {
            (u64::from(b.outstanding) * u64::from(a.budget))
                .cmp(&(u64::from(a.outstanding) * u64::from(b.budget)))
                .then(a.job_id.cmp(&b.job_id))
        });

        let mut agents = Vec::new();
        let mut room = 0usize;
        for (agent_id, job) in ranked {
            if room >= backlog {
                break;
            }
            room += job.budget.saturating_sub(job.outstanding) as usize;
            agents.push((agent_id, job.agent_uuid));
        }
        FollowUpOffer {
            agents,
            covered: room >= backlog,
        }
    }

    /// End the reservation of the suite.
    pub fn release_reservation(&self, suite_id: i64) {
        let mut queues = self.lock();
        if let Some(queue) = queues.get_mut(&suite_id) {
            if queue.reservation.take().is_some() {
                tracing::trace!(
                    suite_id,
                    ready = queue.ready.len(),
                    "Released the suite reservation: its jobs cannot take all its ready tasks"
                );
            }
        }
    }

    /// Reserve the suite for its jobs until each agent in `notified` fetches or `window` ends. This
    /// function does nothing for a suite without a job.
    pub fn reserve(&self, suite_id: i64, notified: Vec<i64>, window: Duration) {
        let mut queues = self.lock();
        let Some(queue) = queues.get_mut(&suite_id) else {
            return;
        };
        if queue.jobs.is_empty() {
            return;
        }
        let awaited = notified.len();
        queue.reservation = Some(Reservation {
            agents: queue.jobs.keys().copied().collect(),
            awaited: notified,
            until: Instant::now() + window,
        });
        tracing::trace!(
            suite_id,
            agents = queue.jobs.len(),
            awaited,
            ready = queue.ready.len(),
            ?window,
            "Reserved the suite for its jobs"
        );
    }

    /// The suites that are reserved for agents other than this agent.
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

    /// Get the queued ids that a claim tries first. `None` refuses the claim, because the suite is
    /// reserved for other agents.
    ///
    /// The number of ids is at most the free room of the job. This function does not remove the
    /// ids: [`SuiteQueues::served`] removes the ids that the claim locked. Thus two agents can get
    /// the same ids, and `SKIP LOCKED` gives each row to one of them.
    ///
    /// A suite without an entry gives no ids and no refusal. This must not occur, because each job
    /// in flight has an entry.
    pub fn take(&self, suite_id: i64, agent_id: i64, budget: u32) -> Option<Vec<i64>> {
        let now = Instant::now();
        let mut queues = self.lock();
        let Some(queue) = queues.get_mut(&suite_id) else {
            tracing::debug!(
                suite_id,
                agent_id,
                budget,
                "A job fetched from a suite that the queue does not track"
            );
            return Some(Vec::new());
        };
        queue.expire(now);
        if queue.reserved_against(agent_id) {
            tracing::trace!(
                suite_id,
                agent_id,
                ready = queue.ready.len(),
                "Refused a claim: the suite is reserved for its jobs"
            );
            return None;
        }
        let want = queue.jobs.get_mut(&agent_id).map_or(budget, |job| {
            job.budget = budget;
            budget.saturating_sub(job.outstanding)
        });
        let hinted: Vec<i64> = queue
            .ready
            .iter()
            .take(want as usize)
            .map(|task| task.id)
            .collect();
        tracing::trace!(
            suite_id,
            agent_id,
            budget,
            want,
            hinted = hinted.len(),
            ready = queue.ready.len(),
            "Gave the queued ids to a claim"
        );
        Some(hinted)
    }

    /// Record the tasks that a claim gave to a job. The tasks leave the queue, and the job holds
    /// them until it commits them.
    ///
    /// `hinted` is the number of ids from [`SuiteQueues::take`]. If the claim got more tasks, a
    /// code path possibly made a task ready without [`SuiteQueues::push_ready`]. Then the queue
    /// counts too few ready tasks, and the offers are too small. A task that became ready between
    /// the two calls gives the same log line and is not a bug. Thus one log line is not proof.
    ///
    /// For the reservation, each fetch of a job counts, also a refused or empty fetch.
    pub fn served(&self, suite_id: i64, agent_id: i64, hinted: usize, claimed: &[i64]) {
        let taken: HashSet<i64> = claimed.iter().copied().collect();
        let mut queues = self.lock();
        let Some(queue) = queues.get_mut(&suite_id) else {
            if !claimed.is_empty() {
                tracing::warn!(
                    suite_id,
                    agent_id,
                    claimed = claimed.len(),
                    "A claim was served for a suite that the queue does not track"
                );
            }
            return;
        };
        if claimed.len() > hinted {
            // The claimed ids that are not in the queue. If the list is empty, the tasks became
            // ready between `take` and this call, and that is not a bug.
            let queued: HashSet<i64> = queue.ready.iter().map(|task| task.id).collect();
            let unknown: Vec<i64> = claimed
                .iter()
                .copied()
                .filter(|id| !queued.contains(id))
                .collect();
            tracing::warn!(
                suite_id,
                agent_id,
                hinted,
                claimed = claimed.len(),
                unknown = unknown.len(),
                unknown_ids = ?unknown,
                ready = queue.ready.len(),
                "A claim found more ready tasks of a suite than the queue had"
            );
        }
        let mut outstanding = 0;
        if let Some(job) = queue.jobs.get_mut(&agent_id) {
            job.outstanding = job.outstanding.saturating_add(claimed.len() as u32);
            outstanding = job.outstanding;
        }
        queue.ready.retain(|task| !taken.contains(&task.id));
        if let Some(reservation) = queue.reservation.as_mut() {
            reservation.awaited.retain(|id| *id != agent_id);
        }
        queue.settle_reservation();
        tracing::trace!(
            suite_id,
            agent_id,
            claimed = claimed.len(),
            outstanding,
            ready = queue.ready.len(),
            reserved = queue.reservation.is_some(),
            "Recorded the tasks of a claim"
        );
    }

    /// Move a queued task to the position for its new priority. If the task is not in the queue,
    /// do nothing: a claim possibly took it, and the task is not ready anymore.
    pub fn reprioritize(&self, suite_id: i64, task_id: i64, priority: i32) {
        let mut queues = self.lock();
        let Some(queue) = queues.get_mut(&suite_id) else {
            return;
        };
        let Some(task) = queue.ready.iter_mut().find(|task| task.id == task_id) else {
            return;
        };
        if task.priority == priority {
            return;
        }
        task.priority = priority;
        queue.ready.sort_unstable();
        tracing::trace!(
            suite_id,
            task_id,
            priority,
            "Moved a queued task after a priority change"
        );
    }

    /// Record that the job committed a task.
    pub fn finished(&self, suite_id: i64, agent_id: i64) {
        let mut queues = self.lock();
        if let Some(job) = queues
            .get_mut(&suite_id)
            .and_then(|queue| queue.jobs.get_mut(&agent_id))
        {
            job.outstanding = job.outstanding.saturating_sub(1);
            tracing::trace!(
                suite_id,
                agent_id,
                outstanding = job.outstanding,
                budget = job.budget,
                "A job committed a task"
            );
        }
    }
}

/// The ready tasks of these suites, as `(suite id, task id, priority)`.
pub async fn ready_tasks<C: ConnectionTrait>(
    db: &C,
    suite_ids: Vec<i64>,
) -> crate::error::Result<Vec<(i64, i64, i32)>> {
    Ok(ActiveTasks::Entity::find()
        .select_only()
        .column(ActiveTasks::Column::TaskSuiteId)
        .column(ActiveTasks::Column::Id)
        .column(ActiveTasks::Column::Priority)
        .filter(ActiveTasks::Column::TaskSuiteId.is_in(suite_ids))
        .filter(ActiveTasks::Column::State.eq(TaskState::Ready))
        .into_tuple::<(Option<i64>, i64, i32)>()
        .all(db)
        .await?
        .into_iter()
        .filter_map(|(suite_id, task_id, priority)| Some((suite_id?, task_id, priority)))
        .collect())
}

/// Remove from each entry the ids that the database does not show as ready.
///
/// A transaction that added ids and then rolled back leaves ids that no claim takes. These ids
/// make the offers too large. See [`SuiteQueues::drop_unclaimable`] for the two reads.
///
/// This function has its own span, because no request starts it.
#[tracing::instrument(
    name = "suite_queue_reconcile",
    skip_all,
    fields(suites = tracing::field::Empty)
)]
pub async fn reconcile(pool: &InfraPool) -> crate::error::Result<()> {
    let suite_ids = pool.suite_queues.tracked_suites();
    if suite_ids.is_empty() {
        return Ok(());
    }
    tracing::Span::current().record("suites", suite_ids.len());
    // Put each suite that was read in the map, also a suite with no ready task.
    let mut claimable: HashMap<i64, HashSet<i64>> =
        suite_ids.iter().map(|id| (*id, HashSet::new())).collect();
    for (suite_id, task_id, _) in ready_tasks(&pool.db, suite_ids).await? {
        claimable.entry(suite_id).or_default().insert(task_id);
    }
    tracing::debug!(
        claimable = claimable.values().map(HashSet::len).sum::<usize>(),
        "Read the ready tasks of the tracked suites"
    );
    pool.suite_queues.drop_unclaimable(&claimable);
    Ok(())
}

/// At coordinator start, create the entries for the jobs in flight.
pub async fn restore(pool: &InfraPool) -> crate::error::Result<()> {
    let jobs: Vec<(i64, Option<i64>, i32)> = SuiteAgentJobs::Entity::find()
        .select_only()
        .column(SuiteAgentJobs::Column::TaskSuiteId)
        .column(SuiteAgentJobs::Column::AgentId)
        .column(SuiteAgentJobs::Column::JobId)
        .filter(SuiteAgentJobs::Column::State.is_in(job::IN_FLIGHT))
        .into_tuple()
        .all(&pool.db)
        .await?;
    if jobs.is_empty() {
        return Ok(());
    }

    let agent_ids: Vec<i64> = jobs
        .iter()
        .filter_map(|(_, agent_id, _)| *agent_id)
        .collect();
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

    let suite_ids: Vec<i64> = jobs.iter().map(|(suite_id, _, _)| *suite_id).collect();
    let budgets: HashMap<i64, u32> = TaskSuites::Entity::find()
        .select_only()
        .column(TaskSuites::Column::Id)
        .column(TaskSuites::Column::WorkerSchedule)
        .filter(TaskSuites::Column::Id.is_in(suite_ids.clone()))
        .into_tuple::<(i64, serde_json::Value)>()
        .all(&pool.db)
        .await?
        .into_iter()
        .filter_map(|(suite_id, schedule)| {
            serde_json::from_value::<WorkerSchedulePlan>(schedule)
                .inspect_err(|e| {
                    tracing::error!(suite_id, "Stored worker schedule is unreadable: {e}");
                })
                .ok()
                .map(|plan| (suite_id, task_budget(&plan)))
        })
        .collect();

    // Count the tasks that each job holds. If the count is zero, the queue offers a full budget to
    // a job that already holds tasks.
    let mut held: HashMap<(i64, Uuid), u32> = HashMap::new();
    for (suite_id, runner) in ActiveTasks::Entity::find()
        .select_only()
        .column(ActiveTasks::Column::TaskSuiteId)
        .column(ActiveTasks::Column::RunnerUuid)
        .filter(ActiveTasks::Column::TaskSuiteId.is_in(suite_ids.clone()))
        .filter(ActiveTasks::Column::RunnerUuid.is_not_null())
        .into_tuple::<(Option<i64>, Option<Uuid>)>()
        .all(&pool.db)
        .await?
    {
        let (Some(suite_id), Some(runner)) = (suite_id, runner) else {
            continue;
        };
        *held.entry((suite_id, runner)).or_default() += 1;
    }

    let mut claimable: HashMap<i64, Vec<(i64, i32)>> = HashMap::new();
    for (suite_id, task_id, priority) in ready_tasks(&pool.db, suite_ids).await? {
        claimable
            .entry(suite_id)
            .or_default()
            .push((task_id, priority));
    }

    let mut restored = 0;
    for (suite_id, agent_id, job_id) in jobs {
        let Some(agent_id) = agent_id else { continue };
        let Some(uuid) = uuids.get(&agent_id) else {
            continue;
        };
        // If the schedule cannot be read, the budget is zero until the next fetch sets it.
        let budget = budgets.get(&suite_id).copied().unwrap_or(0);
        let outstanding = held.get(&(suite_id, *uuid)).copied().unwrap_or(0);
        if pool
            .suite_queues
            .open(suite_id, agent_id, *uuid, job_id, budget, outstanding)
        {
            pool.suite_queues
                .push_ready(suite_id, claimable.remove(&suite_id).unwrap_or_default());
        }
        restored += 1;
    }
    tracing::info!(jobs = restored, "Restored the suite jobs in flight");
    Ok(())
}

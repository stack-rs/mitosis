//! What the coordinator keeps in memory about the suites that have a live job.
//!
//! One entry per suite with at least one in-flight job. Each holds
//!
//! - the jobs running it, keyed by agent — the map is the entry's refcount, so
//!   the last job to end takes the entry with it;
//! - the ids of its claimable tasks, in the order a claim would take them;
//! - an optional reservation window.
//!
//! The database stays the authority on every claim. This is a dispatch hint and
//! a policy knob, never a queue a task can be lost from: an entry dropped
//! because its last job ended — or because the coordinator restarted — costs the
//! next fetch one ordinary `ORDER BY priority` query and nothing else.
//!
//! ## How full a job is
//!
//! A job may hold [`task_budget`] tasks at once — what its workers can be
//! running, plus one more worker's worth to keep them fed. How many it is
//! holding right now is counted here: up on the batch it is handed, down as it
//! commits them, and gone with the entry when the job ends. The agent is told
//! how many it may take; it never asks for a number, because "what is this job
//! holding" is a question about a claim the coordinator itself granted.
//!
//! Both numbers are kept per job rather than per suite. Today a suite's schedule
//! gives every job on it the same budget, but a scheme that sized a job to the
//! machine it landed on would not, and nothing here would need to change.
//!
//! The count is audited where it dies: an ending job hands back exactly the
//! tasks it never committed, so [`SuiteQueues::close`] compares the two and
//! warns when they disagree.
//!
//! ## Who a suite's new work is offered to
//!
//! A task submitted into a suite that a job is already sitting idle on should be
//! run by that job, not by a second one provisioned from scratch to run one
//! task. So the offer goes to the jobs already on the suite, **fullest first** —
//! most tasks held, of those with room. Work packs onto the jobs that are nearly
//! full instead of spreading a task each across every free slot in the fleet,
//! which leaves whole agents free to drain and move to another suite. Only as
//! many jobs are told as it takes to cover the backlog
//! ([`SuiteQueues::follow_up_offer`]).
//!
//! ## The queue is the suite's claimable tasks, exactly
//!
//! Not a sample of them. It is seeded from the database when the suite's first
//! job opens, added to as tasks become claimable, and emptied only by the claim
//! that takes them or the cancellation that archives them. Exactness is what
//! lets it answer "how much is waiting", and that answer decides whether more
//! agents are worth provisioning: a queue holding a fraction of the truth would
//! leave every suite whose work arrived before its first agent looking nearly
//! idle, and it would be left to whoever was already there.
//!
//! It is an invariant with upkeep, then: **a path that makes a suite task
//! claimable, or takes one away, has to say so here.** One that forgets shows up
//! in the log rather than in the dispatch — see [`SuiteQueues::take`].
//!
//! ## The reservation window
//!
//! The jobs that were told are told directly ([`AgentNotification::TasksAvailable`]),
//! and the suite is reserved for the agents already on it: while the window
//! lasts, no other agent may accept the suite or claim its tasks.
//!
//! **Only an offer the warm jobs can cover is reserved.** The window exists to
//! stop a second job being provisioned for work an existing one can absorb; work
//! beyond what they can absorb is work more agents genuinely help with, and a
//! job provisioned for it earns its hook.
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

/// The longest a suite stays reserved for the jobs already running it. Reached
/// only when a job that was told about a task never comes back for it, so this
/// is the delay a suite pays for an agent that died between the notification and
/// the fetch — not the normal cost of a window, which is one round trip.
pub const RESERVATION_WINDOW: Duration = Duration::from_secs(30);

/// Upper bound on a single claim batch, so an unvalidated `worker_count` cannot
/// ask the coordinator to lock an unbounded number of rows.
const MAX_FETCH_BATCH: u32 = 256;

/// The most tasks one job may hold at a time: what its workers can be running,
/// plus what it may keep claimed and ready for them.
///
/// The buffer is one worker's worth per worker, so a slot that frees finds its
/// next task already claimed instead of paying a round trip for it. That depth
/// is not the suite's to choose — only whether it wants the buffer at all. A
/// suite that does not gets its slot count as the whole budget, and a task of
/// its is claimed only once there is a free slot to take it: a round trip per
/// task, worth it when the tasks run far longer than that and holding one
/// claimed on a busy agent is worse than leaving it for a free one.
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
    /// The suite's claimable tasks, in the order a claim would take them.
    ready: Vec<ReadyTask>,
    reservation: Option<Reservation>,
}

/// A claimable task, ordered the way the claim query hands them out: highest
/// priority first, and within a priority the one submitted first.
///
/// Matching that order is what makes the queue useful as a hint — the ids handed
/// to a claim are the ones it would have chosen for itself, so it can take them
/// by id instead of scanning for them.
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
    /// The suite's own ordinal for this job, so the fullest-first order can
    /// break its ties on the oldest job — the one closest to being done with
    /// this suite, and so the one worth filling rather than starting another.
    job_id: i32,
    /// The most this job may hold at once, from [`task_budget`]. Refreshed by
    /// every fetch, since the schedule is the authority on it and this is only a
    /// copy for [`SuiteQueues::follow_up_offer`], which has no database to ask.
    budget: u32,
    /// How many tasks this job is holding: handed to it and not yet committed.
    ///
    /// Every way a task can leave a job either passes through the commit that
    /// decrements this ([`SuiteQueues::finished`]) or ends the job itself — a
    /// retirement, a forced stop, a cancelled suite — which drops the whole
    /// entry. So there is no third path for this to drift down, and it cannot
    /// drift up: the coordinator is the only thing that hands a task out.
    outstanding: u32,
}

/// The answer to "who should be told this suite has work?", from
/// [`SuiteQueues::follow_up_offer`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct FollowUpOffer {
    /// The jobs to tell, fullest first. Empty when no job on the suite has room.
    pub agents: Vec<(i64, Uuid)>,
    /// Whether the room they have left adds up to what is queued. Only a
    /// covered offer is worth reserving the suite for.
    pub covered: bool,
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

    /// End the window once it has nothing left to protect: every job it was
    /// opened for has been back, or there are no hinted ids left. The second is
    /// the weaker signal — an empty hint list is not an empty suite — but it errs
    /// towards letting other agents in, which is the safe way to be wrong.
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
    /// `outstanding` is what the job is already holding — zero for a job just
    /// accepted, and whatever the database says for one [`restore`] is
    /// rebuilding.
    ///
    /// Returns whether this call created the suite's entry, and so whether its
    /// claimable tasks still have to be read out of the database and handed to
    /// [`SuiteQueues::push_ready`]. A suite with no entry drops everything
    /// pushed at it, so without that seeding the queue would know only about
    /// work that arrived after its first agent did.
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
        opened
    }

    /// A job ended. The entry — queue and reservation with it — goes when the
    /// last one does.
    ///
    /// `reclaimed` is how many tasks the database just took back from this job,
    /// and is the audit of everything this module counted: the tasks an ending
    /// job still holds are exactly the ones it was handed and never committed,
    /// so it should equal `outstanding` to the task. A mismatch is a bug in the
    /// bookkeeping — a decrement that never ran, a hand-out that was never
    /// recorded — and it is logged rather than corrected, since the entry is
    /// going either way and the number is only useful as a symptom.
    ///
    /// It can also be a race rather than a bug, on the paths that end a job
    /// under a working agent: a commit that lands between the reclaim and this
    /// call is archived rather than reclaimed, and may decrement after the
    /// comparison. One line either side of a forced stop or a retirement means
    /// little; a mismatch on a graceful `complete`, where every slot has already
    /// joined, or a drift that grows, is the real thing.
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
                "An ending job gave back a different number of tasks than it was counted holding"
            ),
            // Nothing here was tracking the job, so there is nothing to
            // reconcile — but the database having tasks to take back from it
            // says something should have been.
            None if reclaimed > 0 => tracing::warn!(
                suite_id,
                agent_id,
                reclaimed,
                "A job the suite queue was not tracking gave tasks back on its way out"
            ),
            _ => {}
        }
        let Some(queue) = queues.get_mut(&suite_id) else {
            return;
        };
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
    ///
    /// No audit here, unlike [`SuiteQueues::close`]: a force-cancel settles the
    /// suite's tasks itself, in the transaction that kills the jobs, so nothing
    /// is reclaimed per agent to compare against.
    pub fn close_suite(&self, suite_id: i64) {
        self.lock().remove(&suite_id);
    }

    /// These tasks of the suite are claimable now — newly submitted, triggered
    /// out of `Pending`, or handed back by an agent that will not run them.
    ///
    /// Ignored unless a job is running the suite: with no entry there is nothing
    /// to hand them to, and the entry that opens next reads the suite's
    /// claimable tasks out of the database for itself ([`SuiteQueues::open`]).
    ///
    /// Idempotent, so seeding an entry that a submission has already pushed to
    /// cannot count the same task twice.
    pub fn push_ready(&self, suite_id: i64, tasks: impl IntoIterator<Item = (i64, i32)>) {
        let mut queues = self.lock();
        let Some(queue) = queues.get_mut(&suite_id) else {
            return;
        };
        queue.ready.extend(
            tasks
                .into_iter()
                .map(|(id, priority)| ReadyTask { id, priority }),
        );
        queue.ready.sort_unstable();
        queue.ready.dedup();
    }

    /// These tasks are not claimable any more, and it was not a claim that took
    /// them: a cancellation archived them where they stood.
    ///
    /// A claim removes its own through [`SuiteQueues::served`], which knows
    /// exactly which rows it locked.
    pub fn remove_ready(&self, suite_id: i64, task_ids: impl IntoIterator<Item = i64>) {
        let gone: HashSet<i64> = task_ids.into_iter().collect();
        if gone.is_empty() {
            return;
        }
        let mut queues = self.lock();
        let Some(queue) = queues.get_mut(&suite_id) else {
            return;
        };
        queue.ready.retain(|task| !gone.contains(&task.id));
        queue.settle_reservation();
    }

    /// Who this suite's claimable work should be offered to, and whether they
    /// can cover it.
    ///
    /// The jobs with room, fullest first, cut off as soon as the room they have
    /// left adds up to the backlog. `covered` false means the work outruns them:
    /// the caller tells the idle agents too, and reserves nothing.
    ///
    /// The backlog is `ready.len()`, which is every claimable task of the suite
    /// and not merely the ones this entry happened to see. An empty one is
    /// offered to nobody: there is nothing to hand out, and whatever prompted
    /// the offer has already been taken.
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
        // Fullest first, as a fraction of each job's own budget — two jobs on
        // one suite need not be the same size. Compared by cross-multiplying, so
        // the fraction never has to be one.
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

    /// Let the suite out of a window that no longer makes sense, because the
    /// work queued on it has outgrown what the jobs holding it can take.
    pub fn release_reservation(&self, suite_id: i64) {
        let mut queues = self.lock();
        if let Some(queue) = queues.get_mut(&suite_id) {
            queue.reservation = None;
        }
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

    /// May this agent claim from the suite, how many tasks may it be handed, and
    /// which ones should it be tried on first?
    ///
    /// `None` is a refusal: the suite is reserved for someone else and this
    /// agent gets nothing, whatever the database holds. `Some` carries the size
    /// of the batch — `budget` less what this job is already holding — and up to
    /// that many queued task ids to try before falling back to a query. The ids
    /// are removed from the queue either way, since a stale id costs the caller
    /// nothing and leaving it in would hand it to the next agent too.
    ///
    /// The ids are only read, not taken: what a claim actually locked is what
    /// leaves the queue, through [`SuiteQueues::served`]. Handing the same ids to
    /// two agents at once costs nothing — `SKIP LOCKED` gives the row to one of
    /// them and the other falls back to a query — while removing them up front
    /// would lose whatever the claim did not take.
    ///
    /// Because the queue holds *every* claimable task, a claim that comes back
    /// with more rows than it was offered ids means the database had claimable
    /// work the queue did not know about. `served` reports that.
    ///
    /// `budget` is passed in rather than read from the job because the caller has
    /// the suite in hand; the copy kept on the job is for
    /// [`SuiteQueues::follow_up_offer`], which has no database to ask.
    ///
    /// A fetch for a suite with no entry is answered with the whole budget. It
    /// should not happen — `open` and `restore` between them cover every live
    /// job — and erring towards handing work out is the harmless way to be
    /// wrong.
    pub fn take(&self, suite_id: i64, agent_id: i64, budget: u32) -> Option<(u32, Vec<i64>)> {
        let now = Instant::now();
        let mut queues = self.lock();
        let Some(queue) = queues.get_mut(&suite_id) else {
            return Some((budget, Vec::new()));
        };
        queue.expire(now);
        if queue.reserved_against(agent_id) {
            return None;
        }
        // The schedule is the authority on the budget, so a fetch is also when a
        // job `restore` rebuilt learns what its own is.
        let want = queue.jobs.get_mut(&agent_id).map_or(budget, |job| {
            job.budget = budget;
            budget.saturating_sub(job.outstanding)
        });
        let hinted = queue
            .ready
            .iter()
            .take(want as usize)
            .map(|task| task.id)
            .collect();
        Some((want, hinted))
    }

    /// Record what a fetch handed a job: those tasks are its to run until it
    /// commits them, and they leave the queue, since nobody else may be offered
    /// them.
    ///
    /// `hinted` is how many ids [`SuiteQueues::take`] offered it. More claimed
    /// than that means the database held claimable tasks this queue did not know
    /// about — some path that makes a task claimable is not calling
    /// [`SuiteQueues::push_ready`] — which is worth a line in the log, because
    /// the same gap silently under-reports the backlog that decides whether more
    /// agents are provisioned. A task becoming claimable between the two calls
    /// looks identical and is harmless, so one line is not proof of a bug; a
    /// steady stream of them is.
    ///
    /// This is also what settles the window: the job has taken its turn, and
    /// once every job the window was opened for has had one — or once there is
    /// nothing queued left to protect — holding the suite shut only delays
    /// whoever else could help. A fetch that was refused, or that found nothing,
    /// still counts as the turn.
    pub fn served(&self, suite_id: i64, agent_id: i64, hinted: usize, claimed: &[i64]) {
        if claimed.len() > hinted {
            tracing::warn!(
                suite_id,
                agent_id,
                hinted,
                claimed = claimed.len(),
                "A claim found more of a suite's tasks than the queue knew were claimable"
            );
        }
        let taken: HashSet<i64> = claimed.iter().copied().collect();
        let mut queues = self.lock();
        let Some(queue) = queues.get_mut(&suite_id) else {
            return;
        };
        if let Some(job) = queue.jobs.get_mut(&agent_id) {
            job.outstanding = job.outstanding.saturating_add(claimed.len() as u32);
        }
        queue.ready.retain(|task| !taken.contains(&task.id));
        if let Some(reservation) = queue.reservation.as_mut() {
            reservation.awaited.retain(|id| *id != agent_id);
        }
        queue.settle_reservation();
    }

    /// A task this job was holding is committed and gone, so the job has room
    /// for another. Its own slot is free at the same moment, which is what sends
    /// it back to fetch.
    pub fn finished(&self, suite_id: i64, agent_id: i64) {
        let mut queues = self.lock();
        if let Some(job) = queues
            .get_mut(&suite_id)
            .and_then(|queue| queue.jobs.get_mut(&agent_id))
        {
            job.outstanding = job.outstanding.saturating_sub(1);
        }
    }
}

/// The claimable tasks of these suites, as `(suite id, task id, priority)`.
///
/// What [`SuiteQueues::push_ready`] wants when an entry is seeded — at boot for
/// every suite still being run, and in `accept` for the one whose first job is
/// opening.
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

/// Rebuild the entries for jobs that outlived the coordinator.
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

    // What each of those jobs is still holding. Seeding this from zero would
    // read every job as empty and hand it a second budget on top of the tasks it
    // already has — every task an agent has claimed and not committed is still
    // sitting in `active_tasks` with its name on it.
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

    // And what is still waiting to be claimed on them, which is the queue's
    // whole reason for existing: a suite restored without it looks like a suite
    // with nothing to run.
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
        // A budget we could not read is left at zero, which offers the job
        // nothing until its next fetch says what the suite's schedule asks for.
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
    tracing::info!(jobs = restored, "Restored the in-flight suite jobs");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// Open a job on a suite whose jobs may each hold `budget` tasks.
    fn open(queues: &SuiteQueues, suite_id: i64, agent_id: i64, budget: u32) -> bool {
        queues.open(
            suite_id,
            agent_id,
            agent(agent_id as u128),
            agent_id as i32,
            budget,
            0,
        )
    }

    /// Open a job of the same size as its neighbours and hand it enough tasks to
    /// leave it `room` short of full.
    fn open_with_room(queues: &SuiteQueues, suite_id: i64, agent_id: i64, room: u32) {
        const BUDGET: u32 = 8;
        open(queues, suite_id, agent_id, BUDGET);
        let held: Vec<i64> = (0..(BUDGET - room) as i64).map(|n| -n - agent_id).collect();
        queues.push_ready(suite_id, held.iter().map(|id| (*id, 0)));
        queues.served(suite_id, agent_id, held.len(), &held);
    }

    /// Queue `ids` as claimable at the same priority.
    fn ready(queues: &SuiteQueues, suite_id: i64, ids: impl IntoIterator<Item = i64>) {
        queues.push_ready(suite_id, ids.into_iter().map(|id| (id, 0)));
    }

    #[test]
    fn a_suites_schedule_says_how_much_one_job_may_hold() {
        let prefetching = WorkerSchedulePlan::FixedWorkers {
            worker_count: 4,
            cpu_binding: None,
            prefetch: true,
        };
        let bare = WorkerSchedulePlan::FixedWorkers {
            worker_count: 4,
            cpu_binding: None,
            prefetch: false,
        };
        // A task per worker, and a second one each to keep them fed.
        assert_eq!(task_budget(&prefetching), 8);
        // Without the buffer, a task is claimed only for a slot that can start
        // it now.
        assert_eq!(task_budget(&bare), 4);

        let huge = WorkerSchedulePlan::FixedWorkers {
            worker_count: u32::MAX,
            cpu_binding: None,
            prefetch: true,
        };
        assert_eq!(task_budget(&huge), MAX_FETCH_BATCH);
    }

    #[test]
    fn only_the_suites_first_job_is_told_to_seed_the_queue() {
        let queues = SuiteQueues::new();
        assert!(open(&queues, 1, 10, 4));
        // The second job joins an entry that is already keeping the queue.
        assert!(!open(&queues, 1, 11, 4));

        queues.close(1, 10, 0);
        queues.close(1, 11, 0);
        // The entry went with the last job, so the next one seeds again.
        assert!(open(&queues, 1, 12, 4));
    }

    #[test]
    fn entry_lives_as_long_as_its_jobs() {
        let queues = SuiteQueues::new();
        open(&queues, 1, 10, 4);
        open(&queues, 1, 11, 4);
        ready(&queues, 1, [100, 101]);

        queues.close(1, 10, 0);
        assert_eq!(queues.take(1, 11, 1), Some((1, vec![100])));

        // The last job out takes the queue with it, tail and all.
        queues.close(1, 11, 0);
        assert_eq!(queues.take(1, 11, 1), Some((1, Vec::new())));
    }

    #[test]
    fn tasks_are_only_queued_for_a_suite_with_a_job() {
        let queues = SuiteQueues::new();
        ready(&queues, 1, [100]);
        assert_eq!(queues.take(1, 10, 1), Some((1, Vec::new())));
    }

    #[test]
    fn the_queue_hands_out_the_order_a_claim_would_take() {
        let queues = SuiteQueues::new();
        open(&queues, 1, 10, 8);
        queues.push_ready(1, [(100, 0), (101, 5), (102, 5)]);

        // Priority first, and the older task within a priority.
        assert_eq!(queues.take(1, 10, 8), Some((8, vec![101, 102, 100])));
    }

    #[test]
    fn seeding_a_queue_that_was_pushed_to_counts_nothing_twice() {
        let queues = SuiteQueues::new();
        open(&queues, 1, 10, 8);
        ready(&queues, 1, [100, 101]);
        // The seed read the same rows the submission pushed.
        ready(&queues, 1, [100, 101, 102]);

        assert_eq!(queues.take(1, 10, 8), Some((8, vec![100, 101, 102])));
    }

    #[test]
    fn a_batch_is_the_budget_less_what_the_job_holds() {
        let queues = SuiteQueues::new();
        open(&queues, 1, 10, 4);
        ready(&queues, 1, [100, 101, 102, 103, 104]);

        // Holding nothing, so it may take the lot.
        assert_eq!(queues.take(1, 10, 4), Some((4, vec![100, 101, 102, 103])));
        queues.served(1, 10, 4, &[100, 101, 102, 103]);

        // Full: nothing more until it commits one.
        assert_eq!(queues.take(1, 10, 4), Some((0, Vec::new())));
        queues.finished(1, 10);
        assert_eq!(queues.take(1, 10, 4), Some((1, vec![104])));
    }

    #[test]
    fn only_what_a_claim_took_leaves_the_queue() {
        let queues = SuiteQueues::new();
        open(&queues, 1, 10, 4);
        open(&queues, 1, 11, 4);
        ready(&queues, 1, [100, 101, 102]);

        // Both are offered the same ids; nothing is spent by the offer.
        assert_eq!(queues.take(1, 10, 4), Some((4, vec![100, 101, 102])));
        assert_eq!(queues.take(1, 11, 4), Some((4, vec![100, 101, 102])));

        // One of them locks two of the three, and only those two are gone.
        queues.served(1, 10, 3, &[100, 102]);
        assert_eq!(queues.take(1, 11, 4), Some((4, vec![101])));
    }

    #[test]
    fn a_cancelled_task_stops_being_claimable() {
        let queues = SuiteQueues::new();
        open(&queues, 1, 10, 4);
        ready(&queues, 1, [100, 101, 102]);

        queues.remove_ready(1, [101]);
        assert_eq!(queues.take(1, 10, 4), Some((4, vec![100, 102])));
    }

    #[test]
    fn a_job_with_no_room_is_offered_nothing() {
        let queues = SuiteQueues::new();
        open(&queues, 1, 10, 2);
        ready(&queues, 1, [100, 101]);
        assert_eq!(queues.follow_up_offer(1).agents, vec![(10, agent(10))]);

        // Holding both of the two it may: more work is not its to be offered.
        queues.served(1, 10, 2, &[100, 101]);
        ready(&queues, 1, [102, 103]);
        assert!(queues.follow_up_offer(1).agents.is_empty());

        queues.finished(1, 10);
        assert_eq!(queues.follow_up_offer(1).agents, vec![(10, agent(10))]);
    }

    #[test]
    fn nothing_claimable_is_offered_to_nobody() {
        let queues = SuiteQueues::new();
        open(&queues, 1, 10, 8);

        // The queue is the suite's claimable tasks, so an empty one means the
        // work that prompted this has already been taken.
        assert_eq!(queues.follow_up_offer(1), FollowUpOffer::default());
    }

    #[test]
    fn the_fullest_job_is_offered_the_work() {
        let queues = SuiteQueues::new();
        open_with_room(&queues, 1, 10, 3);
        open_with_room(&queues, 1, 11, 1);
        open_with_room(&queues, 1, 12, 2);
        ready(&queues, 1, [100]);

        // One task, so the job with the least room to spare takes it and the
        // other two are left whole.
        let offer = queues.follow_up_offer(1);
        assert_eq!(offer.agents, vec![(11, agent(11))]);
        assert!(offer.covered);
    }

    #[test]
    fn fullest_is_a_fraction_of_each_jobs_own_budget() {
        let queues = SuiteQueues::new();
        // Three of four slots taken, against five of eight: the smaller job is
        // the fuller one, though it is holding less.
        open(&queues, 1, 10, 4);
        ready(&queues, 1, [200, 201, 202, 203, 204, 205, 206, 207]);
        queues.served(1, 10, 3, &[200, 201, 202]);
        open(&queues, 1, 11, 8);
        queues.served(1, 11, 5, &[203, 204, 205, 206, 207]);
        ready(&queues, 1, [100]);

        assert_eq!(queues.follow_up_offer(1).agents, vec![(10, agent(10))]);
    }

    #[test]
    fn ties_go_to_the_older_job() {
        let queues = SuiteQueues::new();
        queues.open(1, 10, agent(10), 7, 2, 0);
        queues.open(1, 11, agent(11), 3, 2, 0);
        ready(&queues, 1, [100]);

        assert_eq!(queues.follow_up_offer(1).agents, vec![(11, agent(11))]);
    }

    #[test]
    fn enough_jobs_are_told_to_cover_the_queue() {
        let queues = SuiteQueues::new();
        open_with_room(&queues, 1, 10, 3);
        open_with_room(&queues, 1, 11, 1);
        open_with_room(&queues, 1, 12, 2);
        ready(&queues, 1, [100, 101, 102]);

        // 1 + 2 covers three tasks, so the roomiest job is not disturbed.
        let offer = queues.follow_up_offer(1);
        assert_eq!(offer.agents, vec![(11, agent(11)), (12, agent(12))]);
        assert!(offer.covered);
    }

    #[test]
    fn a_queue_the_warm_jobs_cannot_cover_is_not_covered() {
        let queues = SuiteQueues::new();
        open_with_room(&queues, 1, 10, 1);
        ready(&queues, 1, [100, 101, 102]);

        let offer = queues.follow_up_offer(1);
        assert_eq!(offer.agents, vec![(10, agent(10))]);
        assert!(!offer.covered);
    }

    #[test]
    fn work_a_suite_had_before_its_first_agent_counts() {
        let queues = SuiteQueues::new();
        open_with_room(&queues, 1, 10, 3);
        // Seeded at `open` from the database, then one more task submitted.
        ready(&queues, 1, [100, 101, 102, 103, 104]);
        ready(&queues, 1, [105]);

        // Six waiting against room for three: the arriving task does not get to
        // hide the five that were already there.
        let offer = queues.follow_up_offer(1);
        assert!(!offer.covered);
    }

    #[test]
    fn a_reservation_holds_the_suite_for_its_incumbents() {
        let queues = SuiteQueues::new();
        open_with_room(&queues, 1, 10, 1);
        ready(&queues, 1, [100]);
        queues.reserve(1, vec![10], RESERVATION_WINDOW);

        assert_eq!(queues.blocked_for(11), vec![1]);
        assert!(queues.blocked_for(10).is_empty());
        assert_eq!(queues.take(1, 11, 8), None);
        assert_eq!(queues.take(1, 10, 8), Some((1, vec![100])));

        // Taken, so the window has served its purpose and ends early.
        queues.served(1, 10, 1, &[100]);
        assert!(queues.blocked_for(11).is_empty());
    }

    #[test]
    fn a_reservation_ends_once_the_jobs_it_was_held_for_have_been_back() {
        let queues = SuiteQueues::new();
        open_with_room(&queues, 1, 10, 1);
        // More work than the one waiting job will take in a batch.
        ready(&queues, 1, [100, 101, 102]);
        queues.reserve(1, vec![10], RESERVATION_WINDOW);
        assert_eq!(queues.blocked_for(11), vec![1]);

        assert_eq!(queues.take(1, 10, 8), Some((1, vec![100])));
        queues.served(1, 10, 1, &[100]);
        // Still two queued: work enough that a second agent is welcome to it.
        assert!(queues.blocked_for(11).is_empty());
    }

    #[test]
    fn a_reservation_nobody_acts_on_lapses() {
        let window = Duration::from_millis(20);
        let queues = SuiteQueues::new();
        open_with_room(&queues, 1, 10, 1);
        ready(&queues, 1, [100]);
        queues.reserve(1, vec![10], window);
        assert_eq!(queues.take(1, 11, 8), None);

        std::thread::sleep(window * 2);
        assert!(queues.blocked_for(11).is_empty());
        assert_eq!(queues.take(1, 11, 8), Some((8, vec![100])));
    }

    #[test]
    fn a_job_that_ends_stops_holding_the_reservation() {
        let queues = SuiteQueues::new();
        open_with_room(&queues, 1, 10, 1);
        open_with_room(&queues, 1, 11, 1);
        ready(&queues, 1, [100, 101]);
        queues.reserve(1, vec![10, 11], RESERVATION_WINDOW);
        assert_eq!(queues.blocked_for(12), vec![1]);

        // Each was handed seven of its eight, and gives back what it never
        // committed.
        queues.close(1, 10, 7);
        queues.close(1, 11, 7);
        // No incumbent left to hold it for.
        assert!(queues.blocked_for(12).is_empty());
    }

    #[test]
    fn work_the_incumbents_cannot_take_lets_everyone_else_in() {
        let queues = SuiteQueues::new();
        open_with_room(&queues, 1, 10, 1);
        ready(&queues, 1, [100]);
        queues.reserve(1, vec![10], RESERVATION_WINDOW);
        assert_eq!(queues.blocked_for(11), vec![1]);

        // Four more land before the notified job has been back for the first.
        ready(&queues, 1, [101, 102, 103, 104]);
        assert!(!queues.follow_up_offer(1).covered);
        queues.release_reservation(1);
        assert!(queues.blocked_for(11).is_empty());
    }
}

//! Job runner (`SPEC.md` §8, `.claude/skills/add-job`).
//!
//! A job is a list of named [`Step`]s for one target. [`Jobs::submit`] records it and returns
//! at once; a tokio task runs the steps in order. If a step fails, the steps that already
//! completed are compensated in reverse order. Every change is persisted to SQLite and
//! broadcast as an event for `GET /jobs/{id}/events`.
//!
//! Only one job may be active per target; [`Jobs::submit`] returns [`TargetBusy`] otherwise.

pub mod container;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use arilxc::ContainerBackend;
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use time::OffsetDateTime;
use tokio::sync::broadcast;
use uuid::Uuid;

/// Most log lines kept per step; later lines are dropped (a note says so).
const MAX_LOG_LINES: usize = 1000;

/// Kinds of job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// `POST /containers`
    ContainerCreate,
    /// `POST /containers/{name}/start`
    ContainerStart,
    /// `POST /containers/{name}/stop`
    ContainerStop,
    /// `POST /containers/{name}/restart`
    ContainerRestart,
    /// `DELETE /containers/{name}`
    ContainerDelete,
}

/// Job status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    /// Accepted, not started.
    Queued,
    /// Running steps (or compensating).
    Running,
    /// All steps succeeded.
    Succeeded,
    /// A step failed; completed steps were compensated.
    Failed,
    /// Cancelled (reserved; v1 has no cancel endpoint).
    Cancelled,
}

impl JobStatus {
    fn as_str(self) -> &'static str {
        match self {
            JobStatus::Queued => "queued",
            JobStatus::Running => "running",
            JobStatus::Succeeded => "succeeded",
            JobStatus::Failed => "failed",
            JobStatus::Cancelled => "cancelled",
        }
    }
}

/// Step status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    /// Not reached.
    Pending,
    /// Running.
    Running,
    /// Done.
    Succeeded,
    /// This step failed.
    Failed,
    /// Undone after a later failure.
    Compensated,
    /// Undo failed; something may be left behind.
    CompensationFailed,
}

/// A log line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogLine {
    /// When.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// Text (English, no secrets).
    pub message: String,
}

/// A step's progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepRecord {
    /// Stable id, e.g. `create_container`.
    pub id: String,
    /// English label.
    pub label: String,
    /// Status.
    pub status: StepStatus,
    /// Start time.
    #[serde(with = "time::serde::rfc3339::option")]
    pub started_at: Option<OffsetDateTime>,
    /// End time.
    #[serde(with = "time::serde::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
    /// Log lines.
    pub log: Vec<LogLine>,
}

/// Why a job failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobError {
    /// Stable code, e.g. `backend_error`, `network_timeout`, `daemon_restarted`.
    pub code: String,
    /// English message.
    pub message: String,
    /// Id of the failing step.
    pub step: Option<String>,
    /// Some compensation failed, so leftovers may exist.
    pub rollback_incomplete: bool,
}

/// A job as stored and returned by the API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRecord {
    /// UUIDv7.
    pub id: Uuid,
    /// Kind.
    pub kind: JobKind,
    /// Target (container name).
    pub target: String,
    /// Status.
    pub status: JobStatus,
    /// Steps in order.
    pub steps: Vec<StepRecord>,
    /// Set when failed.
    pub error: Option<JobError>,
    /// Creation time.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Start time.
    #[serde(with = "time::serde::rfc3339::option")]
    pub started_at: Option<OffsetDateTime>,
    /// End time.
    #[serde(with = "time::serde::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
}

/// Progress events (`GET /jobs/{id}/events`). The SSE event name is the `type`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JobEvent {
    /// A step began.
    StepStarted {
        /// Step id.
        step: String,
    },
    /// A log line from a step.
    Log {
        /// Step id.
        step: String,
        /// Line.
        #[serde(flatten)]
        line: LogLine,
    },
    /// A step reached a final state (including compensation).
    StepFinished {
        /// Step id.
        step: String,
        /// Its status.
        status: StepStatus,
    },
    /// The job finished; no more events follow.
    JobFinished {
        /// Final status.
        status: JobStatus,
        /// Error if failed.
        error: Option<JobError>,
    },
}

impl JobEvent {
    /// SSE event name.
    pub fn name(&self) -> &'static str {
        match self {
            JobEvent::StepStarted { .. } => "step_started",
            JobEvent::Log { .. } => "log",
            JobEvent::StepFinished { .. } => "step_finished",
            JobEvent::JobFinished { .. } => "job_finished",
        }
    }
}

/// A step failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepError {
    /// Stable code.
    pub code: &'static str,
    /// English message for the UI (no stderr or paths).
    pub message: String,
}

impl StepError {
    /// Builds an error.
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Context for steps: backends and a logger for the current step.
pub struct JobCtx {
    /// Container backend.
    pub containers: Arc<dyn ContainerBackend>,
    jobs: Arc<JobsInner>,
    job_id: Uuid,
    step: usize,
}

impl JobCtx {
    /// Records a log line against the current step and broadcasts it.
    pub fn log(&self, message: impl Into<String>) {
        let line = LogLine {
            at: OffsetDateTime::now_utc(),
            message: message.into(),
        };
        self.jobs.update(self.job_id, |live| {
            let step = &mut live.record.steps[self.step];
            if step.log.len() < MAX_LOG_LINES {
                step.log.push(line.clone());
            } else if step.log.len() == MAX_LOG_LINES {
                step.log.push(LogLine {
                    at: line.at,
                    message: "(further log lines dropped)".into(),
                });
            }
            Some(JobEvent::Log {
                step: step.id.clone(),
                line,
            })
        });
    }
}

/// One unit of work in a job.
#[async_trait::async_trait]
pub trait Step: Send + Sync {
    /// Stable machine id, e.g. `create_container`. Never changes once shipped.
    fn id(&self) -> &'static str;
    /// English label for the UI.
    fn label(&self) -> &'static str;
    /// Does the work.
    async fn run(&self, ctx: &mut JobCtx) -> Result<(), StepError>;
    /// Undoes [`Step::run`] after a later step failed. Must tolerate partial state.
    async fn compensate(&self, _ctx: &mut JobCtx) -> Result<(), StepError> {
        Ok(())
    }
}

/// A job to submit.
pub struct JobSpec {
    /// Kind.
    pub kind: JobKind,
    /// Target; one active job per target.
    pub target: String,
    /// Steps in order.
    pub steps: Vec<Box<dyn Step>>,
}

/// Another job is already active for this target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetBusy {
    /// The active job.
    pub job_id: Uuid,
}

struct Live {
    record: JobRecord,
    tx: broadcast::Sender<JobEvent>,
}

#[derive(Default)]
struct State {
    /// target → active job id
    active: HashMap<String, Uuid>,
    /// jobs that have not finished yet
    live: HashMap<Uuid, Live>,
}

struct JobsInner {
    db: SqlitePool,
    state: Mutex<State>,
}

impl JobsInner {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Mutates a live job and broadcasts the event `f` returns.
    fn update(&self, id: Uuid, f: impl FnOnce(&mut Live) -> Option<JobEvent>) {
        let mut state = self.lock();
        if let Some(live) = state.live.get_mut(&id)
            && let Some(event) = f(live)
        {
            // No subscribers is fine.
            let _ = live.tx.send(event);
        }
    }

    fn snapshot(&self, id: Uuid) -> Option<JobRecord> {
        self.lock().live.get(&id).map(|l| l.record.clone())
    }

    async fn persist(&self, id: Uuid) {
        let Some(record) = self.snapshot(id) else {
            return;
        };
        if let Err(e) = save(&self.db, &record).await {
            tracing::error!(job_id = %id, error = %e, "persisting job");
        }
    }
}

/// The job runner. Cheap to clone.
#[derive(Clone)]
pub struct Jobs {
    inner: Arc<JobsInner>,
    containers: Arc<dyn ContainerBackend>,
    /// How long `wait_for_network` waits for an address.
    pub network_timeout: Duration,
}

impl Jobs {
    /// A runner backed by `db`.
    pub fn new(db: SqlitePool, containers: Arc<dyn ContainerBackend>) -> Self {
        Self {
            inner: Arc::new(JobsInner {
                db,
                state: Mutex::new(State::default()),
            }),
            containers,
            network_timeout: Duration::from_secs(60),
        }
    }

    /// Marks jobs left `queued`/`running` by a previous daemon as failed. Call once at start.
    pub async fn mark_interrupted(&self) -> Result<u64, sqlx::Error> {
        let error = serde_json::json!({
            "code": "daemon_restarted",
            "message": "The daemon restarted while this job was running",
            "step": null,
            "rollback_incomplete": true,
        });
        let result = sqlx::query(
            "UPDATE jobs SET status = 'failed', error = ?, finished_at = ? \
             WHERE status IN ('queued', 'running')",
        )
        .bind(error.to_string())
        .bind(rfc3339(OffsetDateTime::now_utc()))
        .execute(&self.inner.db)
        .await?;
        Ok(result.rows_affected())
    }

    /// Records and starts a job. Fails if the target already has an active job.
    pub async fn submit(&self, spec: JobSpec) -> Result<Uuid, TargetBusy> {
        let id = Uuid::now_v7();
        let record = JobRecord {
            id,
            kind: spec.kind,
            target: spec.target.clone(),
            status: JobStatus::Queued,
            steps: spec
                .steps
                .iter()
                .map(|s| StepRecord {
                    id: s.id().to_owned(),
                    label: s.label().to_owned(),
                    status: StepStatus::Pending,
                    started_at: None,
                    finished_at: None,
                    log: Vec::new(),
                })
                .collect(),
            error: None,
            created_at: OffsetDateTime::now_utc(),
            started_at: None,
            finished_at: None,
        };
        {
            let mut state = self.inner.lock();
            if let Some(&job_id) = state.active.get(&spec.target) {
                return Err(TargetBusy { job_id });
            }
            state.active.insert(spec.target.clone(), id);
            let (tx, _) = broadcast::channel(256);
            state.live.insert(id, Live { record, tx });
        }
        self.inner.persist(id).await;

        let inner = self.inner.clone();
        let containers = self.containers.clone();
        let target = spec.target;
        let steps = spec.steps;
        tokio::spawn(async move {
            // Run in a child task so a panicking step still finishes the job.
            let worker = tokio::spawn(run(inner.clone(), containers, id, steps));
            if let Err(e) = worker.await {
                tracing::error!(job_id = %id, error = %e, "job task panicked");
                finish(
                    &inner,
                    id,
                    JobStatus::Failed,
                    Some(JobError {
                        code: "internal_error".into(),
                        message: "Internal error; see the server log".into(),
                        step: None,
                        rollback_incomplete: true,
                    }),
                )
                .await;
            }
            let mut state = inner.lock();
            state.live.remove(&id);
            if state.active.get(&target) == Some(&id) {
                state.active.remove(&target);
            }
        });
        Ok(id)
    }

    /// A job by id (live state if still running).
    pub async fn get(&self, id: Uuid) -> Result<Option<JobRecord>, sqlx::Error> {
        if let Some(record) = self.inner.snapshot(id) {
            return Ok(Some(record));
        }
        load(&self.inner.db, id).await
    }

    /// Most recent jobs first.
    pub async fn list(&self, limit: u32) -> Result<Vec<JobRecord>, sqlx::Error> {
        let rows = sqlx::query("SELECT * FROM jobs ORDER BY id DESC LIMIT ?")
            .bind(i64::from(limit))
            .fetch_all(&self.inner.db)
            .await?;
        let state = self.inner.lock();
        rows.iter()
            .map(|row| {
                let record = from_row(row)?;
                Ok(state
                    .live
                    .get(&record.id)
                    .map_or(record, |l| l.record.clone()))
            })
            .collect()
    }

    /// The job's current state plus a receiver for later events (`None` once finished).
    /// Taken under one lock, so no event between the two is lost.
    pub async fn subscribe(
        &self,
        id: Uuid,
    ) -> Result<Option<(JobRecord, Option<broadcast::Receiver<JobEvent>>)>, sqlx::Error> {
        {
            let state = self.inner.lock();
            if let Some(live) = state.live.get(&id) {
                return Ok(Some((live.record.clone(), Some(live.tx.subscribe()))));
            }
        }
        Ok(load(&self.inner.db, id).await?.map(|r| (r, None)))
    }

    /// Whether `target` has an active job.
    pub fn is_busy(&self, target: &str) -> bool {
        self.inner.lock().active.contains_key(target)
    }
}

async fn run(
    inner: Arc<JobsInner>,
    containers: Arc<dyn ContainerBackend>,
    id: Uuid,
    steps: Vec<Box<dyn Step>>,
) {
    inner.update(id, |live| {
        live.record.status = JobStatus::Running;
        live.record.started_at = Some(OffsetDateTime::now_utc());
        None
    });
    inner.persist(id).await;

    let mut ctx = JobCtx {
        containers,
        jobs: inner.clone(),
        job_id: id,
        step: 0,
    };
    let mut completed = Vec::new();
    let mut failure = None;

    for (i, step) in steps.iter().enumerate() {
        ctx.step = i;
        set_step(&inner, id, i, StepStatus::Running, true);
        inner.persist(id).await;
        match step.run(&mut ctx).await {
            Ok(()) => {
                set_step(&inner, id, i, StepStatus::Succeeded, false);
                completed.push(i);
            }
            Err(e) => {
                tracing::warn!(job_id = %id, step = step.id(), code = e.code, "step failed");
                ctx.log(e.message.clone());
                set_step(&inner, id, i, StepStatus::Failed, false);
                failure = Some((step.id(), e));
            }
        }
        inner.persist(id).await;
        if failure.is_some() {
            break;
        }
    }

    let Some((failed_step, error)) = failure else {
        finish(&inner, id, JobStatus::Succeeded, None).await;
        return;
    };

    let mut rollback_incomplete = false;
    for &i in completed.iter().rev() {
        ctx.step = i;
        let status = match steps[i].compensate(&mut ctx).await {
            Ok(()) => StepStatus::Compensated,
            Err(e) => {
                // Keep going: other compensations may still succeed.
                tracing::error!(job_id = %id, step = steps[i].id(), code = e.code, "compensation failed");
                ctx.log(format!("Rollback failed: {}", e.message));
                rollback_incomplete = true;
                StepStatus::CompensationFailed
            }
        };
        set_step(&inner, id, i, status, false);
        inner.persist(id).await;
    }

    finish(
        &inner,
        id,
        JobStatus::Failed,
        Some(JobError {
            code: error.code.into(),
            message: error.message,
            step: Some(failed_step.into()),
            rollback_incomplete,
        }),
    )
    .await;
}

fn set_step(inner: &JobsInner, id: Uuid, i: usize, status: StepStatus, starting: bool) {
    inner.update(id, |live| {
        let now = OffsetDateTime::now_utc();
        let step = &mut live.record.steps[i];
        step.status = status;
        if starting {
            step.started_at = Some(now);
            Some(JobEvent::StepStarted {
                step: step.id.clone(),
            })
        } else {
            step.finished_at = Some(now);
            Some(JobEvent::StepFinished {
                step: step.id.clone(),
                status,
            })
        }
    });
}

async fn finish(inner: &JobsInner, id: Uuid, status: JobStatus, error: Option<JobError>) {
    // Persist first so a client that reconnects after `job_finished` sees the final row.
    inner.update(id, |live| {
        live.record.status = status;
        live.record.error = error.clone();
        live.record.finished_at = Some(OffsetDateTime::now_utc());
        None
    });
    inner.persist(id).await;
    inner.update(id, |_| Some(JobEvent::JobFinished { status, error }));
}

// ---- persistence ----------------------------------------------------------------------------

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

fn parse_time(s: Option<String>) -> Result<Option<OffsetDateTime>, sqlx::Error> {
    s.map(|s| {
        OffsetDateTime::parse(&s, &time::format_description::well_known::Rfc3339)
            .map_err(|e| sqlx::Error::Decode(Box::new(e)))
    })
    .transpose()
}

fn json_err(e: serde_json::Error) -> sqlx::Error {
    sqlx::Error::Decode(Box::new(e))
}

async fn save(db: &SqlitePool, r: &JobRecord) -> Result<(), sqlx::Error> {
    let kind = serde_json::to_value(r.kind).map_err(json_err)?;
    sqlx::query(
        "INSERT INTO jobs (id, kind, target, status, steps, error, created_at, started_at, finished_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT (id) DO UPDATE SET status = excluded.status, steps = excluded.steps, \
         error = excluded.error, started_at = excluded.started_at, finished_at = excluded.finished_at",
    )
    .bind(r.id.to_string())
    .bind(kind.as_str().unwrap_or_default())
    .bind(&r.target)
    .bind(r.status.as_str())
    .bind(serde_json::to_string(&r.steps).map_err(json_err)?)
    .bind(
        r.error
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(json_err)?,
    )
    .bind(rfc3339(r.created_at))
    .bind(r.started_at.map(rfc3339))
    .bind(r.finished_at.map(rfc3339))
    .execute(db)
    .await?;
    Ok(())
}

async fn load(db: &SqlitePool, id: Uuid) -> Result<Option<JobRecord>, sqlx::Error> {
    let row = sqlx::query("SELECT * FROM jobs WHERE id = ?")
        .bind(id.to_string())
        .fetch_optional(db)
        .await?;
    row.as_ref().map(from_row).transpose()
}

fn from_row(row: &sqlx::sqlite::SqliteRow) -> Result<JobRecord, sqlx::Error> {
    let id: String = row.try_get("id")?;
    let kind: String = row.try_get("kind")?;
    let status: String = row.try_get("status")?;
    let steps: String = row.try_get("steps")?;
    let error: Option<String> = row.try_get("error")?;
    Ok(JobRecord {
        id: id.parse().map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
        kind: serde_json::from_value(kind.into()).map_err(json_err)?,
        target: row.try_get("target")?,
        status: serde_json::from_value(status.into()).map_err(json_err)?,
        steps: serde_json::from_str(&steps).map_err(json_err)?,
        error: error
            .map(|e| serde_json::from_str(&e))
            .transpose()
            .map_err(json_err)?,
        created_at: parse_time(row.try_get("created_at")?)?.unwrap_or(OffsetDateTime::UNIX_EPOCH),
        started_at: parse_time(row.try_get("started_at")?)?,
        finished_at: parse_time(row.try_get("finished_at")?)?,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use arilxc::MockBackend;

    use super::*;

    /// Shared log of what test steps did, in order.
    pub(crate) type Trace = Arc<Mutex<Vec<String>>>;

    struct TestStep {
        id: &'static str,
        trace: Trace,
        fail_run: bool,
        fail_compensate: bool,
        release: Option<Arc<tokio::sync::Notify>>,
    }

    #[async_trait::async_trait]
    impl Step for TestStep {
        fn id(&self) -> &'static str {
            self.id
        }
        fn label(&self) -> &'static str {
            "Test step"
        }
        async fn run(&self, ctx: &mut JobCtx) -> Result<(), StepError> {
            if let Some(n) = &self.release {
                n.notified().await;
            }
            ctx.log(format!("running {}", self.id));
            self.trace.lock().unwrap().push(format!("run {}", self.id));
            if self.fail_run {
                Err(StepError::new(
                    "test_failure",
                    format!("{} failed", self.id),
                ))
            } else {
                Ok(())
            }
        }
        async fn compensate(&self, _ctx: &mut JobCtx) -> Result<(), StepError> {
            self.trace.lock().unwrap().push(format!("undo {}", self.id));
            if self.fail_compensate {
                Err(StepError::new("undo_failure", "could not undo"))
            } else {
                Ok(())
            }
        }
    }

    fn step(id: &'static str, trace: &Trace) -> TestStep {
        TestStep {
            id,
            trace: trace.clone(),
            fail_run: false,
            fail_compensate: false,
            release: None,
        }
    }

    pub(crate) async fn jobs() -> Jobs {
        Jobs::new(
            crate::db::memory().await.unwrap(),
            Arc::new(MockBackend::new()),
        )
    }

    /// Waits until the job is no longer queued/running.
    pub(crate) async fn wait(jobs: &Jobs, id: Uuid) -> JobRecord {
        for _ in 0..500 {
            let r = jobs.get(id).await.unwrap().unwrap();
            if matches!(r.status, JobStatus::Succeeded | JobStatus::Failed)
                && !jobs.is_busy(&r.target)
            {
                return r;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("job {id} did not finish");
    }

    fn spec(target: &str, steps: Vec<TestStep>) -> JobSpec {
        JobSpec {
            kind: JobKind::ContainerCreate,
            target: target.into(),
            steps: steps
                .into_iter()
                .map(|s| Box::new(s) as Box<dyn Step>)
                .collect(),
        }
    }

    #[tokio::test]
    async fn happy_path_runs_steps_in_order_and_emits_events() {
        let jobs = jobs().await;
        let trace = Trace::default();
        let gate = Arc::new(tokio::sync::Notify::new());
        let mut first = step("a", &trace);
        first.release = Some(gate.clone());
        let id = jobs
            .submit(spec(
                "web",
                vec![first, step("b", &trace), step("c", &trace)],
            ))
            .await
            .unwrap();

        // Subscribe while the first step is blocked, then let it go.
        let (snapshot, rx) = jobs.subscribe(id).await.unwrap().unwrap();
        assert!(matches!(
            snapshot.status,
            JobStatus::Queued | JobStatus::Running
        ));
        let mut rx = rx.unwrap();
        gate.notify_one();

        let mut names = Vec::new();
        loop {
            let event = rx.recv().await.unwrap();
            names.push(event.name());
            if let JobEvent::JobFinished { status, .. } = event {
                assert_eq!(status, JobStatus::Succeeded);
                break;
            }
        }
        assert_eq!(
            names,
            [
                "step_started",
                "log",
                "step_finished",
                "step_started",
                "log",
                "step_finished",
                "step_started",
                "log",
                "step_finished",
                "job_finished"
            ]
        );
        assert_eq!(*trace.lock().unwrap(), ["run a", "run b", "run c"]);

        let done = wait(&jobs, id).await;
        assert!(done.steps.iter().all(|s| s.status == StepStatus::Succeeded));
        assert!(done.started_at.is_some() && done.finished_at.is_some());
        assert_eq!(done.steps[0].log[0].message, "running a");
        // Persisted: a fresh read from SQLite matches.
        assert_eq!(load(&jobs.inner.db, id).await.unwrap().unwrap(), done);
    }

    #[tokio::test]
    async fn failure_at_each_step_compensates_completed_steps_in_reverse() {
        let ids = ["a", "b", "c", "d"];
        for fail_at in 0..ids.len() {
            let jobs = jobs().await;
            let trace = Trace::default();
            let steps = ids
                .iter()
                .enumerate()
                .map(|(i, id)| {
                    let mut s = step(id, &trace);
                    s.fail_run = i == fail_at;
                    s
                })
                .collect();
            let job = wait(&jobs, jobs.submit(spec("web", steps)).await.unwrap()).await;

            let mut expected: Vec<String> = ids[..=fail_at]
                .iter()
                .map(|id| format!("run {id}"))
                .collect();
            expected.extend(ids[..fail_at].iter().rev().map(|id| format!("undo {id}")));
            assert_eq!(*trace.lock().unwrap(), expected, "fail at {fail_at}");

            assert_eq!(job.status, JobStatus::Failed);
            let err = job.error.unwrap();
            assert_eq!(
                (err.code.as_str(), err.step.as_deref()),
                ("test_failure", Some(ids[fail_at]))
            );
            assert!(!err.rollback_incomplete);
            for (i, s) in job.steps.iter().enumerate() {
                let want = match i.cmp(&fail_at) {
                    std::cmp::Ordering::Less => StepStatus::Compensated,
                    std::cmp::Ordering::Equal => StepStatus::Failed,
                    std::cmp::Ordering::Greater => StepStatus::Pending,
                };
                assert_eq!(s.status, want, "fail at {fail_at}, step {i}");
            }
        }
    }

    #[tokio::test]
    async fn compensation_failure_is_recorded_and_others_still_run() {
        let jobs = jobs().await;
        let trace = Trace::default();
        let a = step("a", &trace);
        let mut b = step("b", &trace);
        b.fail_compensate = true;
        let mut c = step("c", &trace);
        c.fail_run = true;
        let job = wait(
            &jobs,
            jobs.submit(spec("web", vec![a, b, c])).await.unwrap(),
        )
        .await;

        assert_eq!(
            *trace.lock().unwrap(),
            ["run a", "run b", "run c", "undo b", "undo a"]
        );
        assert!(job.error.unwrap().rollback_incomplete);
        assert_eq!(job.steps[1].status, StepStatus::CompensationFailed);
        assert_eq!(job.steps[0].status, StepStatus::Compensated);
    }

    #[tokio::test]
    async fn one_active_job_per_target() {
        let jobs = jobs().await;
        let trace = Trace::default();
        let gate = Arc::new(tokio::sync::Notify::new());
        let mut blocked = step("a", &trace);
        blocked.release = Some(gate.clone());
        let first = jobs.submit(spec("web", vec![blocked])).await.unwrap();

        let busy = jobs.submit(spec("web", vec![step("x", &trace)])).await;
        assert_eq!(busy, Err(TargetBusy { job_id: first }));
        // Other targets are independent.
        let other = jobs
            .submit(spec("db", vec![step("y", &trace)]))
            .await
            .unwrap();
        wait(&jobs, other).await;

        gate.notify_one();
        wait(&jobs, first).await;
        // Free again once finished.
        assert!(
            jobs.submit(spec("web", vec![step("z", &trace)]))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn panicking_step_fails_the_job_and_frees_the_target() {
        struct Boom;
        #[async_trait::async_trait]
        impl Step for Boom {
            fn id(&self) -> &'static str {
                "boom"
            }
            fn label(&self) -> &'static str {
                "Boom"
            }
            async fn run(&self, _ctx: &mut JobCtx) -> Result<(), StepError> {
                panic!("boom");
            }
        }
        let jobs = jobs().await;
        let id = jobs
            .submit(JobSpec {
                kind: JobKind::ContainerStart,
                target: "web".into(),
                steps: vec![Box::new(Boom)],
            })
            .await
            .unwrap();
        let job = wait(&jobs, id).await;
        assert_eq!(job.error.unwrap().code, "internal_error");
        assert!(!jobs.is_busy("web"));
    }

    #[tokio::test]
    async fn interrupted_jobs_are_marked_failed() {
        let jobs = jobs().await;
        // Simulate a row left by a crashed daemon.
        let mut record = wait(&jobs, jobs.submit(spec("web", vec![])).await.unwrap()).await;
        record.id = Uuid::now_v7();
        record.status = JobStatus::Running;
        record.finished_at = None;
        save(&jobs.inner.db, &record).await.unwrap();

        assert_eq!(jobs.mark_interrupted().await.unwrap(), 1);
        let after = jobs.get(record.id).await.unwrap().unwrap();
        assert_eq!(after.status, JobStatus::Failed);
        assert_eq!(after.error.unwrap().code, "daemon_restarted");
    }

    #[tokio::test]
    async fn list_is_newest_first_and_limited() {
        let jobs = jobs().await;
        let mut ids = Vec::new();
        for t in ["a", "b", "c"] {
            ids.push(jobs.submit(spec(t, vec![])).await.unwrap());
        }
        for id in &ids {
            wait(&jobs, *id).await;
        }
        let listed: Vec<Uuid> = jobs.list(2).await.unwrap().iter().map(|j| j.id).collect();
        assert_eq!(listed, [ids[2], ids[1]]);
    }

    #[test]
    fn event_json_shape() {
        let e = JobEvent::StepFinished {
            step: "start_container".into(),
            status: StepStatus::Succeeded,
        };
        assert_eq!(
            serde_json::to_value(&e).unwrap(),
            serde_json::json!({ "type": "step_finished", "step": "start_container", "status": "succeeded" })
        );
    }
}

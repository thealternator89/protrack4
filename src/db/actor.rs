use std::collections::VecDeque;
use std::future::poll_fn;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::task::{Poll, Waker};
use std::thread::{self, JoinHandle};

use crossbeam_channel::{Receiver, Sender, unbounded};
use rusqlite::{Connection, TransactionBehavior};

use crate::commands::{CommandEffect, UndoableCommand};
use crate::db::migrations::{checkpoint_and_switch_to_delete_mode, seed_sample_project};
use crate::db::{SearchResultItem, load_project_snapshot, load_workspace_overview, map_sqlite_err};
use crate::domain::{
    DomainError, DomainEvent, ProjectId, ProjectSnapshot, WorkspaceOverview,
};

pub enum DbRequest {
    LoadWorkspaceOverview {
        preferred_project_id: Option<ProjectId>,
        reply: oneshot::Sender<Result<WorkspaceOverview, DomainError>>,
    },
    LoadProjectSnapshot {
        project_id: ProjectId,
        reply: oneshot::Sender<Result<ProjectSnapshot, DomainError>>,
    },
    SearchItemsFts {
        query: String,
        project_id: Option<ProjectId>,
        limit: usize,
        reply: oneshot::Sender<Result<Vec<SearchResultItem>, DomainError>>,
    },
    SeedSampleProject {
        reply: oneshot::Sender<Result<ProjectId, DomainError>>,
    },
    ExecuteCommand {
        command: Box<dyn UndoableCommand>,
        reply: oneshot::Sender<Result<CommandEffect, DomainError>>,
    },
    Shutdown {
        reply: oneshot::Sender<Result<(), DomainError>>,
    },
}

#[derive(Default)]
struct SubscriberState {
    queue: VecDeque<DomainEvent>,
    waker: Option<Waker>,
    closed: bool,
}

#[derive(Clone)]
pub struct DomainEventReceiver {
    state: Arc<Mutex<SubscriberState>>,
}

impl DomainEventReceiver {
    pub async fn recv(&self) -> Option<DomainEvent> {
        let state = Arc::clone(&self.state);
        poll_fn(move |cx| {
            let mut guard = state.lock().expect("domain event state poisoned");
            if let Some(event) = guard.queue.pop_front() {
                return Poll::Ready(Some(event));
            }
            if guard.closed {
                return Poll::Ready(None);
            }
            guard.waker = Some(cx.waker().clone());
            Poll::Pending
        })
        .await
    }
}

#[derive(Default)]
struct DomainEventBus {
    subscribers: Mutex<Vec<Weak<Mutex<SubscriberState>>>>,
}

impl DomainEventBus {
    fn subscribe(&self) -> DomainEventReceiver {
        let state = Arc::new(Mutex::new(SubscriberState::default()));
        let mut subs = self.subscribers.lock().expect("event bus poisoned");
        subs.push(Arc::downgrade(&state));
        DomainEventReceiver { state }
    }

    fn emit(&self, event: DomainEvent) {
        let mut subs = self.subscribers.lock().expect("event bus poisoned");
        subs.retain(|weak| {
            let Some(arc) = weak.upgrade() else {
                return false;
            };
            let mut guard = arc.lock().expect("subscriber state poisoned");
            if guard.closed {
                return false;
            }
            guard.queue.push_back(event.clone());
            if let Some(waker) = guard.waker.take() {
                waker.wake();
            }
            true
        });
    }

    fn close_all(&self) {
        let mut subs = self.subscribers.lock().expect("event bus poisoned");
        for weak in subs.drain(..) {
            if let Some(arc) = weak.upgrade() {
                let mut guard = arc.lock().expect("subscriber state poisoned");
                guard.closed = true;
                if let Some(waker) = guard.waker.take() {
                    waker.wake();
                }
            }
        }
    }
}

struct DbServiceInner {
    tx: Sender<DbRequest>,
    bus: Arc<DomainEventBus>,
    shutdown_started: AtomicBool,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for DbServiceInner {
    fn drop(&mut self) {
        if !self.shutdown_started.swap(true, Ordering::SeqCst) {
            let (reply_tx, reply_rx) = oneshot::channel();
            if self.tx.send(DbRequest::Shutdown { reply: reply_tx }).is_ok() {
                let _ = reply_rx.recv();
            }
        }
        if let Ok(mut guard) = self.thread.lock()
            && let Some(handle) = guard.take()
        {
            let _ = handle.join();
        }
    }
}

#[derive(Clone)]
pub struct DbService {
    inner: Arc<DbServiceInner>,
}

impl DbService {
    pub fn spawn(conn: Connection) -> Result<Self, DomainError> {
        let (tx, rx): (Sender<DbRequest>, Receiver<DbRequest>) = unbounded();
        let bus = Arc::new(DomainEventBus::default());
        let actor_bus = Arc::clone(&bus);

        let handle = thread::Builder::new()
            .name("protrack-db-actor".to_string())
            .spawn(move || run_actor_loop(conn, rx, actor_bus))
            .map_err(|err| {
                DomainError::Storage(format!("failed to spawn DbService actor thread: {err}"))
            })?;

        Ok(Self {
            inner: Arc::new(DbServiceInner {
                tx,
                bus,
                shutdown_started: AtomicBool::new(false),
                thread: Mutex::new(Some(handle)),
            }),
        })
    }

    pub fn subscribe_events(&self) -> DomainEventReceiver {
        self.inner.bus.subscribe()
    }

    pub async fn load_workspace_overview(
        &self,
        preferred_project_id: Option<ProjectId>,
    ) -> Result<WorkspaceOverview, DomainError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.send_request(DbRequest::LoadWorkspaceOverview {
            preferred_project_id,
            reply: reply_tx,
        })?;
        await_oneshot(reply_rx).await
    }

    pub fn load_workspace_overview_blocking(
        &self,
        preferred_project_id: Option<ProjectId>,
    ) -> Result<WorkspaceOverview, DomainError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.send_request(DbRequest::LoadWorkspaceOverview {
            preferred_project_id,
            reply: reply_tx,
        })?;
        reply_rx
            .recv()
            .map_err(|_| DomainError::Storage("DbService actor terminated unexpectedly".into()))?
    }

    pub async fn load_project_snapshot(
        &self,
        project_id: ProjectId,
    ) -> Result<ProjectSnapshot, DomainError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.send_request(DbRequest::LoadProjectSnapshot {
            project_id,
            reply: reply_tx,
        })?;
        await_oneshot(reply_rx).await
    }

    pub async fn search_items_fts(
        &self,
        query: String,
        project_id: Option<ProjectId>,
        limit: usize,
    ) -> Result<Vec<SearchResultItem>, DomainError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.send_request(DbRequest::SearchItemsFts {
            query,
            project_id,
            limit,
            reply: reply_tx,
        })?;
        await_oneshot(reply_rx).await
    }

    pub fn search_items_fts_blocking(
        &self,
        query: String,
        project_id: Option<ProjectId>,
        limit: usize,
    ) -> Result<Vec<SearchResultItem>, DomainError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.send_request(DbRequest::SearchItemsFts {
            query,
            project_id,
            limit,
            reply: reply_tx,
        })?;
        reply_rx
            .recv()
            .map_err(|_| DomainError::Storage("DbService actor terminated unexpectedly".into()))?
    }

    pub async fn seed_sample_project(&self) -> Result<ProjectId, DomainError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.send_request(DbRequest::SeedSampleProject { reply: reply_tx })?;
        await_oneshot(reply_rx).await
    }

    pub fn seed_sample_project_blocking(&self) -> Result<ProjectId, DomainError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.send_request(DbRequest::SeedSampleProject { reply: reply_tx })?;
        reply_rx
            .recv()
            .map_err(|_| DomainError::Storage("DbService actor terminated unexpectedly".into()))?
    }

    pub async fn execute_command(
        &self,
        command: Box<dyn UndoableCommand>,
    ) -> Result<CommandEffect, DomainError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.send_request(DbRequest::ExecuteCommand {
            command,
            reply: reply_tx,
        })?;
        await_oneshot(reply_rx).await
    }

    pub fn execute_command_blocking(
        &self,
        command: Box<dyn UndoableCommand>,
    ) -> Result<CommandEffect, DomainError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.send_request(DbRequest::ExecuteCommand {
            command,
            reply: reply_tx,
        })?;
        reply_rx
            .recv()
            .map_err(|_| DomainError::Storage("DbService actor terminated unexpectedly".into()))?
    }

    pub fn shutdown(&self) -> Result<(), DomainError> {
        let result = if !self.inner.shutdown_started.swap(true, Ordering::SeqCst) {
            let (reply_tx, reply_rx) = oneshot::channel();
            if self.inner.tx.send(DbRequest::Shutdown { reply: reply_tx }).is_ok() {
                reply_rx.recv().unwrap_or(Ok(()))
            } else {
                Ok(())
            }
        } else {
            Ok(())
        };

        if let Ok(mut guard) = self.inner.thread.lock()
            && let Some(handle) = guard.take()
        {
            let _ = handle.join();
        }

        result
    }

    fn send_request(&self, req: DbRequest) -> Result<(), DomainError> {
        self.inner
            .tx
            .send(req)
            .map_err(|_| DomainError::Storage("DbService actor channel is closed".to_string()))
    }
}

async fn await_oneshot<T>(rx: oneshot::Receiver<Result<T, DomainError>>) -> Result<T, DomainError> {
    rx.await
        .map_err(|_| DomainError::Storage("DbService actor dropped response channel".to_string()))?
}

fn run_actor_loop(conn: Connection, rx: Receiver<DbRequest>, bus: Arc<DomainEventBus>) {
    let mut conn_opt = Some(conn);

    while let Ok(req) = rx.recv() {
        let Some(conn) = conn_opt.as_mut() else {
            break;
        };

        match req {
            DbRequest::LoadWorkspaceOverview {
                preferred_project_id,
                reply,
            } => {
                let res = load_workspace_overview(conn, preferred_project_id);
                let _ = reply.send(res);
            }
            DbRequest::LoadProjectSnapshot { project_id, reply } => {
                let res = load_project_snapshot(conn, project_id);
                let _ = reply.send(res);
            }
            DbRequest::SearchItemsFts {
                query,
                project_id,
                limit,
                reply,
            } => {
                let res = crate::db::item_repo::search_items_fts(conn, &query, project_id, limit);
                let _ = reply.send(res);
            }
            DbRequest::SeedSampleProject { reply } => {
                let res = seed_sample_project(conn);
                if let Ok(project_id) = res {
                    bus.emit(DomainEvent::ProjectsChanged);
                    bus.emit(DomainEvent::ItemsChanged { project_id });
                }
                let _ = reply.send(res);
            }
            DbRequest::ExecuteCommand { mut command, reply } => {
                let res = (|| -> Result<CommandEffect, DomainError> {
                    let tx = conn
                        .transaction_with_behavior(TransactionBehavior::Immediate)
                        .map_err(map_sqlite_err)?;
                    let effect = command.execute(&tx)?;
                    tx.commit().map_err(map_sqlite_err)?;
                    Ok(effect)
                })();
                if let Ok(effect) = &res {
                    for event in &effect.events {
                        bus.emit(event.clone());
                    }
                }
                let _ = reply.send(res);
            }
            DbRequest::Shutdown { reply } => {
                let res = if let Some(c) = conn_opt.take() {
                    let checkpoint_res = checkpoint_and_switch_to_delete_mode(&c);
                    drop(c);
                    checkpoint_res
                } else {
                    Ok(())
                };
                bus.close_all();
                let _ = reply.send(res);
                break;
            }
        }
    }

    if let Some(c) = conn_opt.take() {
        let _ = checkpoint_and_switch_to_delete_mode(&c);
        drop(c);
    }
    bus.close_all();
}

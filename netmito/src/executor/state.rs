//! Writes the detailed execution state of this task to Redis, and waits for the
//! state of other tasks.

use futures::StreamExt;
use redis::AsyncCommands;
use tokio::time::Instant;
use uuid::Uuid;

use crate::entity::state::TaskExecState;
use crate::error::ErrorMsg;
use crate::schema::*;

use super::Executor;

impl Executor {
    pub(super) async fn set_task_state_ex(&mut self, uuid: &Uuid, state: i32, ex: u64) {
        if let Some(ref mut conn) = self.task_redis_conn {
            tracing::trace!("Set task state: {} -> {}", uuid, state);
            let _: Result<String, _> = conn.set_ex(format!("task:{uuid}"), state, ex).await;
        }
    }

    pub(super) async fn set_task_state(&mut self, uuid: &Uuid, state: i32) {
        if let Some(ref mut conn) = self.task_redis_conn {
            tracing::trace!("Set task state: {} -> {}", uuid, state);
            let _: Result<String, _> = conn.set(format!("task:{uuid}"), state).await;
        }
    }

    pub(super) async fn get_task_state(&mut self, uuid: &Uuid) -> Option<TaskExecState> {
        if let Some(ref mut conn) = self.task_redis_conn {
            tracing::trace!("Get task state: {}", uuid);
            let state: Result<i32, _> = conn.get(format!("task:{uuid}")).await;
            state.ok().map(TaskExecState::from)
        } else {
            None
        }
    }

    pub(super) async fn publish_state(&mut self, uuid: &Uuid, state: i32) {
        if let Some(ref mut conn) = self.task_redis_conn {
            tracing::trace!("Publish task state: {} -> {}", uuid, state);
            let _: Result<i32, _> = conn.publish(format!("task:{uuid}"), state).await;
        }
    }

    pub(super) async fn announce_task_state(&mut self, uuid: &Uuid, state: i32) {
        self.set_task_state(uuid, state).await;
        self.publish_state(uuid, state).await;
    }

    pub(super) async fn announce_task_state_ex(&mut self, uuid: &Uuid, state: i32, ex: u64) {
        self.set_task_state_ex(uuid, state, ex).await;
        self.publish_state(uuid, state).await;
    }

    pub(super) async fn watch_task(&mut self, uuid: &Uuid, state: TaskExecState) {
        tracing::debug!("Watch task: {} -> {:?}", uuid, state);
        let mut wait_until = Instant::now();
        if let Some(pubsub) = self.task_redis_pubsub.as_mut() {
            let channel_name = format!("task:{uuid}");
            let _ = pubsub.subscribe(&channel_name).await;
            let mut stream = pubsub.on_message();
            loop {
                tokio::select! {
                    biased;
                    msg = stream.next() => {
                        if let Some(msg) = msg {
                            if msg.get_channel_name() == channel_name {
                                if let Ok(task_state) = msg.get_payload::<i32>() {
                                    let cur_state = TaskExecState::from(task_state);
                                    if cur_state.is_reach(&state) {
                                        break;
                                    }
                                }
                            }
                        }
                    },
                    _ = tokio::time::sleep_until(wait_until) => {
                        wait_until = Instant::now() + std::time::Duration::from_secs(30);
                        let cur_state = if let Some(ref mut conn) = self.task_redis_conn {
                            tracing::trace!("Get task state: {}", uuid);
                            let state: Result<i32, _> = conn.get(format!("task:{uuid}")).await;
                            state.ok().map(TaskExecState::from)
                        } else {
                            None
                        };
                        if let Some(cur_state) = cur_state {
                            if cur_state.is_reach(&state) {
                                break;
                            }
                        }
                        self.http_url
                            .set_path(format!("tasks/{uuid}").as_str());
                        let resp = self
                            .http_client
                            .get(self.http_url.as_str())
                            .bearer_auth(&self.task_credential)
                            .send()
                            .await;
                        match resp {
                            Ok(resp) => {
                                if resp.status().is_success() {
                                    if let Ok(task) = resp.json::<TaskQueryResp>().await {
                                        if task.info.state.is_reach(&state, task.info.result) {
                                            break;
                                        }
                                    }
                                } else {
                                    let resp: ErrorMsg = resp
                                        .json()
                                        .await
                                        .unwrap_or_else(|e| ErrorMsg { msg: e.to_string() });
                                    tracing::error!("Get Task failed with error: {}", resp.msg);
                                }
                            }
                            Err(e) => {
                                tracing::error!("Get task failed with error: {}", e);
                            }
                        }
                    },
                }
            }
        } else {
            loop {
                tokio::time::sleep_until(wait_until).await;
                wait_until = Instant::now() + std::time::Duration::from_secs(30);
                if let Some(cur_state) = self.get_task_state(uuid).await {
                    if cur_state.is_reach(&state) {
                        break;
                    }
                }
                self.http_url.set_path(format!("tasks/{uuid}").as_str());
                let resp = self
                    .http_client
                    .get(self.http_url.as_str())
                    .bearer_auth(&self.task_credential)
                    .send()
                    .await;
                match resp {
                    Ok(resp) => {
                        if resp.status().is_success() {
                            if let Ok(task) = resp.json::<TaskQueryResp>().await {
                                if task.info.state.is_reach(&state, task.info.result) {
                                    break;
                                }
                            }
                        } else {
                            let resp: ErrorMsg = resp
                                .json()
                                .await
                                .unwrap_or_else(|e| ErrorMsg { msg: e.to_string() });
                            tracing::error!("Get Task failed with error: {}", resp.msg);
                        }
                    }
                    Err(e) => {
                        tracing::error!("Get task failed with error: {}", e);
                    }
                }
            }
        }
    }

    pub async fn subscribe_task_exec_state(&mut self, uuid: &Uuid) {
        if let Some(pubsub) = self.task_redis_pubsub.as_mut() {
            let _ = pubsub.subscribe(format!("task:{uuid}")).await;
        }
    }

    pub async fn unsubscribe_task_exec_state(&mut self, uuid: &Uuid) {
        if let Some(pubsub) = self.task_redis_pubsub.as_mut() {
            let _ = pubsub.unsubscribe(format!("task:{uuid}")).await;
        }
    }
}

use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::StatusCode,
    response::IntoResponse,
};
use axum_extra::{headers::Authorization, TypedHeader};
use futures::{SinkExt, StreamExt};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set};
use uuid::Uuid;

use crate::{
    config::InfraPool,
    entity::state::NodeManagerState,
    service::auth::token::verify_token,
    websocket::{ConnectionRegistry, ManagerMessage},
};

/// State shared with WebSocket handlers
#[derive(Clone)]
pub struct WebSocketState {
    pub infra_pool: InfraPool,
    pub ws_registry: ConnectionRegistry,
}

/// WebSocket upgrade handler for manager connections
/// Validates JWT token and upgrades the connection
pub async fn websocket_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<WebSocketState>>,
    TypedHeader(auth): TypedHeader<Authorization<axum_extra::headers::authorization::Bearer>>,
) -> Result<impl IntoResponse, StatusCode> {
    // Verify JWT token
    let token = auth.token();
    let claims = verify_token(token).map_err(|e| {
        tracing::warn!(error = %e, "Invalid JWT token for WebSocket connection");
        StatusCode::UNAUTHORIZED
    })?;

    // Extract manager UUID from token subject
    // The subject should be the manager UUID as a string
    let manager_uuid = Uuid::parse_str(claims.sub.as_ref()).map_err(|e| {
        tracing::warn!(
            error = %e,
            subject = %claims.sub,
            "Invalid manager UUID in token"
        );
        StatusCode::UNAUTHORIZED
    })?;

    tracing::info!(
        manager_uuid = %manager_uuid,
        "WebSocket upgrade request authorized"
    );

    // Upgrade to WebSocket
    Ok(ws.on_upgrade(move |socket| handle_manager_socket(socket, manager_uuid, state)))
}

/// Handle an individual manager WebSocket connection
async fn handle_manager_socket(socket: WebSocket, manager_uuid: Uuid, state: Arc<WebSocketState>) {
    let (sender, mut receiver) = socket.split();
    let sender = Arc::new(tokio::sync::RwLock::new(sender));

    // Register connection
    state.ws_registry.register(manager_uuid, sender.clone()).await;

    // Handle incoming messages
    while let Some(msg) = receiver.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                tracing::debug!(
                    manager_uuid = %manager_uuid,
                    message_length = text.len(),
                    "Received text message"
                );

                if let Err(e) = handle_manager_message(&text, manager_uuid, &state).await {
                    tracing::error!(
                        manager_uuid = %manager_uuid,
                        error = %e,
                        "Error handling manager message"
                    );
                }
            }
            Ok(Message::Close(frame)) => {
                tracing::info!(
                    manager_uuid = %manager_uuid,
                    reason = ?frame,
                    "Manager closed connection"
                );
                break;
            }
            Ok(Message::Ping(data)) => {
                if let Err(e) = sender.write().await.send(Message::Pong(data)).await {
                    tracing::error!(
                        manager_uuid = %manager_uuid,
                        error = %e,
                        "Failed to send pong"
                    );
                    break;
                }
            }
            Ok(Message::Pong(_)) => {
                // Ignore pong messages
            }
            Ok(Message::Binary(_)) => {
                tracing::warn!(
                    manager_uuid = %manager_uuid,
                    "Received unexpected binary message"
                );
            }
            Err(e) => {
                tracing::error!(
                    manager_uuid = %manager_uuid,
                    error = %e,
                    "WebSocket error"
                );
                break;
            }
        }
    }

    // Cleanup on disconnect
    state.ws_registry.unregister(&manager_uuid).await;

    // Update manager state in database to Offline
    if let Err(e) = mark_manager_offline(&state.infra_pool, manager_uuid).await {
        tracing::error!(
            manager_uuid = %manager_uuid,
            error = %e,
            "Failed to mark manager as offline"
        );
    }
}

/// Route incoming manager messages to appropriate handlers
async fn handle_manager_message(
    text: &str,
    manager_uuid: Uuid,
    state: &Arc<WebSocketState>,
) -> crate::error::Result<()> {
    let message: ManagerMessage = serde_json::from_str(text)?;

    tracing::debug!(
        manager_uuid = %manager_uuid,
        message_type = ?message,
        "Routing message"
    );

    match message {
        ManagerMessage::Heartbeat {
            manager_uuid: msg_uuid,
            state: mgr_state,
            metrics,
        } => {
            // Verify manager UUID matches
            if msg_uuid != manager_uuid {
                tracing::warn!(
                    expected = %manager_uuid,
                    received = %msg_uuid,
                    "Manager UUID mismatch in heartbeat"
                );
                return Err(crate::error::Error::Custom(
                    "Manager UUID mismatch".to_string(),
                ));
            }
            handle_heartbeat(manager_uuid, mgr_state, metrics, state).await?;
        }
        ManagerMessage::FetchTask {
            request_id,
            worker_local_id,
        } => {
            handle_fetch_task(manager_uuid, request_id, worker_local_id, state).await?;
        }
        ManagerMessage::ReportTask {
            request_id,
            task_id,
            op,
        } => {
            handle_report_task(manager_uuid, request_id, task_id, op, state).await?;
        }
        ManagerMessage::ReportFailure {
            task_uuid,
            failure_count,
            error_message,
            worker_local_id,
        } => {
            handle_report_failure(
                manager_uuid,
                task_uuid,
                failure_count,
                error_message,
                worker_local_id,
                state,
            )
            .await?;
        }
        ManagerMessage::AbortTask { task_uuid, reason } => {
            handle_abort_task(manager_uuid, task_uuid, reason, state).await?;
        }
        ManagerMessage::SuiteCompleted {
            suite_uuid,
            tasks_completed,
            tasks_failed,
        } => {
            handle_suite_completed(
                manager_uuid,
                suite_uuid,
                tasks_completed,
                tasks_failed,
                state,
            )
            .await?;
        }
    }

    Ok(())
}

/// Handle heartbeat message - updates manager state and last_heartbeat timestamp
async fn handle_heartbeat(
    manager_uuid: Uuid,
    state: NodeManagerState,
    metrics: crate::websocket::messages::ManagerMetrics,
    ws_state: &Arc<WebSocketState>,
) -> crate::error::Result<()> {
    use sea_orm::sea_query::Expr;

    tracing::debug!(
        manager_uuid = %manager_uuid,
        state = ?state,
        active_workers = metrics.active_workers,
        "Processing heartbeat"
    );

    let db = &ws_state.infra_pool.db;

    // Update manager state and last_heartbeat
    use crate::entity::node_managers;

    let now = time::OffsetDateTime::now_utc();

    node_managers::Entity::update_many()
        .col_expr(node_managers::Column::State, Expr::value(state))
        .col_expr(node_managers::Column::LastHeartbeat, Expr::value(now))
        .col_expr(node_managers::Column::UpdatedAt, Expr::value(now))
        .filter(node_managers::Column::Uuid.eq(manager_uuid))
        .exec(db)
        .await?;

    Ok(())
}

/// Handle fetch task request - returns a task from the manager's assigned suite
async fn handle_fetch_task(
    manager_uuid: Uuid,
    request_id: u64,
    _worker_local_id: u32,
    ws_state: &Arc<WebSocketState>,
) -> crate::error::Result<()> {
    tracing::debug!(
        manager_uuid = %manager_uuid,
        request_id = request_id,
        "Processing fetch task request"
    );

    // TODO: Implement task fetching logic
    // For now, just return None to indicate no tasks available
    let task = None;

    // Send response back
    let response = crate::websocket::CoordinatorMessage::TaskAvailable { request_id, task };

    ws_state
        .ws_registry
        .send_to_manager(&manager_uuid, &response)
        .await?;

    Ok(())
}

/// Handle task report - processes task completion, cancellation, commit, or upload
async fn handle_report_task(
    manager_uuid: Uuid,
    request_id: u64,
    task_id: i64,
    op: crate::schema::ReportTaskOp,
    ws_state: &Arc<WebSocketState>,
) -> crate::error::Result<()> {
    tracing::debug!(
        manager_uuid = %manager_uuid,
        request_id = request_id,
        task_id = task_id,
        op = ?op,
        "Processing report task"
    );

    // TODO: Implement task reporting logic based on operation type
    let success = true;
    let url = None;

    // Send acknowledgment
    let response = crate::websocket::CoordinatorMessage::TaskReportAck {
        request_id,
        success,
        url,
    };

    ws_state
        .ws_registry
        .send_to_manager(&manager_uuid, &response)
        .await?;

    Ok(())
}

/// Handle failure report - records task execution failure
async fn handle_report_failure(
    manager_uuid: Uuid,
    task_uuid: Uuid,
    failure_count: u32,
    error_message: String,
    worker_local_id: u32,
    ws_state: &Arc<WebSocketState>,
) -> crate::error::Result<()> {
    tracing::warn!(
        manager_uuid = %manager_uuid,
        task_uuid = %task_uuid,
        failure_count = failure_count,
        worker_local_id = worker_local_id,
        error = error_message,
        "Task failure reported"
    );

    let db = &ws_state.infra_pool.db;

    // Get manager record to find assigned suite and task_id
    use crate::entity::node_managers;

    let manager = node_managers::Entity::find()
        .filter(node_managers::Column::Uuid.eq(manager_uuid))
        .one(db)
        .await?
        .ok_or_else(|| crate::error::Error::Custom("Manager not found".to_string()))?;

    // Get task_id from task_uuid
    use crate::entity::active_tasks;

    let task = active_tasks::Entity::find()
        .filter(active_tasks::Column::Uuid.eq(task_uuid))
        .one(db)
        .await?
        .ok_or_else(|| crate::error::Error::Custom("Task not found".to_string()))?;

    // Record failure in task_execution_failures table
    use crate::entity::task_execution_failures;

    let now = time::OffsetDateTime::now_utc();

    let failure = task_execution_failures::ActiveModel {
        task_id: Set(task.id),
        task_uuid: Set(task_uuid),
        task_suite_id: Set(manager.assigned_task_suite_id),
        manager_id: Set(manager.id),
        failure_count: Set(failure_count as i32),
        last_failure_at: Set(now),
        error_messages: Set(Some(vec![error_message])),
        worker_local_id: Set(Some(worker_local_id as i32)),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };

    task_execution_failures::Entity::insert(failure)
        .exec(db)
        .await?;

    Ok(())
}

/// Handle task abort - marks task as aborted
async fn handle_abort_task(
    manager_uuid: Uuid,
    task_uuid: Uuid,
    reason: String,
    _ws_state: &Arc<WebSocketState>,
) -> crate::error::Result<()> {
    tracing::info!(
        manager_uuid = %manager_uuid,
        task_uuid = %task_uuid,
        reason = reason,
        "Task aborted"
    );

    // TODO: Implement task abort logic

    Ok(())
}

/// Handle suite completion - clears manager assignment and updates suite state
async fn handle_suite_completed(
    manager_uuid: Uuid,
    suite_uuid: Uuid,
    tasks_completed: u64,
    tasks_failed: u64,
    ws_state: &Arc<WebSocketState>,
) -> crate::error::Result<()> {
    use sea_orm::sea_query::Expr;

    tracing::info!(
        manager_uuid = %manager_uuid,
        suite_uuid = %suite_uuid,
        tasks_completed = tasks_completed,
        tasks_failed = tasks_failed,
        "Suite execution completed"
    );

    let db = &ws_state.infra_pool.db;
    let now = time::OffsetDateTime::now_utc();

    // Clear manager assignment
    use crate::entity::node_managers;

    node_managers::Entity::update_many()
        .col_expr(
            node_managers::Column::State,
            Expr::value(NodeManagerState::Idle),
        )
        .col_expr(
            node_managers::Column::AssignedTaskSuiteId,
            Expr::value(Option::<i64>::None),
        )
        .col_expr(
            node_managers::Column::LeaseExpiresAt,
            Expr::value(Option::<time::OffsetDateTime>::None),
        )
        .col_expr(node_managers::Column::UpdatedAt, Expr::value(now))
        .filter(node_managers::Column::Uuid.eq(manager_uuid))
        .exec(db)
        .await?;

    // Check if suite is fully complete
    use crate::entity::{state::TaskSuiteState, task_suites};

    let suite = task_suites::Entity::find()
        .filter(task_suites::Column::Uuid.eq(suite_uuid))
        .one(db)
        .await?;

    if let Some(suite) = suite {
        if suite.pending_tasks == 0 {
            // Mark suite as complete
            task_suites::Entity::update_many()
                .col_expr(
                    task_suites::Column::State,
                    Expr::value(TaskSuiteState::Complete),
                )
                .col_expr(task_suites::Column::UpdatedAt, Expr::value(now))
                .col_expr(task_suites::Column::CompletedAt, Expr::value(now))
                .filter(task_suites::Column::Uuid.eq(suite_uuid))
                .exec(db)
                .await?;

            tracing::info!(suite_uuid = %suite_uuid, "Suite fully completed");
        }
    }

    Ok(())
}

/// Mark a manager as offline in the database
async fn mark_manager_offline(
    infra_pool: &InfraPool,
    manager_uuid: Uuid,
) -> crate::error::Result<()> {
    use sea_orm::sea_query::Expr;

    let db = &infra_pool.db;
    let now = time::OffsetDateTime::now_utc();

    use crate::entity::node_managers;

    node_managers::Entity::update_many()
        .col_expr(
            node_managers::Column::State,
            Expr::value(NodeManagerState::Offline),
        )
        .col_expr(node_managers::Column::UpdatedAt, Expr::value(now))
        .filter(node_managers::Column::Uuid.eq(manager_uuid))
        .exec(db)
        .await?;

    tracing::info!(
        manager_uuid = %manager_uuid,
        "Marked manager as offline"
    );

    Ok(())
}

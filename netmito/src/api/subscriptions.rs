use axum::{
    extract::{Path, State},
    middleware,
    response::sse::{Event, Sse},
    routing::{get, post},
    Extension, Json, Router,
};
use uuid::Uuid;

use crate::{
    config::InfraPool,
    error::ApiError,
    schema::*,
    service::{
        auth::{user_auth_with_name_middleware, AuthUserWithName},
        subscription::{
            create_sse_stream, get_client_subscriptions, subscribe_to_task, unsubscribe_all,
            unsubscribe_from_task,
        },
    },
};

pub fn subscriptions_router(st: InfraPool) -> Router<InfraPool> {
    Router::new()
        .route(
            "/tasks/:uuid",
            post(subscribe_to_task_endpoint).delete(unsubscribe_from_task_endpoint),
        )
        .route("/list", post(get_client_subscriptions_endpoint))
        .route("/unsubscribe_all", post(unsubscribe_all_endpoint))
        .route("/events", get(sse_handler))
        .route_layer(middleware::from_fn_with_state(
            st.clone(),
            user_auth_with_name_middleware,
        ))
        .with_state(st)
}

pub async fn subscribe_to_task_endpoint(
    Extension(user): Extension<AuthUserWithName>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
    Json(req): Json<SubscribeTaskReq>,
) -> Result<Json<TaskSubscriptionResp>, ApiError> {
    let session_id = req.session_id.clone();
    subscribe_to_task(&pool.subscription_manager_tx, req.session_id, uuid)
        .await
        .map_err(|e| {
            tracing::error!("Failed to subscribe to task {}: {}", uuid, e);
            ApiError::InternalServerError
        })?;

    tracing::info!(
        "User {} subscribed to task {} with session {}",
        user.username,
        uuid,
        session_id
    );

    Ok(Json(TaskSubscriptionResp {
        subscribed: true,
        task_uuid: uuid,
    }))
}

pub async fn unsubscribe_from_task_endpoint(
    Extension(user): Extension<AuthUserWithName>,
    State(pool): State<InfraPool>,
    Path(uuid): Path<Uuid>,
    Json(req): Json<UnsubscribeTaskReq>,
) -> Result<Json<TaskSubscriptionResp>, ApiError> {
    let session_id = req.session_id.clone();
    let was_subscribed = unsubscribe_from_task(&pool.subscription_manager_tx, req.session_id, uuid)
        .await
        .map_err(|e| {
            tracing::error!("Failed to unsubscribe from task {}: {}", uuid, e);
            ApiError::InternalServerError
        })?;

    if was_subscribed {
        tracing::info!(
            "User {} unsubscribed from task {} with session {}",
            user.username,
            uuid,
            session_id
        );
    }

    Ok(Json(TaskSubscriptionResp {
        subscribed: false,
        task_uuid: uuid,
    }))
}

pub async fn get_client_subscriptions_endpoint(
    Extension(user): Extension<AuthUserWithName>,
    State(pool): State<InfraPool>,
    Json(req): Json<SessionCredentialsReq>,
) -> Result<Json<ClientSubscriptionsResp>, ApiError> {
    let subscriptions = get_client_subscriptions(&pool.subscription_manager_tx, req.session_id)
        .await
        .map_err(|e| {
            tracing::error!("Failed to get subscriptions for {}: {}", user.username, e);
            ApiError::InternalServerError
        })?;

    Ok(Json(ClientSubscriptionsResp { subscriptions }))
}

pub async fn unsubscribe_all_endpoint(
    Extension(user): Extension<AuthUserWithName>,
    State(pool): State<InfraPool>,
    Json(req): Json<SessionCredentialsReq>,
) -> Result<Json<ClientSubscriptionsResp>, ApiError> {
    let session_id = req.session_id.clone();
    let unsubscribed_tasks = unsubscribe_all(&pool.subscription_manager_tx, req.session_id)
        .await
        .map_err(|e| {
            tracing::error!("Failed to unsubscribe all for {}: {}", user.username, e);
            ApiError::InternalServerError
        })?;

    if !unsubscribed_tasks.is_empty() {
        tracing::info!(
            "User {} unsubscribed from {} tasks with session {}",
            user.username,
            unsubscribed_tasks.len(),
            session_id
        );
    }

    Ok(Json(ClientSubscriptionsResp {
        subscriptions: unsubscribed_tasks,
    }))
}

pub async fn sse_handler(
    Extension(user): Extension<AuthUserWithName>,
    State(pool): State<InfraPool>,
) -> Sse<impl futures::Stream<Item = std::result::Result<Event, axum::Error>>> {
    let client_id = format!("user:{}", user.username);
    tracing::info!("Starting SSE stream for user {}", user.username);

    create_sse_stream(client_id, pool.subscription_manager_tx.clone())
}

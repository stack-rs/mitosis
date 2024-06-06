use axum::{
    extract::State,
    middleware,
    routing::{delete, post},
    Extension, Json, Router,
};

use crate::{
    config::InfraPool,
    error::{ApiError, ApiResult, Error},
    schema::*,
    service::{
        auth::{token::generate_token, worker_auth_middleware, AuthUser, AuthWorker},
        worker,
    },
};

pub fn worker_router(st: InfraPool) -> Router<InfraPool> {
    Router::new()
        .route("/", delete(unregister))
        .route("/heartbeat", post(heartbeat))
        .route("/task", post(report_task).get(fetch_task))
        .layer(middleware::from_fn_with_state(
            st.clone(),
            worker_auth_middleware,
        ))
        .with_state(st)
}

pub async fn register(
    Extension(u): Extension<AuthUser>,
    State(pool): State<InfraPool>,
    Json(req): Json<RegisterWorkerReq>,
) -> ApiResult<Json<RegisterWorkerResp>> {
    let uuid = worker::register_worker(
        u.id,
        Vec::from_iter(req.tags),
        Vec::from_iter(req.groups),
        &pool,
    )
    .await
    .map_err(|e| match e {
        Error::ApiError(e) => e,
        _ => {
            tracing::error!("{}", e);
            ApiError::InternalServerError
        }
    })?;
    let token = generate_token(uuid.clone().to_string(), 0).map_err(|e| match e {
        crate::error::Error::AuthError(err) => ApiError::AuthError(err),
        crate::error::Error::ApiError(e) => e,
        _ => {
            tracing::error!("{}", e);
            ApiError::InternalServerError
        }
    })?;
    Ok(Json(RegisterWorkerResp {
        worker_id: uuid,
        token,
    }))
}

pub async fn unregister(
    Extension(w): Extension<AuthWorker>,
    State(pool): State<InfraPool>,
) -> ApiResult<Json<()>> {
    match worker::unregister_worker(w.id, &pool).await {
        Ok(_) => Ok(Json(())),
        Err(Error::ApiError(e)) => Err(e),
        Err(e) => {
            tracing::error!("{}", e);
            Err(ApiError::InternalServerError)
        }
    }
}

pub async fn heartbeat(
    Extension(w): Extension<AuthWorker>,
    State(pool): State<InfraPool>,
) -> ApiResult<Json<()>> {
    match worker::heartbeat(w.id, &pool).await {
        Ok(_) => Ok(Json(())),
        Err(Error::ApiError(e)) => Err(e),
        Err(e) => {
            tracing::error!("{}", e);
            Err(ApiError::InternalServerError)
        }
    }
}

pub async fn fetch_task(
    Extension(w): Extension<AuthWorker>,
    State(pool): State<InfraPool>,
) -> ApiResult<Json<Option<WorkerTaskResp>>> {
    match worker::fetch_task(w.id, &pool).await {
        Ok(t) => Ok(Json(t)),
        Err(Error::ApiError(e)) => Err(e),
        Err(e) => {
            tracing::error!("{}", e);
            Err(ApiError::InternalServerError)
        }
    }
}

pub async fn report_task(
    Extension(w): Extension<AuthWorker>,
    State(pool): State<InfraPool>,
    Json(ReportTaskReq { id, op }): Json<ReportTaskReq>,
) -> ApiResult<Json<ReportTaskResp>> {
    match worker::report_task(w.id, id, op, &pool).await {
        Ok(url) => Ok(Json(ReportTaskResp { url })),
        Err(Error::ApiError(e)) => Err(e),
        Err(e) => {
            tracing::error!("{}", e);
            Err(ApiError::InternalServerError)
        }
    }
}

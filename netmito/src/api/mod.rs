pub mod admin;
pub mod group;
pub mod user;
pub mod worker;

#[cfg(feature = "debugging")]
use std::net::SocketAddr;

#[cfg(feature = "debugging")]
use axum::{
    body::{Body, Bytes},
    extract::{ConnectInfo, Request},
    middleware::Next,
    response::{IntoResponse, Response},
};
use axum::{
    http::StatusCode,
    middleware,
    routing::{get, post},
    Json, Router,
};
#[cfg(feature = "debugging")]
use http_body_util::BodyExt;
use serde_json::json;

use crate::{config::InfraPool, service::auth::user_auth_middleware};

pub fn router(st: InfraPool) -> Router {
    #[cfg(not(feature = "debugging"))]
    {
        Router::new()
            .route(
                "/health",
                get(|| async { (StatusCode::OK, Json(json!({"status": "ok"}))) }),
            )
            .route("/login", post(user::login_user))
            .nest("/user", user::user_router(st.clone()))
            .nest("/admin", admin::admin_router(st.clone()))
            .route(
                "/group",
                post(group::create_group).layer(middleware::from_fn_with_state(
                    st.clone(),
                    user_auth_middleware,
                )),
            )
            .route(
                "/worker",
                post(worker::register).layer(middleware::from_fn_with_state(
                    st.clone(),
                    user_auth_middleware,
                )),
            )
            .nest("/worker", worker::worker_router(st.clone()))
            .with_state(st)
    }
    #[cfg(feature = "debugging")]
    {
        Router::new()
            .route(
                "/health",
                get(|| async { (StatusCode::OK, Json(json!({"status": "ok"}))) }),
            )
            .route("/login", post(user::login_user))
            .nest("/user", user::user_router(st.clone()))
            .nest("/admin", admin::admin_router(st.clone()))
            .route(
                "/group",
                post(group::create_group).layer(middleware::from_fn_with_state(
                    st.clone(),
                    user_auth_middleware,
                )),
            )
            .route(
                "/worker",
                post(worker::register).layer(middleware::from_fn_with_state(
                    st.clone(),
                    user_auth_middleware,
                )),
            )
            .nest("/worker", worker::worker_router(st.clone()))
            .with_state(st)
            .layer(middleware::from_fn(print_request_addr))
            .layer(middleware::from_fn(print_request_response))
    }
}

#[cfg(feature = "debugging")]
// For debugging purpose
async fn print_request_addr(
    req: Request,
    next: Next,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let remote_addr = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0);
    tracing::debug!("receive request from {:?} to {}", remote_addr, req.uri());
    let res = next.run(req).await;

    Ok(res)
}

#[cfg(feature = "debugging")]
// For debugging purpose
async fn print_request_response(
    req: Request,
    next: Next,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let (parts, body) = req.into_parts();
    let bytes = buffer_and_print("request", body).await?;
    let req = Request::from_parts(parts, Body::from(bytes));

    let res = next.run(req).await;

    let (parts, body) = res.into_parts();
    let bytes = buffer_and_print("response", body).await?;
    let res = Response::from_parts(parts, Body::from(bytes));

    Ok(res)
}

#[cfg(feature = "debugging")]
// For debugging purpose
async fn buffer_and_print<B>(direction: &str, body: B) -> Result<Bytes, (StatusCode, String)>
where
    B: axum::body::HttpBody<Data = Bytes>,
    B::Error: std::fmt::Display,
{
    let bytes = match body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(err) => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("failed to read {direction} body: {err}"),
            ));
        }
    };

    if let Ok(body) = std::str::from_utf8(&bytes) {
        tracing::debug!("{direction} body = {body:?}");
    }

    Ok(bytes)
}

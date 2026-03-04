//! WebSocket API routes

use axum::{middleware, routing::get, Router};

use crate::{config::InfraPool, service::auth::agent_auth_middleware, ws::websocket_handler};

pub fn ws_router(st: InfraPool) -> Router<InfraPool> {
    Router::new()
        .route("/agents", get(websocket_handler))
        .route_layer(middleware::from_fn_with_state(
            st.clone(),
            agent_auth_middleware,
        ))
        .with_state(st)
}

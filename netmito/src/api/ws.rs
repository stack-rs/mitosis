use axum::{middleware, routing::get, Router};

use crate::{
    config::InfraPool,
    service::auth::{auth_middleware, roles::AGENT},
    ws::websocket_handler,
};

pub fn ws_router(st: InfraPool) -> Router<InfraPool> {
    Router::new()
        .route("/agents", get(websocket_handler))
        .route_layer(middleware::from_fn_with_state(
            st.clone(),
            auth_middleware::<AGENT>,
        ))
        .with_state(st)
}

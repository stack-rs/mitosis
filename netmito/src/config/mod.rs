pub mod client;
pub mod coordinator;
pub mod worker;
pub use client::{ClientConfig, ClientConfigCli};
pub use coordinator::{CoordinatorConfig, CoordinatorConfigCli, InfraPool};
pub(crate) use coordinator::{
    DECODING_KEY, ENCODING_KEY, INIT_ADMIN_USER, REDIS_CONNECTION_INFO, SERVER_CONFIG,
    SHUTDOWN_SECRET,
};
use tracing::subscriber::DefaultGuard;
use tracing_appender::non_blocking::WorkerGuard;
pub use worker::{WorkerConfig, WorkerConfigCli};

pub struct TracingGuard {
    pub subscriber_guard: Option<DefaultGuard>,
    pub file_guard: Option<WorkerGuard>,
}

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::signal;
use tokio_util::sync::CancellationToken;

pub(crate) async fn shutdown_signal(cancel_token: CancellationToken) {
    let shutdown_initiated = Arc::new(AtomicBool::new(false));
    let shutdown_initiated_clone = shutdown_initiated.clone();

    let ctrl_c = async {
        // Wait for first Ctrl+C
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");

        if shutdown_initiated_clone
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            tracing::warn!("Ctrl+C received - initiating graceful shutdown...");

            // Spawn a task to handle subsequent Ctrl+C signals
            tokio::spawn(async {
                loop {
                    if signal::ctrl_c().await.is_ok() {
                        tracing::error!("Second Ctrl+C received - forcing immediate shutdown!");
                        std::process::exit(1);
                    }
                }
            });
        }
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
        tracing::warn!("Terminate signal received");
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {
            tracing::warn!("Terminate signal received, initiating graceful shutdown...");
        },
        _ = cancel_token.cancelled() => {
            tracing::warn!("Cancellation token received, initiating graceful shutdown...");
        },
    }
    tracing::warn!("Shutdown signal processed, stopping services...");
}

//! Feature-agnostic unbounded MPSC channel.
//!
//! The worker-side actors predate this module and each carry two
//! `#[cfg(feature = "crossfire-channel")]` copies of their plumbing. The
//! agent-side actors use these aliases instead so their code is written once;
//! the feature still selects the same underlying implementation.

/// Cloneable sender half. `send` never blocks (the channel is unbounded) and
/// only fails once every receiver is gone.
#[cfg(not(feature = "crossfire-channel"))]
pub type MTx<T> = tokio::sync::mpsc::UnboundedSender<T>;
#[cfg(feature = "crossfire-channel")]
pub type MTx<T> = crossfire::MTx<T>;

/// Receiver half. Wrapped so `recv` has one signature across both backends
/// (tokio yields `Option`, crossfire yields `Result`).
pub struct MRx<T> {
    #[cfg(not(feature = "crossfire-channel"))]
    inner: tokio::sync::mpsc::UnboundedReceiver<T>,
    #[cfg(feature = "crossfire-channel")]
    inner: crossfire::AsyncRx<T>,
}

impl<T> MRx<T> {
    /// Receive the next message, or `None` once the channel is closed and drained.
    pub async fn recv(&mut self) -> Option<T> {
        #[cfg(not(feature = "crossfire-channel"))]
        {
            self.inner.recv().await
        }
        #[cfg(feature = "crossfire-channel")]
        {
            self.inner.recv().await.ok()
        }
    }
}

/// Create an unbounded channel. `Unpin` is required by the crossfire backend
/// and stated unconditionally so both builds accept the same message types.
pub fn unbounded<T: Unpin>() -> (MTx<T>, MRx<T>) {
    #[cfg(not(feature = "crossfire-channel"))]
    let (tx, inner) = tokio::sync::mpsc::unbounded_channel();
    #[cfg(feature = "crossfire-channel")]
    let (tx, inner) = crossfire::mpsc::unbounded_async();
    (tx, MRx { inner })
}

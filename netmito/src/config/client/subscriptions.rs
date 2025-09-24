use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct SubscriptionArgs {
    #[command(subcommand)]
    pub command: SubscriptionCommands,
}

#[derive(Subcommand, Serialize, Debug, Deserialize, Clone)]
pub enum SubscriptionCommands {
    /// Subscribe to task state change notifications
    Subscribe(SubscribeTaskArgs),
    /// Unsubscribe from task state change notifications
    Unsubscribe(UnsubscribeTaskArgs),
    /// List current subscriptions
    List,
    /// Unsubscribe from all tasks
    Clear,
    /// Watch for task state changes in real-time
    Watch(WatchArgs),
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct SubscribeTaskArgs {
    /// UUID of the task to subscribe to
    pub uuid: Uuid,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct UnsubscribeTaskArgs {
    /// UUID of the task to unsubscribe from
    pub uuid: Uuid,
}

#[derive(Serialize, Debug, Deserialize, Args, Clone)]
pub struct WatchArgs {
    /// UUIDs of tasks to watch (if not specified, watch all subscribed tasks)
    pub uuids: Vec<Uuid>,
    /// Stop watching after this many notifications (0 = infinite)
    #[arg(long, short = 'n', default_value = "0")]
    pub max_notifications: usize,
    /// Timeout in seconds (0 = no timeout)
    #[arg(long, short = 't', default_value = "0")]
    pub timeout: u64,
}

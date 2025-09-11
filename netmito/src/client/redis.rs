use ouroboros::self_referencing;
use redis::{
    aio::{MultiplexedConnection, PubSub},
    AsyncCommands, Commands, PubSubCommands, PushInfo,
};
#[cfg(feature = "crossfire-channel")]
use std::ops::Deref;
#[cfg(not(feature = "crossfire-channel"))]
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use uuid::Uuid;

use crate::entity::state::TaskExecState;
pub use redis::{ControlFlow, Msg};

#[cfg(feature = "crossfire-channel")]
pub struct UnboundedReceiver<T>(crossfire::AsyncRx<T>);

#[cfg(feature = "crossfire-channel")]
impl redis::aio::AsyncPushSender for UnboundedSender<PushInfo> {
    fn send(&self, info: PushInfo) -> std::result::Result<(), redis::aio::SendError> {
        self.0.send(info).map_err(|_| redis::aio::SendError)
    }
}

#[cfg(feature = "crossfire-channel")]
impl<T> Deref for UnboundedReceiver<T> {
    type Target = crossfire::AsyncRx<T>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[cfg(feature = "crossfire-channel")]
#[derive(Clone)]
pub struct UnboundedSender<T>(crossfire::MTx<T>);

#[cfg(feature = "crossfire-channel")]
impl<T> Deref for UnboundedSender<T> {
    type Target = crossfire::MTx<T>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[self_referencing]
pub struct MitoRedisPubSubClient {
    pub client: redis::Client,
    pub connection: redis::Connection,
    pubsub_con: redis::Connection,
    #[borrows(mut pubsub_con)]
    #[not_covariant]
    pubsub: redis::PubSub<'this>,
}

pub struct MitoRedisClient {
    pub client: redis::Client,
    pub connection: redis::Connection,
}

// There seems to be an internal bug for the async PubSub to lost messages occasionally
pub struct MitoAsyncRedisClient {
    pub client: redis::Client,
    pub connection: MultiplexedConnection,
    pub pubsub: PubSub,
}

impl MitoRedisPubSubClient {
    pub fn new_with_url(url: &str) -> crate::error::Result<Self> {
        let client = redis::Client::open(url)?;
        let connection = client.get_connection()?;
        let pubsub_con = client.get_connection()?;
        Ok(MitoRedisPubSubClientBuilder {
            client,
            connection,
            pubsub_con,
            pubsub_builder: |pubsub_con| pubsub_con.as_pubsub(),
        }
        .build())
    }
    pub fn get_task_exec_state(&mut self, uuid: &Uuid) -> crate::error::Result<TaskExecState> {
        self.with_connection_mut(|con| {
            let state: i32 = con.get(format!("task:{uuid}"))?;
            Ok(TaskExecState::from(state))
        })
    }

    pub fn subscribe_with<T, F, U>(&mut self, uuids: T, func: F) -> crate::error::Result<U>
    where
        F: FnMut(Msg) -> redis::ControlFlow<U>,
        T: IntoIterator<Item = Uuid>,
    {
        let uuids = uuids
            .into_iter()
            .map(|uuid| format!("task:{uuid}"))
            .collect::<Vec<_>>();
        self.with_connection_mut(|con| Ok(con.subscribe(uuids, func)?))
    }

    pub fn get_connection(&self) -> crate::error::Result<redis::Connection> {
        self.with_client(|client| Ok(client.get_connection()?))
    }

    pub fn subscribe_task_exec_state(&mut self, uuid: &Uuid) -> crate::error::Result<()> {
        self.with_pubsub_mut(|pubsub| pubsub.subscribe(format!("task:{uuid}")))?;
        Ok(())
    }

    pub fn unsubscribe_task_exec_state(&mut self, uuid: &Uuid) -> crate::error::Result<()> {
        self.with_pubsub_mut(|pubsub| pubsub.unsubscribe(format!("task:{uuid}")))?;
        Ok(())
    }

    pub fn get_task_exec_state_message(&mut self) -> crate::error::Result<Msg> {
        self.with_pubsub_mut(|pubsub| Ok(pubsub.get_message()?))
    }
}

impl MitoRedisClient {
    pub fn new(url: &str) -> crate::error::Result<Self> {
        let client = redis::Client::open(url)?;
        let connection = client.get_connection()?;
        Ok(MitoRedisClient { client, connection })
    }
    pub fn get_task_exec_state(&mut self, uuid: &Uuid) -> crate::error::Result<TaskExecState> {
        let state: i32 = self.connection.get(format!("task:{uuid}"))?;
        Ok(TaskExecState::from(state))
    }

    pub fn subscribe_with<T, F, U>(&mut self, uuids: T, func: F) -> crate::error::Result<U>
    where
        F: FnMut(Msg) -> redis::ControlFlow<U>,
        T: IntoIterator<Item = Uuid>,
    {
        let uuids = uuids
            .into_iter()
            .map(|uuid| format!("task:{uuid}"))
            .collect::<Vec<_>>();
        Ok(self.connection.subscribe(uuids, func)?)
    }

    pub fn get_connection(&self) -> crate::error::Result<redis::Connection> {
        Ok(self.client.get_connection()?)
    }
}

pub struct AsyncPubSub {
    pub connection: MultiplexedConnection,
    pub tx: UnboundedSender<PushInfo>,
    pub rx: UnboundedReceiver<PushInfo>,
}

impl AsyncPubSub {
    pub fn get_connection(&self) -> MultiplexedConnection {
        self.connection.clone()
    }

    pub fn get_tx(&self) -> UnboundedSender<PushInfo> {
        self.tx.clone()
    }

    pub fn get_mut_rx(&mut self) -> &mut UnboundedReceiver<PushInfo> {
        &mut self.rx
    }
}

impl MitoAsyncRedisClient {
    pub async fn new(url: &str) -> crate::error::Result<Self> {
        let client = redis::Client::open(url)?;
        let connection = client.get_multiplexed_async_connection().await?;
        let pubsub = client.get_async_pubsub().await?;
        Ok(MitoAsyncRedisClient {
            client,
            connection,
            pubsub,
        })
    }

    pub async fn get_task_exec_state(
        &mut self,
        uuid: &Uuid,
    ) -> crate::error::Result<TaskExecState> {
        let state: i32 = self.connection.get(format!("task:{uuid}")).await?;
        Ok(TaskExecState::from(state))
    }

    pub async fn subscribe_task_exec_state(&mut self, uuid: &Uuid) -> crate::error::Result<()> {
        self.pubsub.subscribe(format!("task:{uuid}")).await?;
        Ok(())
    }

    pub async fn on_task_exec_state_message(
        &mut self,
    ) -> crate::error::Result<impl futures::stream::Stream<Item = Msg> + '_> {
        Ok(self.pubsub.on_message())
    }

    pub async fn unsubscribe_task_exec_state(&mut self, uuid: &Uuid) -> crate::error::Result<()> {
        self.pubsub.unsubscribe(format!("task:{uuid}")).await?;
        Ok(())
    }

    pub async fn get_resp3_pubsub(&mut self) -> crate::error::Result<AsyncPubSub> {
        #[cfg(not(feature = "crossfire-channel"))]
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        #[cfg(feature = "crossfire-channel")]
        let (tx, rx) = {
            let (tx, rx) = crossfire::mpsc::unbounded_async();
            (UnboundedSender(tx), UnboundedReceiver(rx))
        };
        let config = redis::AsyncConnectionConfig::new().set_push_sender(tx.clone());
        let con = self
            .client
            .get_multiplexed_async_connection_with_config(&config)
            .await?;
        Ok(AsyncPubSub {
            connection: con,
            tx,
            rx,
        })
    }
}

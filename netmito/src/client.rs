use std::{borrow::Cow, path::PathBuf};

use clap_repl::{
    reedline::{self, FileBackedHistory},
    ClapEditor, ReadCommandOutput,
};
use humansize::{format_size, DECIMAL};
use ouroboros::self_referencing;
use redis::{
    aio::{MultiplexedConnection, PubSub},
    AsyncCommands, Commands, Msg, PubSubCommands, PushInfo,
};
use reqwest::{header::CONTENT_LENGTH, Client};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use url::Url;
use uuid::Uuid;

use crate::{
    config::{client::*, ClientConfig, ClientConfigCli},
    entity::{role::GroupWorkerRole, state::TaskExecState},
    error::{get_error_from_resp, map_reqwest_err, RequestError, S3Error},
    schema::*,
    service::{
        auth::cred::{get_user_credential, refresh_user_credential},
        s3::{download_file, get_xml_error_message},
    },
};

static DEFAULT_LEFT_PROMPT: &str = "[mito::client]";
static DEFAULT_INDICATOR: &str = "> ";
static DEFAULT_MULTILINE_INDICATOR: &str = "::: ";

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
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
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

pub struct MitoPrompt;

impl reedline::Prompt for MitoPrompt {
    fn render_prompt_left(&self) -> std::borrow::Cow<str> {
        Cow::Borrowed(DEFAULT_LEFT_PROMPT)
    }

    fn render_prompt_right(&self) -> std::borrow::Cow<str> {
        Cow::Borrowed("")
    }

    fn render_prompt_indicator(
        &self,
        _prompt_mode: reedline::PromptEditMode,
    ) -> std::borrow::Cow<str> {
        Cow::Borrowed(DEFAULT_INDICATOR)
    }

    fn render_prompt_multiline_indicator(&self) -> std::borrow::Cow<str> {
        Cow::Borrowed(DEFAULT_MULTILINE_INDICATOR)
    }

    fn render_prompt_history_search_indicator(
        &self,
        history_search: reedline::PromptHistorySearch,
    ) -> std::borrow::Cow<str> {
        let prefix = match history_search.status {
            reedline::PromptHistorySearchStatus::Passing => "",
            reedline::PromptHistorySearchStatus::Failing => "failing ",
        };
        // NOTE: magic strings, given there is logic on how these compose I am not sure if it
        // is worth extracting in to static constant
        Cow::Owned(format!(
            "({}reverse-search: {}) ",
            prefix, history_search.term
        ))
    }
}

pub struct MitoClient {
    http_client: Client,
    url: Url,
    credential: String,
    credential_path: PathBuf,
    username: String,
    redis_client: Option<MitoRedisClient>,
    redis_pubsub_client: Option<MitoRedisPubSubClient>,
    async_redis_client: Option<MitoAsyncRedisClient>,
}

impl MitoClient {
    pub async fn main(mut cli: ClientConfigCli) {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "netmito=info".into()),
            )
            .with(tracing_subscriber::fmt::layer())
            .init();
        match ClientConfig::new(&cli) {
            Ok(config) => match Self::setup(&config).await {
                Ok(mut client) => {
                    if let Some(cmd) = cli.command.take() {
                        client.handle_command(cmd).await;
                    }
                    if cli.interactive {
                        println!("Client is running in interactive mode. Enter 'quit' or Ctrl-D to exit and 'help' to see available commands.");
                        let prompt = MitoPrompt;
                        let cache_file = dirs::cache_dir().map(|mut p| {
                            p.push("mitosis");
                            p.push("client-history");
                            p
                        });
                        let mut rl = ClapEditor::<ClientInteractiveShell>::builder()
                            .with_prompt(Box::new(prompt))
                            .with_editor_hook(|reed| {
                                // Do custom things with `Reedline` instance here
                                if let Some(history_file) = cache_file {
                                    reed.with_history(Box::new(
                                        FileBackedHistory::with_file(1000, history_file).unwrap(),
                                    ))
                                } else {
                                    reed
                                }
                            })
                            .build();
                        loop {
                            match rl.read_command() {
                                ReadCommandOutput::Command(c) => {
                                    if !client.handle_command(c.command).await {
                                        break;
                                    }
                                },
                                ReadCommandOutput::CtrlC | ReadCommandOutput::EmptyLine => {},
                                ReadCommandOutput::ClapError(error) => println!("{error}"),
                                ReadCommandOutput::ShlexError => println!("error: Input was not lexically valid, for example it had odd number of \""),
                                ReadCommandOutput::ReedlineError(error) => println!("error: Reedline failed to work with stdio due to {error}"),
                                ReadCommandOutput::CtrlD => break,
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("{}", e);
                }
            },
            Err(e) => {
                tracing::error!("{}", e);
            }
        }
    }

    pub async fn setup(config: &ClientConfig) -> crate::error::Result<Self> {
        tracing::debug!("Client is setting up");
        let http_client = Client::new();
        let credential_path = config
            .credential_path
            .as_ref()
            .map(|p| p.relative())
            .or_else(|| {
                dirs::config_dir().map(|mut p| {
                    p.push("mitosis");
                    p.push("credentials");
                    p
                })
            })
            .ok_or(crate::error::Error::ConfigError(Box::new(
                figment::Error::from("credential path not found"),
            )))?;
        let (username, credential) = get_user_credential(
            config.credential_path.as_ref(),
            &http_client,
            config.coordinator_addr.clone(),
            config.user.as_ref(),
            config.password.as_ref(),
        )
        .await?;
        let url = config.coordinator_addr.clone();
        Ok(MitoClient {
            http_client,
            url,
            credential,
            credential_path,
            username,
            redis_client: None,
            redis_pubsub_client: None,
            async_redis_client: None,
        })
    }

    pub async fn get_redis_connection_info(&mut self) -> crate::error::Result<RedisConnectionInfo> {
        self.url.set_path("user/redis");
        let resp = self
            .http_client
            .get(self.url.as_str())
            .bearer_auth(&self.credential)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            let resp = resp
                .json::<RedisConnectionInfo>()
                .await
                .map_err(RequestError::from)?;
            Ok(resp)
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn setup_redis_client(&mut self) -> crate::error::Result<()> {
        let resp = self.get_redis_connection_info().await?;
        if let Some(redis_url) = resp.url {
            let client = MitoRedisClient::new(&redis_url)?;
            self.redis_client = Some(client);
        } else {
            tracing::warn!("No Redis connection info found from coordinator");
        }
        Ok(())
    }

    pub async fn setup_redis_pubsub_client(&mut self) -> crate::error::Result<()> {
        let resp = self.get_redis_connection_info().await?;
        if let Some(redis_url) = resp.url {
            let client = MitoRedisPubSubClient::new_with_url(&redis_url)?;
            self.redis_pubsub_client = Some(client);
        } else {
            tracing::warn!("No Redis connection info found from coordinator");
        }
        Ok(())
    }

    pub async fn setup_async_redis_client(&mut self) -> crate::error::Result<()> {
        let resp = self.get_redis_connection_info().await?;
        if let Some(redis_url) = resp.url {
            let client = MitoAsyncRedisClient::new(&redis_url).await?;
            self.async_redis_client = Some(client);
        } else {
            tracing::warn!("No Redis connection info found from coordinator");
        }
        Ok(())
    }

    pub async fn get_redis_client(&mut self) -> crate::error::Result<MitoRedisClient> {
        let resp = self.get_redis_connection_info().await?;
        if let Some(redis_url) = resp.url {
            let client = MitoRedisClient::new(&redis_url)?;
            Ok(client)
        } else {
            Err(crate::error::Error::Custom(
                "No Redis connection info found from coordinator".to_string(),
            ))
        }
    }

    pub async fn get_redis_pubsub_client(&mut self) -> crate::error::Result<MitoRedisPubSubClient> {
        let resp = self.get_redis_connection_info().await?;
        if let Some(redis_url) = resp.url {
            let client = MitoRedisPubSubClient::new_with_url(&redis_url)?;
            Ok(client)
        } else {
            Err(crate::error::Error::Custom(
                "No Redis connection info found from coordinator".to_string(),
            ))
        }
    }

    pub async fn get_async_redis_client(&mut self) -> crate::error::Result<MitoAsyncRedisClient> {
        let resp = self.get_redis_connection_info().await?;
        if let Some(redis_url) = resp.url {
            let client = MitoAsyncRedisClient::new(&redis_url).await?;
            Ok(client)
        } else {
            Err(crate::error::Error::Custom(
                "No Redis connection info found from coordinator".to_string(),
            ))
        }
    }

    pub fn get_task_exec_state(&mut self, uuid: &Uuid) -> crate::error::Result<TaskExecState> {
        if let Some(client) = self.redis_client.as_mut() {
            client.get_task_exec_state(uuid)
        } else if let Some(client) = self.redis_pubsub_client.as_mut() {
            client.get_task_exec_state(uuid)
        } else {
            Err(crate::error::Error::Custom(
                "No Redis connection found".to_string(),
            ))
        }
    }

    pub async fn async_get_task_exec_state(
        &mut self,
        uuid: &Uuid,
    ) -> crate::error::Result<TaskExecState> {
        if let Some(client) = self.async_redis_client.as_mut() {
            client.get_task_exec_state(uuid).await
        } else {
            Err(crate::error::Error::Custom(
                "No Redis connection found".to_string(),
            ))
        }
    }

    pub fn subscribe_with<T, F, U>(&mut self, uuids: T, func: F) -> crate::error::Result<U>
    where
        F: FnMut(Msg) -> redis::ControlFlow<U>,
        T: IntoIterator<Item = Uuid>,
    {
        if let Some(client) = self.redis_client.as_mut() {
            client.subscribe_with(uuids, func)
        } else {
            Err(crate::error::Error::Custom(
                "No Redis connection found".to_string(),
            ))
        }
    }

    pub fn subscribe_task_exec_state(&mut self, uuid: &Uuid) -> crate::error::Result<()> {
        if let Some(client) = self.redis_pubsub_client.as_mut() {
            client.subscribe_task_exec_state(uuid)
        } else {
            Err(crate::error::Error::Custom(
                "No Redis connection found".to_string(),
            ))
        }
    }

    pub async fn async_subscribe_task_exec_state(
        &mut self,
        uuid: &Uuid,
    ) -> crate::error::Result<()> {
        if let Some(client) = self.async_redis_client.as_mut() {
            client.subscribe_task_exec_state(uuid).await
        } else {
            Err(crate::error::Error::Custom(
                "No Redis connection found".to_string(),
            ))
        }
    }

    pub fn get_task_exec_state_message(&mut self) -> crate::error::Result<Msg> {
        if let Some(client) = self.redis_pubsub_client.as_mut() {
            client.get_task_exec_state_message()
        } else {
            Err(crate::error::Error::Custom(
                "No Redis connection found".to_string(),
            ))
        }
    }

    pub async fn on_task_exec_state_message(
        &mut self,
    ) -> crate::error::Result<impl futures::stream::Stream<Item = Msg> + '_> {
        if let Some(client) = self.async_redis_client.as_mut() {
            client.on_task_exec_state_message().await
        } else {
            Err(crate::error::Error::Custom(
                "No Redis connection found".to_string(),
            ))
        }
    }

    pub fn unsubscribe_task_exec_state(&mut self, uuid: &Uuid) -> crate::error::Result<()> {
        if let Some(client) = self.redis_pubsub_client.as_mut() {
            client.unsubscribe_task_exec_state(uuid)
        } else {
            Err(crate::error::Error::Custom(
                "No Redis connection found".to_string(),
            ))
        }
    }

    pub async fn async_unsubscribe_task_exec_state(
        &mut self,
        uuid: &Uuid,
    ) -> crate::error::Result<()> {
        if let Some(client) = self.async_redis_client.as_mut() {
            client.unsubscribe_task_exec_state(uuid).await
        } else {
            Err(crate::error::Error::Custom(
                "No Redis connection found".to_string(),
            ))
        }
    }

    pub async fn auth_user(&mut self, args: UserLoginReq) -> crate::error::Result<()> {
        let token = refresh_user_credential(
            Some(&self.credential_path),
            &self.http_client,
            &mut self.url,
            &args,
        )
        .await?;
        self.credential = token;
        Ok(())
    }

    pub async fn create_user(&mut self, args: CreateUserArgs) -> crate::error::Result<()> {
        self.url.set_path("admin/user");
        let req = CreateUserReq {
            username: args.username,
            md5_password: md5::compute(args.password.as_bytes()).0,
            admin: args.admin,
        };
        let resp = self
            .http_client
            .post(self.url.as_str())
            .json(&req)
            .bearer_auth(&self.credential)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn create_group(&mut self, args: CreateGroupArgs) -> crate::error::Result<()> {
        self.url.set_path("groups");
        let req = CreateGroupReq {
            group_name: args.name,
        };
        let resp = self
            .http_client
            .post(self.url.as_str())
            .json(&req)
            .bearer_auth(&self.credential)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn get_task(&mut self, args: GetTaskArgs) -> crate::error::Result<TaskQueryResp> {
        self.url.set_path(&format!("user/tasks/{}", args.uuid));
        let resp = self
            .http_client
            .get(self.url.as_str())
            .bearer_auth(&self.credential)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            let resp = resp
                .json::<TaskQueryResp>()
                .await
                .map_err(RequestError::from)?;
            Ok(resp)
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn get_artifact(
        &mut self,
        args: GetArtifactArgs,
    ) -> crate::error::Result<ResourceDownloadInfo> {
        let content_serde_val = serde_json::to_value(args.content_type)?;
        let content_serde_str = content_serde_val.as_str().unwrap_or("result");
        self.url.set_path(&format!(
            "user/artifacts/{}/{}",
            args.uuid, content_serde_str
        ));
        let resp = self
            .http_client
            .get(self.url.as_str())
            .bearer_auth(&self.credential)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            let download_resp = resp
                .json::<RemoteResourceDownloadResp>()
                .await
                .map_err(RequestError::from)?;
            download_file(&self.http_client, &download_resp, &args.output_path).await?;
            Ok(ResourceDownloadInfo {
                size: download_resp.size,
                local_path: args.output_path,
            })
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn get_attachment(
        &mut self,
        args: GetAttachmentArgs,
    ) -> crate::error::Result<ResourceDownloadInfo> {
        self.url.set_path(&format!(
            "user/attachments/{}/{}",
            args.group_name, args.key
        ));
        let resp = self
            .http_client
            .get(self.url.as_str())
            .bearer_auth(&self.credential)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            let download_resp = resp
                .json::<RemoteResourceDownloadResp>()
                .await
                .map_err(RequestError::from)?;
            download_file(&self.http_client, &download_resp, &args.output_path).await?;
            Ok(ResourceDownloadInfo {
                size: download_resp.size,
                local_path: args.output_path,
            })
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn get_attachment_url(
        &mut self,
        args: GetAttachmentArgs,
    ) -> crate::error::Result<String> {
        self.url.set_path(&format!(
            "user/attachments/{}/{}",
            args.group_name, args.key
        ));
        let resp = self
            .http_client
            .get(self.url.as_str())
            .bearer_auth(&self.credential)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            let download_resp = resp
                .json::<RemoteResourceDownloadResp>()
                .await
                .map_err(RequestError::from)?;
            Ok(download_resp.url)
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn get_attachment_meta(
        &mut self,
        args: GetAttachmentMetaArgs,
    ) -> crate::error::Result<AttachmentMetadata> {
        self.url.set_path(&format!(
            "user/attachments/meta/{}/{}",
            args.group_name, args.key
        ));
        let resp = self
            .http_client
            .get(self.url.as_str())
            .bearer_auth(&self.credential)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            let meta = resp
                .json::<AttachmentMetadata>()
                .await
                .map_err(RequestError::from)?;
            Ok(meta)
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn get_tasks(
        &mut self,
        args: TasksQueryReq,
    ) -> crate::error::Result<Vec<TaskQueryInfo>> {
        self.url.set_path("user/filters/tasks");
        let resp = self
            .http_client
            .post(self.url.as_str())
            .json(&args)
            .bearer_auth(&self.credential)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            let resp = resp
                .json::<Vec<TaskQueryInfo>>()
                .await
                .map_err(RequestError::from)?;
            Ok(resp)
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn get_attachments(
        &mut self,
        args: AttachmentsQueryReq,
    ) -> crate::error::Result<Vec<AttachmentQueryInfo>> {
        self.url.set_path("user/filters/attachments");
        let resp = self
            .http_client
            .post(self.url.as_str())
            .json(&args)
            .bearer_auth(&self.credential)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            let resp = resp
                .json::<Vec<AttachmentQueryInfo>>()
                .await
                .map_err(RequestError::from)?;
            Ok(resp)
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn get_worker(
        &mut self,
        args: GetWorkerArgs,
    ) -> crate::error::Result<WorkerQueryResp> {
        self.url.set_path(&format!("user/workers/{}/", args.uuid));
        let resp = self
            .http_client
            .get(self.url.as_str())
            .bearer_auth(&self.credential)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            let resp = resp
                .json::<WorkerQueryResp>()
                .await
                .map_err(RequestError::from)?;
            Ok(resp)
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn get_workers(
        &mut self,
        args: WorkersQueryReq,
    ) -> crate::error::Result<WorkersQueryResp> {
        self.url.set_path("user/filters/workers");
        let resp = self
            .http_client
            .post(self.url.as_str())
            .json(&args)
            .bearer_auth(&self.credential)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            let resp = resp
                .json::<WorkersQueryResp>()
                .await
                .map_err(RequestError::from)?;
            Ok(resp)
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn get_group(&mut self, args: GetGroupArgs) -> crate::error::Result<GroupQueryInfo> {
        self.url.set_path(&format!("groups/{}", args.group));
        let resp = self
            .http_client
            .get(self.url.as_str())
            .bearer_auth(&self.credential)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            let resp = resp
                .json::<GroupQueryInfo>()
                .await
                .map_err(RequestError::from)?;
            Ok(resp)
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn get_groups(&mut self) -> crate::error::Result<GroupsQueryResp> {
        self.url.set_path("user/groups");
        let resp = self
            .http_client
            .get(self.url.as_str())
            .bearer_auth(&self.credential)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            let resp = resp
                .json::<GroupsQueryResp>()
                .await
                .map_err(RequestError::from)?;
            Ok(resp)
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn submit_task(
        &mut self,
        args: SubmitTaskArgs,
    ) -> crate::error::Result<SubmitTaskResp> {
        self.url.set_path("user/tasks");
        let req = self.gen_submit_task_req(args);
        let resp = self
            .http_client
            .post(self.url.as_str())
            .json(&req)
            .bearer_auth(&self.credential)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            let resp = resp
                .json::<SubmitTaskResp>()
                .await
                .map_err(RequestError::from)?;
            Ok(resp)
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn upload_artifact(&mut self, args: UploadArtifactArgs) -> crate::error::Result<()> {
        self.url.set_path("user/artifacts");
        let metadata = args
            .local_file
            .metadata()
            .map_err(crate::error::Error::from)?;
        if metadata.is_dir() {
            return Err(crate::error::Error::Custom(
                "Currently we do not support uploading a directory".to_string(),
            ));
        }
        let content_length = metadata.len();
        let req = UploadArtifactReq {
            uuid: args.uuid,
            content_length,
            content_type: args.content_type,
        };
        let resp = self
            .http_client
            .post(self.url.as_str())
            .json(&req)
            .bearer_auth(&self.credential)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            let resp = resp
                .json::<UploadArtifactResp>()
                .await
                .map_err(RequestError::from)?;
            let file = tokio::fs::File::open(args.local_file).await?;
            let upload_file = self
                .http_client
                .put(resp.url)
                .header(CONTENT_LENGTH, content_length)
                .body(file)
                .send()
                .await
                .map_err(map_reqwest_err)?;
            if upload_file.status().is_success() {
                Ok(())
            } else {
                let msg = get_xml_error_message(upload_file).await?;
                Err(S3Error::Custom(msg).into())
            }
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn upload_attachment(
        &mut self,
        args: UploadAttachmentArgs,
    ) -> crate::error::Result<()> {
        self.url.set_path("user/attachments");
        let key = match args.key {
            Some(k) => k,
            None => args
                .local_file
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
                .ok_or(crate::error::Error::Custom("Key is required".to_string()))?,
        };
        let key = path_clean::clean(key).display().to_string();
        let metadata = args
            .local_file
            .metadata()
            .map_err(crate::error::Error::from)?;
        if metadata.is_dir() {
            return Err(crate::error::Error::Custom(
                "Currently we do not support uploading a directory".to_string(),
            ));
        }
        let content_length = metadata.len();
        let req = UploadAttachmentReq {
            group_name: args.group_name.unwrap_or(self.username.clone()),
            key,
            content_length,
        };
        let resp = self
            .http_client
            .post(self.url.as_str())
            .json(&req)
            .bearer_auth(&self.credential)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            let resp = resp
                .json::<UploadAttachmentResp>()
                .await
                .map_err(RequestError::from)?;
            let file = tokio::fs::File::open(args.local_file).await?;
            let upload_file = self
                .http_client
                .put(resp.url)
                .header(CONTENT_LENGTH, content_length)
                .body(file)
                .send()
                .await
                .map_err(map_reqwest_err)?;
            if upload_file.status().is_success() {
                Ok(())
            } else {
                let msg = get_xml_error_message(upload_file).await?;
                Err(S3Error::Custom(msg).into())
            }
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn cancel_worker(
        &mut self,
        uuid: Uuid,
        args: CancelWorkerArgs,
    ) -> crate::error::Result<()> {
        if args.force {
            self.url.set_path(&format!("user/workers/{uuid}/"));
            self.url.set_query(Some("op=force"));
        } else {
            self.url.set_path(&format!("user/workers/{uuid}/"));
        }
        let resp = self
            .http_client
            .delete(self.url.as_str())
            .bearer_auth(&self.credential)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn replace_worker_tags(
        &mut self,
        uuid: Uuid,
        args: ReplaceWorkerTagsReq,
    ) -> crate::error::Result<()> {
        self.url.set_path(&format!("user/workers/{uuid}/tags"));
        let resp = self
            .http_client
            .put(self.url.as_str())
            .json(&args)
            .bearer_auth(&self.credential)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn update_group_worker_roles(
        &mut self,
        uuid: Uuid,
        args: UpdateGroupWorkerRoleReq,
    ) -> crate::error::Result<()> {
        self.url.set_path(&format!("user/workers/{uuid}/groups"));
        let resp = self
            .http_client
            .put(self.url.as_str())
            .json(&args)
            .bearer_auth(&self.credential)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn remove_group_worker_roles(
        &mut self,
        uuid: Uuid,
        args: RemoveGroupWorkerRoleReq,
    ) -> crate::error::Result<()> {
        self.url.set_path(&format!("user/workers/{uuid}/groups"));
        let resp = self
            .http_client
            .delete(self.url.as_str())
            .json(&args)
            .bearer_auth(&self.credential)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn cancel_task(&mut self, uuid: Uuid) -> crate::error::Result<()> {
        self.url.set_path(&format!("user/tasks/{uuid}"));
        let resp = self
            .http_client
            .delete(self.url.as_str())
            .bearer_auth(&self.credential)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn update_task_labels(
        &mut self,
        uuid: Uuid,
        args: UpdateTaskLabelsReq,
    ) -> crate::error::Result<()> {
        self.url.set_path(&format!("user/tasks/{uuid}/labels"));
        let resp = self
            .http_client
            .put(self.url.as_str())
            .json(&args)
            .bearer_auth(&self.credential)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn change_task(
        &mut self,
        uuid: Uuid,
        args: ChangeTaskReq,
    ) -> crate::error::Result<()> {
        self.url.set_path(&format!("user/tasks/{uuid}"));
        let resp = self
            .http_client
            .put(self.url.as_str())
            .json(&args)
            .bearer_auth(&self.credential)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn update_user_group_roles(
        &mut self,
        group: String,
        args: UpdateUserGroupRoleReq,
    ) -> crate::error::Result<()> {
        self.url.set_path(&format!("groups/{group}/users"));
        let resp = self
            .http_client
            .put(self.url.as_str())
            .json(&args)
            .bearer_auth(&self.credential)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn remove_user_group_roles(
        &mut self,
        group: String,
        args: RemoveUserGroupRoleReq,
    ) -> crate::error::Result<()> {
        self.url.set_path(&format!("groups/{group}/users"));
        let resp = self
            .http_client
            .delete(self.url.as_str())
            .json(&args)
            .bearer_auth(&self.credential)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn shutdown_coordinator(&mut self, secret: String) -> crate::error::Result<()> {
        self.url.set_path("admin/shutdown");
        let resp = self
            .http_client
            .post(self.url.as_str())
            .json(&ShutdownReq { secret })
            .bearer_auth(&self.credential)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn quit(self) {}

    pub async fn handle_command<T>(&mut self, cmd: T) -> bool
    where
        T: Into<ClientCommand>,
    {
        let cmd = cmd.into();
        match cmd {
            ClientCommand::Auth(args) => match self.auth_user(args.into()).await {
                Ok(_) => {
                    tracing::info!("Successfully authenticated");
                }
                Err(e) => {
                    tracing::error!("{}", e);
                }
            },
            ClientCommand::Create(create_args) => match create_args.command {
                CreateCommands::User(args) => match self.create_user(args).await {
                    Ok(_) => {
                        tracing::info!("Successfully created user");
                    }
                    Err(e) => {
                        tracing::error!("{}", e);
                    }
                },
                CreateCommands::Group(args) => match self.create_group(args).await {
                    Ok(_) => {
                        tracing::info!("Successfully created group");
                    }
                    Err(e) => {
                        tracing::error!("{}", e);
                    }
                },
            },
            ClientCommand::Get(args) => match args.command {
                GetCommands::Task(args) => match self.get_task(args).await {
                    Ok(resp) => {
                        output_parsed_task_info(&resp.info);
                        if resp.artifacts.is_empty() {
                            tracing::info!("Artifacts: None");
                        } else {
                            for artifact in resp.artifacts {
                                tracing::info!(
                                    "Artifacts: {} of Size {}B, Created at {} and Updated at {}",
                                    artifact.content_type,
                                    artifact.size,
                                    artifact.created_at,
                                    artifact.updated_at
                                );
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!("{}", e);
                    }
                },
                GetCommands::Artifact(args) => match self.get_artifact(args.into()).await {
                    Ok(info) => {
                        tracing::info!(
                            "Artifact of size {}B downloaded to {}",
                            info.size,
                            info.local_path.display()
                        );
                    }
                    Err(e) => {
                        tracing::error!("{}", e);
                    }
                },
                GetCommands::Tasks(args) => {
                    let verbose = args.verbose;
                    match self.get_tasks(args.into()).await {
                        Ok(tasks) => {
                            if verbose {
                                for task in tasks {
                                    output_task_info(&task);
                                }
                            } else {
                                for task in tasks {
                                    tracing::info!("{}", task.uuid);
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!("{}", e);
                        }
                    }
                }
                GetCommands::Attachment(args) => {
                    if args.no_download {
                        match self.get_attachment_url(args.into()).await {
                            Ok(url) => {
                                tracing::info!("Attachment URL: {}", url);
                            }
                            Err(e) => {
                                tracing::error!("{}", e);
                            }
                        }
                    } else {
                        match self.get_attachment(args.into()).await {
                            Ok(info) => {
                                tracing::info!(
                                    "Attachment of size {}B downloaded to {}",
                                    info.size,
                                    info.local_path.display()
                                );
                            }
                            Err(e) => {
                                tracing::error!("{}", e);
                            }
                        }
                    }
                }
                GetCommands::AttachmentMeta(args) => match self.get_attachment_meta(args).await {
                    Ok(info) => {
                        tracing::info!(
                            "Attachment of size {}B, Created at {} and Updated at {}",
                            info.size,
                            info.created_at,
                            info.updated_at
                        );
                    }
                    Err(e) => {
                        tracing::error!("{}", e);
                    }
                },
                GetCommands::Attachments(args) => match self.get_attachments(args.into()).await {
                    Ok(attachments) => {
                        for attachment in attachments {
                            tracing::info!(
                                "Attachment {} of size {}B, Created at {} and Updated at {}",
                                attachment.key,
                                attachment.size,
                                attachment.created_at,
                                attachment.updated_at
                            );
                        }
                    }
                    Err(e) => {
                        tracing::error!("{}", e);
                    }
                },
                GetCommands::Worker(args) => match self.get_worker(args).await {
                    Ok(resp) => {
                        output_worker_info(&resp.info, &resp.groups);
                    }
                    Err(e) => {
                        tracing::error!("{}", e);
                    }
                },
                GetCommands::Workers(args) => {
                    let verbose = args.verbose;
                    match self.get_workers(args.into()).await {
                        Ok(resp) => {
                            let WorkersQueryResp {
                                workers,
                                group_name,
                            } = resp;
                            if verbose {
                                for worker in workers {
                                    output_worker_list_info(&worker, &group_name);
                                }
                            } else {
                                for worker in workers {
                                    tracing::info!("{}", worker.worker_id);
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!("{}", e);
                        }
                    }
                }
                GetCommands::Group(args) => match self.get_group(args).await {
                    Ok(resp) => {
                        output_group_info(&resp);
                    }
                    Err(e) => {
                        tracing::error!("{}", e);
                    }
                },
                GetCommands::Groups => match self.get_groups().await {
                    Ok(resp) => {
                        for (group, role) in resp.groups {
                            tracing::info!("{} = {}", group, role);
                        }
                    }
                    Err(e) => {
                        tracing::error!("{}", e);
                    }
                },
            },
            ClientCommand::Submit(args) => {
                let group_name = args.group_name.clone().unwrap_or(self.username.clone());
                match self.submit_task(args.into()).await {
                    Ok(resp) => {
                        tracing::info!(
                            "Task submitted with id {} in group {} and a global uuid {}",
                            resp.task_id,
                            group_name,
                            resp.uuid
                        );
                    }
                    Err(e) => {
                        tracing::error!("{}", e);
                    }
                }
            }
            ClientCommand::Upload(args) => match args.command {
                UploadCommands::Artifact(args) => match self.upload_artifact(args).await {
                    Ok(_) => {
                        tracing::info!("Artifact uploaded successfully");
                    }
                    Err(e) => {
                        tracing::error!("{}", e);
                    }
                },
                UploadCommands::Attachment(args) => match self.upload_attachment(args).await {
                    Ok(_) => {
                        tracing::info!("Attachment uploaded successfully");
                    }
                    Err(e) => {
                        tracing::error!("{}", e);
                    }
                },
            },
            ClientCommand::Manage(args) => match args.command {
                ManageCommands::Worker(args) => {
                    let uuid = args.uuid;
                    match args.command {
                        ManageWorkerCommands::Cancel(args) => {
                            match self.cancel_worker(uuid, args).await {
                                Ok(_) => {
                                    tracing::info!("Worker cancelled successfully");
                                }
                                Err(e) => {
                                    tracing::error!("{}", e);
                                }
                            }
                        }
                        ManageWorkerCommands::UpdateTags(args) => {
                            match self.replace_worker_tags(uuid, args.into()).await {
                                Ok(_) => {
                                    tracing::info!("Worker tags updated successfully");
                                }
                                Err(e) => {
                                    tracing::error!("{}", e);
                                }
                            }
                        }
                        ManageWorkerCommands::UpdateRoles(args) => {
                            match self.update_group_worker_roles(uuid, args.into()).await {
                                Ok(_) => {
                                    tracing::info!("Group worker roles updated successfully");
                                }
                                Err(e) => {
                                    tracing::error!("{}", e);
                                }
                            }
                        }
                        ManageWorkerCommands::RemoveRoles(args) => {
                            match self.remove_group_worker_roles(uuid, args.into()).await {
                                Ok(_) => {
                                    tracing::info!("Group worker roles removed successfully");
                                }
                                Err(e) => {
                                    tracing::error!("{}", e);
                                }
                            }
                        }
                    }
                }
                ManageCommands::Task(args) => {
                    let uuid = args.uuid;
                    match args.command {
                        ManageTaskCommands::Cancel => match self.cancel_task(uuid).await {
                            Ok(_) => {
                                tracing::info!("Task cancelled successfully");
                            }
                            Err(e) => {
                                tracing::error!("{}", e);
                            }
                        },
                        ManageTaskCommands::UpdateLabels(args) => {
                            match self.update_task_labels(uuid, args.into()).await {
                                Ok(_) => {
                                    tracing::info!("Task labels updated successfully");
                                }
                                Err(e) => {
                                    tracing::error!("{}", e);
                                }
                            }
                        }
                        ManageTaskCommands::Change(args) => {
                            match self.change_task(uuid, args.into()).await {
                                Ok(_) => {
                                    tracing::info!("Task changed successfully");
                                }
                                Err(e) => {
                                    tracing::error!("{}", e);
                                }
                            }
                        }
                    }
                }
                ManageCommands::Group(args) => {
                    let group = args.group;
                    match args.command {
                        ManageGroupCommands::Update(args) => {
                            match self.update_user_group_roles(group, args.into()).await {
                                Ok(_) => {
                                    tracing::info!("User group roles updated successfully");
                                }
                                Err(e) => {
                                    tracing::error!("{}", e);
                                }
                            }
                        }
                        ManageGroupCommands::Remove(args) => {
                            match self.remove_user_group_roles(group, args.into()).await {
                                Ok(_) => {
                                    tracing::info!("User group roles removed successfully");
                                }
                                Err(e) => {
                                    tracing::error!("{}", e);
                                }
                            }
                        }
                    }
                }
            },
            ClientCommand::Shutdown(args) => match self.shutdown_coordinator(args.secret).await {
                Ok(_) => {
                    tracing::info!("Coordinator shutdown successfully");
                    return false;
                }
                Err(e) => {
                    tracing::error!("{}", e);
                }
            },
            ClientCommand::Quit => {
                return false;
            }
        }
        true
    }

    fn gen_submit_task_req(&self, args: SubmitTaskArgs) -> SubmitTaskReq {
        let mut req = SubmitTaskReq {
            group_name: args.group_name.unwrap_or(self.username.clone()),
            tags: args.tags,
            labels: args.labels,
            timeout: args.timeout,
            priority: args.priority,
            task_spec: args.task_spec,
        };
        req.task_spec.resources.iter_mut().for_each(|r| {
            if let RemoteResource::Attachment { ref key } = r.remote_file {
                r.remote_file = RemoteResource::Attachment {
                    key: path_clean::clean(key).display().to_string(),
                };
            }
        });
        req
    }
}

fn output_parsed_task_info(info: &ParsedTaskQueryInfo) {
    tracing::info!("Task UUID: {}", info.uuid);
    tracing::info!("State: {}", info.state);
    tracing::info!(
        "Created by user {} as the #{} task in Group {}",
        info.creator_username,
        info.task_id,
        info.group_name
    );
    tracing::info!("Tags: {:?}", info.tags);
    tracing::info!("Labels: {:?}", info.labels);
    let timeout = std::time::Duration::from_secs(info.timeout as u64);
    tracing::info!("Timeout {:?} and Priority {}", timeout, info.priority);
    tracing::info!(
        "Created at {} and Updated at {}",
        info.created_at,
        info.updated_at
    );
    tracing::info!("Task Spec: {:?}", info.spec);
    if let Some(result) = &info.result {
        tracing::info!("Task Result: {:?}", result);
    } else {
        tracing::info!("Task Result: None");
    }
}

fn output_task_info(info: &TaskQueryInfo) {
    tracing::info!("Task UUID: {}", info.uuid);
    tracing::info!("State: {}", info.state);
    tracing::info!(
        "Created by user {} as the #{} task in Group {}",
        info.creator_username,
        info.task_id,
        info.group_name
    );
    tracing::info!("Tags: {:?}", info.tags);
    tracing::info!("Labels: {:?}", info.labels);
    let timeout = std::time::Duration::from_secs(info.timeout as u64);
    tracing::info!("Timeout {:?} and Priority {}", timeout, info.priority);
    tracing::info!(
        "Created at {} and Updated at {}",
        info.created_at,
        info.updated_at
    );
    tracing::info!("Task Spec: {}", info.spec);
    if let Some(result) = &info.result {
        tracing::info!("Task Result: {}", result);
    } else {
        tracing::info!("Task Result: None");
    }
}

fn output_worker_list_info<T: std::fmt::Display>(info: &WorkerQueryInfo, group_name: &T) {
    tracing::info!("Worker UUID: {}", info.worker_id);
    tracing::info!("Tags: {:?}", info.tags);
    tracing::info!("State: {}", info.state);
    tracing::info!(
        "Created by user {} for group {}",
        info.creator_username,
        group_name
    );
    tracing::info!(
        "Created at {} and Updated at {}",
        info.created_at,
        info.updated_at
    );
    tracing::info!("Last Heartbeat: {}", info.last_heartbeat);
    if let Some(task) = info.assigned_task_id {
        tracing::info!("Assigned Task: {}", task);
    } else {
        tracing::info!("Assigned Task: None");
    }
}

fn output_worker_info(
    info: &WorkerQueryInfo,
    groups: &std::collections::HashMap<String, GroupWorkerRole>,
) {
    tracing::info!("Worker UUID: {}", info.worker_id);
    tracing::info!("Tags: {:?}", info.tags);
    tracing::info!("State: {}", info.state);
    tracing::info!("Accessible Groups: {:?}", groups);
    tracing::info!("Created by user {} ", info.creator_username,);
    tracing::info!(
        "Created at {} and Updated at {}",
        info.created_at,
        info.updated_at
    );
    tracing::info!("Last Heartbeat: {}", info.last_heartbeat);
    if let Some(task) = info.assigned_task_id {
        tracing::info!("Assigned Task: {}", task);
    } else {
        tracing::info!("Assigned Task: None");
    }
}

fn output_group_info(info: &GroupQueryInfo) {
    tracing::info!("Group Name: {}", info.group_name);
    tracing::info!("Created by user {}", info.creator_username);
    tracing::info!(
        "Created at {} and Updated at {}",
        info.created_at,
        info.updated_at
    );
    tracing::info!("State: {}", info.state);
    tracing::info!("Contains {} tasks", info.task_count);
    tracing::info!(
        "Storage(used/total): {} / {}",
        format_size((info.storage_used).max(0) as u64, DECIMAL),
        format_size((info.storage_quota).max(0) as u64, DECIMAL)
    );
    tracing::info!("Can access {} workers", info.worker_count);
    if let Some(ref users) = info.users_in_group {
        tracing::info!("Users in the group:");
        for (user, role) in users {
            tracing::info!(" > {} = {}", user, role);
        }
    }
}

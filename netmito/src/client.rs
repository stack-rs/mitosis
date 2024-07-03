use clap_repl::ClapEditor;
use redis::{
    aio::{MultiplexedConnection, PubSub},
    AsyncCommands, Msg,
};
use reqwest::Client;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use url::Url;
use uuid::Uuid;

use crate::{
    config::{
        client::{
            ClientCommand, ClientInteractiveShell, CreateCommands, CreateGroupArgs, CreateUserArgs,
            GetArtifactArgs, GetCommands, GetTaskArgs, SubmitTaskArgs,
        },
        ClientConfig, ClientConfigCli,
    },
    entity::state::TaskExecState,
    error::{get_error_from_resp, map_reqwest_err, RequestError},
    schema::{
        CreateGroupReq, CreateUserReq, RedisConnectionInfo, RemoteResourceDownloadResp,
        ResourceDownloadInfo, SubmitTaskReq, SubmitTaskResp, TaskQueryResp,
    },
    service::{auth::cred::get_user_credential, s3::download_file},
};

pub struct MitoClient {
    http_client: Client,
    url: Url,
    credential: String,
    username: String,
    redis: Option<(redis::Client, MultiplexedConnection, PubSub)>,
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
                        tracing::info!("Client is running in interactive mode. Enter 'quit' to exit and 'help' to see available commands.");
                        let mut rl = ClapEditor::<ClientInteractiveShell>::new_with_prompt(
                            "[mito::client]> ",
                        );
                        loop {
                            if let Some(c) = rl.read_command() {
                                if !client.handle_command(c.command).await {
                                    break;
                                }
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
            username,
            redis: None,
        })
    }

    pub async fn setup_redis(&mut self) -> crate::error::Result<()> {
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
            if let Some(redis_url) = resp.url {
                let client = redis::Client::open(redis_url)?;
                let con = client.get_multiplexed_tokio_connection().await?;
                let pubsub = client.get_async_pubsub().await?;
                self.redis = Some((client, con, pubsub));
            } else {
                tracing::warn!("No Redis connection info found from coordinator");
            }
            Ok(())
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn get_task_exec_state(
        &mut self,
        uuid: &Uuid,
    ) -> crate::error::Result<TaskExecState> {
        if let Some((_, con, _)) = self.redis.as_mut() {
            let state: i32 = con.get(format!("task:{}", uuid)).await?;
            Ok(TaskExecState::from(state))
        } else {
            Err(crate::error::Error::Custom(
                "No Redis connection found".to_string(),
            ))
        }
    }

    pub async fn subscribe_task_exec_state(&mut self, uuid: &Uuid) -> crate::error::Result<()> {
        if let Some((_, _, pubsub)) = self.redis.as_mut() {
            pubsub.subscribe(format!("task:{}", uuid)).await?;
            Ok(())
        } else {
            Err(crate::error::Error::Custom(
                "No Redis connection found".to_string(),
            ))
        }
    }

    pub async fn on_task_exec_state_message(
        &mut self,
    ) -> crate::error::Result<impl futures::stream::Stream<Item = Msg> + '_> {
        if let Some((_, _, pubsub)) = self.redis.as_mut() {
            Ok(pubsub.on_message())
        } else {
            Err(crate::error::Error::Custom(
                "No Redis connection found".to_string(),
            ))
        }
    }

    pub async fn unsubscribe_task_exec_state(&mut self, uuid: &Uuid) -> crate::error::Result<()> {
        if let Some((_, _, pubsub)) = self.redis.as_mut() {
            pubsub.unsubscribe(format!("task:{}", uuid)).await?;
            Ok(())
        } else {
            Err(crate::error::Error::Custom(
                "No Redis connection found".to_string(),
            ))
        }
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
        self.url.set_path("group");
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
        self.url.set_path(&format!("user/task/{}", args.uuid));
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

    pub async fn submit_task(
        &mut self,
        args: SubmitTaskArgs,
    ) -> crate::error::Result<SubmitTaskResp> {
        self.url.set_path("user/task");
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

    pub async fn quit(self) {}

    pub async fn handle_command<T>(&mut self, cmd: T) -> bool
    where
        T: Into<ClientCommand>,
    {
        let cmd = cmd.into();
        match cmd {
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
                        tracing::info!("Task UUID: {}", resp.uuid);
                        tracing::info!("State: {}", resp.state);
                        tracing::info!(
                            "Created by user {} as the #{} task in Group {}",
                            resp.creator_username,
                            resp.task_id,
                            resp.group_name
                        );
                        tracing::info!("Tags: {:?}", resp.tags);
                        tracing::info!("Labels: {:?}", resp.labels);
                        let timeout = std::time::Duration::from_secs(resp.timeout as u64);
                        tracing::info!("Timeout {:?} and Priority {}", timeout, resp.priority);
                        tracing::info!(
                            "Created at {} and Updated at {}",
                            resp.created_at,
                            resp.updated_at
                        );
                        tracing::info!("Task Spec: {:?}", resp.spec);
                        if let Some(result) = resp.result {
                            tracing::info!("Task Result: {:?}", result);
                        } else {
                            tracing::info!("Task Result: None");
                        }
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
            ClientCommand::Quit => {
                return false;
            }
        }
        true
    }

    fn gen_submit_task_req(&self, args: SubmitTaskArgs) -> SubmitTaskReq {
        SubmitTaskReq {
            group_name: args.group_name.unwrap_or(self.username.clone()),
            tags: args.tags,
            labels: args.labels,
            timeout: args.timeout,
            priority: args.priority,
            task_spec: args.task_spec,
        }
    }
}

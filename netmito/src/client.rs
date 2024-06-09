use std::path::Path;

use clap_repl::ClapEditor;
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
    error::{get_error_from_resp, map_reqwest_err, RequestError},
    schema::{
        CreateGroupReq, CreateUserReq, RemoteResourceDownloadResp, ResourceDownloadInfo,
        SubmitTaskReq, SubmitTaskResp, TaskQueryResp, TaskSpec,
    },
    service::{auth::cred::get_user_credential, s3::download_file},
};

pub struct MitoClient {
    config: ClientConfig,
    http_client: Client,
    url: Url,
    credential: String,
    username: String,
}

impl MitoClient {
    pub async fn main(cli: ClientConfigCli) {
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "netmito=info".into()),
            )
            .with(tracing_subscriber::fmt::layer())
            .init();
        match Self::setup(&cli).await {
            Ok(mut client) => {
                if let Err(e) = client.run().await {
                    tracing::error!("{}", e);
                }
            }
            Err(e) => {
                tracing::error!("{}", e);
            }
        }
    }

    pub async fn setup(cli: &ClientConfigCli) -> crate::error::Result<Self> {
        tracing::debug!("Client is setting up");
        let config = ClientConfig::new(cli)?;
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
            config,
            http_client,
            url,
            credential,
            username,
        })
    }

    pub async fn run(&mut self) -> crate::error::Result<()> {
        if let Some(cmd) = self.config.command.take() {
            self.handle_command(cmd).await;
        }
        if self.config.interactive {
            tracing::info!("Client is running in interactive mode. Enter 'quit' to exit and 'help' to see available commands.");
            let mut rl = ClapEditor::<ClientInteractiveShell>::new_with_prompt("[mito::client]> ");
            loop {
                if let Some(c) = rl.read_command() {
                    if !self.handle_command(c.command).await {
                        break;
                    }
                }
            }
        }

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
        let uuid = Uuid::parse_str(&args.uuid)?;
        self.url.set_path(&format!("user/task/{}", uuid));
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
            let local_path = args
                .output_path
                .and_then(|dir| {
                    let dir = Path::new(&dir);
                    if dir.is_dir() {
                        let file_name = args.content_type.to_string();
                        Some(dir.join(file_name))
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| {
                    let file_name = args.content_type.to_string();
                    Path::new("./").join(file_name)
                });
            download_file(&self.http_client, &download_resp, &local_path).await?;
            Ok(ResourceDownloadInfo {
                size: download_resp.size,
                local_path,
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
                GetCommands::Artifact(args) => match self.get_artifact(args).await {
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
                match self.submit_task(args).await {
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
            tags: args.tags.into_iter().collect(),
            timeout: args.timeout,
            priority: args.priority,
            task_spec: TaskSpec::new(
                args.shell,
                args.spec,
                args.envs,
                args.resources,
                args.terminal_output,
            ),
        }
    }
}

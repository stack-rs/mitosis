use std::path::Path;

use clap_repl::ClapEditor;
use reqwest::Client;
use tokio::{fs::File, io::AsyncWriteExt};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use url::Url;
use uuid::Uuid;

use crate::{
    config::{
        client::{
            ClientCommand, ClientInteractiveShell, CreateCommands, GetCommands, SubmitTaskArgs,
        },
        ClientConfig, ClientConfigCli,
    },
    error::{ErrorMsg, RequestError},
    schema::{
        ArtifactDownloadResp, CreateGroupReq, CreateUserReq, SubmitTaskReq, SubmitTaskResp,
        TaskQueryResp, TaskSpec,
    },
    service::auth::cred::get_user_credential,
};

pub struct MitoClient {
    config: ClientConfig,
    http_client: Client,
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
        Ok(MitoClient {
            config,
            http_client,
            credential,
            username,
        })
    }

    pub async fn run(&mut self) -> crate::error::Result<()> {
        let mut url = self.config.coordinator_addr.clone();
        if let Some(cmd) = self.config.command.take() {
            self.handle_command(&mut url, cmd).await;
        }
        if self.config.interactive {
            tracing::info!("Client is running in interactive mode. Enter 'quit' to exit and 'help' to see available commands.");
            let mut rl = ClapEditor::<ClientInteractiveShell>::new_with_prompt("[mito::client]> ");
            loop {
                if let Some(c) = rl.read_command() {
                    if !self.handle_command(&mut url, c.command).await {
                        break;
                    }
                }
            }
        }

        Ok(())
    }

    async fn handle_command(&self, url: &mut Url, cmd: ClientCommand) -> bool {
        match cmd {
            ClientCommand::Create(create_args) => match create_args.command {
                CreateCommands::User(args) => {
                    url.set_path("admin/user");
                    let req = CreateUserReq {
                        username: args.username,
                        md5_password: md5::compute(args.password.as_bytes()).0,
                        admin: args.admin,
                    };
                    match self
                        .http_client
                        .post(url.as_str())
                        .json(&req)
                        .bearer_auth(&self.credential)
                        .send()
                        .await
                        .map_err(|e| {
                            if e.is_request() && e.is_connect() {
                                url.set_path("");
                                RequestError::ConnectionError(url.to_string())
                            } else {
                                e.into()
                            }
                        }) {
                        Ok(resp) => {
                            if resp.status().is_success() {
                                tracing::info!("Successfully created user");
                            } else {
                                let resp: ErrorMsg = resp
                                    .json()
                                    .await
                                    .unwrap_or_else(|e| ErrorMsg { msg: e.to_string() });
                                tracing::error!("{}", resp.msg);
                            }
                        }
                        Err(e) => {
                            tracing::error!("{}", e);
                        }
                    }
                }
                CreateCommands::Group(args) => {
                    url.set_path("group");
                    let req = CreateGroupReq {
                        group_name: args.name,
                    };
                    match self
                        .http_client
                        .post(url.as_str())
                        .json(&req)
                        .bearer_auth(&self.credential)
                        .send()
                        .await
                    {
                        Ok(resp) => {
                            if resp.status().is_success() {
                                tracing::info!("Successfully created group");
                            } else {
                                let resp: ErrorMsg = resp
                                    .json()
                                    .await
                                    .unwrap_or_else(|e| ErrorMsg { msg: e.to_string() });
                                tracing::error!("{}", resp.msg);
                            }
                        }
                        Err(e) => {
                            tracing::error!("{}", e);
                        }
                    }
                }
            },
            ClientCommand::Get(args) => match args.command {
                GetCommands::Task(args) => {
                    let uuid = match Uuid::parse_str(&args.uuid) {
                        Ok(uuid) => uuid,
                        Err(e) => {
                            tracing::error!("Fail to parse uuid: {}", e);
                            return true;
                        }
                    };
                    url.set_path(&format!("user/task/{}", uuid));
                    match self
                        .http_client
                        .get(url.as_str())
                        .bearer_auth(&self.credential)
                        .send()
                        .await
                    {
                        Ok(resp) => {
                            if resp.status().is_success() {
                                match resp.json::<TaskQueryResp>().await {
                                    Ok(resp) => {
                                        tracing::info!("{:?}", resp);
                                        tracing::info!("Task UUID: {}", resp.uuid);
                                        tracing::info!("State: {}", resp.state);
                                        tracing::info!(
                                            "Created by {} as the #{} task in Group {}",
                                            resp.creator_username,
                                            resp.task_id,
                                            resp.group_name
                                        );
                                        tracing::info!("Tags: {:?}", resp.tags);
                                        let timeout =
                                            std::time::Duration::from_secs(resp.timeout as u64);
                                        tracing::info!(
                                            "Timeout {:?} and Priority {}",
                                            timeout,
                                            resp.priority
                                        );
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
                                }
                            } else {
                                let resp: ErrorMsg = resp
                                    .json()
                                    .await
                                    .unwrap_or_else(|e| ErrorMsg { msg: e.to_string() });
                                tracing::error!("{}", resp.msg);
                            }
                        }
                        Err(e) => {
                            tracing::error!("{}", e);
                        }
                    }
                }
                GetCommands::Artifact(args) => {
                    let file_path = args
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
                    let content_serde_str = match serde_json::to_string(&args.content_type) {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::error!("{}", e);
                            return true;
                        }
                    };
                    url.set_path(&format!(
                        "user/artifacts/{}/{}",
                        args.uuid, content_serde_str
                    ));
                    match self
                        .http_client
                        .get(url.as_str())
                        .bearer_auth(&self.credential)
                        .send()
                        .await
                    {
                        Ok(resp) => {
                            if resp.status().is_success() {
                                match resp.json::<ArtifactDownloadResp>().await {
                                    Ok(resp) => {
                                        match self.http_client.get(&resp.url).send().await {
                                            Ok(mut resp) => {
                                                if resp.status().is_success() {
                                                    let mut file =
                                                        match File::create(&file_path).await {
                                                            Ok(file) => file,
                                                            Err(e) => {
                                                                tracing::error!("{}", e);
                                                                return true;
                                                            }
                                                        };
                                                    let hd = async {
                                                        while let Some(chunk) =
                                                            resp.chunk().await.map_err(|e| {
                                                                RequestError::Custom(e)
                                                            })?
                                                        {
                                                            file.write_all(&chunk).await?;
                                                        }
                                                        crate::error::Result::Ok(())
                                                    };
                                                    match hd.await {
                                                        Ok(_) => {
                                                            tracing::info!(
                                                                "Artifact {} downloaded to {}",
                                                                content_serde_str,
                                                                file_path.display()
                                                            );
                                                        }
                                                        Err(e) => {
                                                            tracing::error!("{}", e);
                                                        }
                                                    }
                                                } else {
                                                    let resp: ErrorMsg =
                                                        resp.json().await.unwrap_or_else(|e| {
                                                            ErrorMsg { msg: e.to_string() }
                                                        });
                                                    tracing::error!("{}", resp.msg);
                                                }
                                            }
                                            Err(e) => {
                                                tracing::error!("{}", e);
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        tracing::error!("{}", e);
                                    }
                                }
                            } else {
                                let resp: ErrorMsg = resp
                                    .json()
                                    .await
                                    .unwrap_or_else(|e| ErrorMsg { msg: e.to_string() });
                                tracing::error!("{}", resp.msg);
                            }
                        }
                        Err(e) => {
                            tracing::error!("{}", e);
                        }
                    }
                }
            },
            ClientCommand::Submit(args) => {
                url.set_path("user/task");
                let req = self.gen_submit_task_req(args);
                match self
                    .http_client
                    .post(url.as_str())
                    .json(&req)
                    .bearer_auth(&self.credential)
                    .send()
                    .await
                {
                    Ok(resp) => {
                        if resp.status().is_success() {
                            match resp.json::<SubmitTaskResp>().await {
                                Ok(resp) => {
                                    tracing::info!(
                                        "Task submitted with id {} in group {} and global uuid {}",
                                        resp.task_id,
                                        req.group_name,
                                        resp.uuid
                                    );
                                }
                                Err(e) => {
                                    tracing::error!("{}", e);
                                }
                            }
                        } else {
                            let resp: ErrorMsg = resp
                                .json()
                                .await
                                .unwrap_or_else(|e| ErrorMsg { msg: e.to_string() });
                            tracing::error!("{}", resp.msg);
                        }
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
            task_spec: TaskSpec::new(args.shell, args.spec, args.envs, args.terminal_output),
        }
    }
}

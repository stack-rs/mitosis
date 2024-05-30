use clap_repl::ClapEditor;
use reqwest::Client;
use url::Url;

use crate::{
    config::{
        client::{ClientCommand, ClientInteractiveShell, CreateCommands, SubmitTaskArgs},
        ClientConfig, ClientConfigCli,
    },
    error::{ErrorMsg, RequestError},
    schema::{CreateGroupReq, CreateUserReq, SubmitTaskReq, SubmitTaskResp, TaskSpec},
    service::auth::cred::get_user_credential,
};

pub struct MitoClient {
    config: ClientConfig,
    http_client: Client,
    credential: String,
    username: String,
}

impl MitoClient {
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
            ClientCommand::Get => {
                // TODO
                tracing::error!("Get command is not supported yet");
            }
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
            task_spec: TaskSpec {
                command: args.shell,
                args: args.spec,
                terminal_output: args.terminal_output,
            },
        }
    }
}

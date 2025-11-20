use std::{io::Write, process::Stdio};

use clap_repl::ReadCommandOutput;
use http::MitoHttpClient;
use humansize::{format_size, DECIMAL};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use uuid::Uuid;

use crate::{
    config::{client::*, ClientConfig, ClientConfigCli},
    entity::state::TaskExecState,
    schema::*,
    service::auth::fill_user_login,
};

pub mod http;
pub mod interactive;
pub mod redis;
pub use interactive::*;
pub use redis::*;

pub struct MitoClient {
    http_client: http::MitoHttpClient,
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
            .with(tracing_subscriber::fmt::layer().with_target(false))
            .init();
        match ClientConfig::new(&cli) {
            Ok(config) => match Self::setup(config).await {
                Ok(mut client) => {
                    if let Some(cmd) = cli.command.take() {
                        client.handle_command(cmd).await;
                    }
                    if cli.interactive {
                        let username = client.username.as_str();
                        println!("Logged in as {username}. Client is running in interactive mode.");
                        println!(
                            "Enter 'quit', 'exit' or Ctrl-D to exit and 'help' to see available commands."
                        );
                        let cache_file = dirs::cache_dir().map(|mut p| {
                            p.push("mitosis");
                            p.push("client-history");
                            p
                        });
                        let mut rl = get_interactive_shell(cache_file);
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

    pub async fn setup(config: ClientConfig) -> crate::error::Result<Self> {
        tracing::debug!("Client is setting up");
        let mut http_client = MitoHttpClient::new(config.coordinator_addr);
        let username = http_client
            .connect(
                config.credential_path,
                config.user,
                config.password,
                config.retain,
            )
            .await?;
        Ok(MitoClient {
            http_client,
            username,
            redis_client: None,
            redis_pubsub_client: None,
            async_redis_client: None,
        })
    }

    pub fn http_client(&self) -> &http::MitoHttpClient {
        &self.http_client
    }

    pub fn http_client_mut(&mut self) -> &mut http::MitoHttpClient {
        &mut self.http_client
    }

    pub async fn get_redis_connection_info(&mut self) -> crate::error::Result<RedisConnectionInfo> {
        self.http_client.get_redis_connection_info().await
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

    pub async fn user_login(&mut self, args: LoginArgs) -> crate::error::Result<()> {
        let req = fill_user_login(args.username, args.password, args.retain)?;
        self.http_client.user_login(req).await
    }

    pub async fn user_auth(&mut self) -> crate::error::Result<String> {
        self.http_client.user_auth().await
    }

    pub async fn admin_change_user_password(
        &mut self,
        args: ChangePasswordArgs,
    ) -> crate::error::Result<()> {
        let (username, req) = fill_admin_change_password(args.username, args.new_password)?;
        self.http_client.admin_change_password(username, req).await
    }

    pub async fn user_change_password(
        &mut self,
        args: UserChangePasswordArgs,
    ) -> crate::error::Result<()> {
        let (username, req) = fill_user_change_password(
            Some(self.username.clone()),
            args.orig_password,
            args.new_password,
        )?;
        self.http_client.user_change_password(username, req).await
    }

    pub async fn admin_users_create(
        &mut self,
        args: AdminCreateUserArgs,
    ) -> crate::error::Result<()> {
        let req = fill_admin_create_user(args.username, args.password, args.admin)?;
        self.http_client.admin_create_user(req).await
    }

    pub async fn admin_users_delete(
        &mut self,
        args: AdminDeleteUserArgs,
    ) -> crate::error::Result<()> {
        self.http_client.admin_delete_user(args.username).await
    }

    pub async fn groups_create(&mut self, args: GroupCreateArgs) -> crate::error::Result<()> {
        let req = CreateGroupReq {
            group_name: args.group,
        };
        self.http_client.user_create_group(req).await
    }

    pub async fn tasks_get(&mut self, args: GetTaskArgs) -> crate::error::Result<TaskQueryResp> {
        self.http_client.get_task_by_uuid(args.uuid).await
    }

    pub async fn download_artifact(
        &mut self,
        args: DownloadArtifactArgs,
    ) -> crate::error::Result<ResourceDownloadInfo> {
        let output_path = args
            .output_path
            .map(|dir| {
                let dir = std::path::Path::new(&dir);
                if dir.is_dir() {
                    let file_name = args.content_type.to_string();
                    dir.join(file_name)
                } else {
                    dir.to_path_buf()
                }
            })
            .unwrap_or_else(|| {
                let file_name = args.content_type.to_string();
                std::path::Path::new("").join(file_name)
            });
        let download_resp = self
            .http_client
            .get_artifact_download_resp(args.uuid, args.content_type)
            .await?;
        self.http_client
            .download_file(&download_resp, &output_path, args.pb)
            .await?;
        Ok(ResourceDownloadInfo {
            size: download_resp.size,
            local_path: output_path,
        })
    }

    pub async fn get_artifact_download_url(
        &mut self,
        args: DownloadArtifactArgs,
    ) -> crate::error::Result<String> {
        let resp = self
            .http_client
            .get_artifact_download_resp(args.uuid, args.content_type)
            .await?;
        Ok(resp.url)
    }

    pub async fn tasks_artifacts_delete(
        &mut self,
        args: DeleteArtifactArgs,
    ) -> crate::error::Result<()> {
        self.http_client
            .delete_artifact(args.uuid, args.content_type, false)
            .await
    }

    pub async fn admin_tasks_artifacts_delete(
        &mut self,
        args: DeleteArtifactArgs,
    ) -> crate::error::Result<()> {
        self.http_client
            .delete_artifact(args.uuid, args.content_type, true)
            .await
    }

    async fn parse_download_attachment_args(
        &self,
        args: DownloadAttachmentArgs,
    ) -> InnerDownloadAttachmentArgs {
        fn get_fname(key: String) -> String {
            let p = std::path::Path::new(&key);
            let fname = p
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
                .unwrap_or(key);
            fname
        }
        let parse_key = |key: String, smart: bool| if smart { get_fname(key) } else { key };
        let group_key_parser = || match args.group_name {
            Some(g) => (args.key, g),
            None => {
                if args.smart {
                    if let Some((g, key)) = args.key.split_once('/') {
                        return (key.to_string(), g.to_string());
                    }
                }
                (args.key, self.username.clone())
            }
        };
        let (key, group_name) = group_key_parser();
        let output_path = args
            .output_path
            .map(|dir| {
                let dir = std::path::Path::new(&dir);
                if dir.is_dir() {
                    let file_name = parse_key(key.clone(), args.smart);
                    dir.join(file_name)
                } else {
                    dir.to_path_buf()
                }
            })
            .unwrap_or_else(|| {
                let file_name = parse_key(key.clone(), args.smart);
                std::path::Path::new("").join(file_name)
            });
        InnerDownloadAttachmentArgs {
            group_name,
            key,
            output_path,
            show_pb: args.pb,
        }
    }

    pub async fn download_attachment(
        &mut self,
        args: DownloadAttachmentArgs,
    ) -> crate::error::Result<ResourceDownloadInfo> {
        let args = self.parse_download_attachment_args(args).await;
        let download_resp = self
            .http_client
            .get_attachment_download_resp(&args.group_name, &args.key)
            .await?;
        self.http_client
            .download_file(&download_resp, &args.output_path, args.show_pb)
            .await?;
        Ok(ResourceDownloadInfo {
            size: download_resp.size,
            local_path: args.output_path,
        })
    }

    pub async fn get_attachment_download_url(
        &mut self,
        args: DownloadAttachmentArgs,
    ) -> crate::error::Result<String> {
        let args = self.parse_download_attachment_args(args).await;
        let resp = self
            .http_client
            .get_attachment_download_resp(&args.group_name, &args.key)
            .await?;
        Ok(resp.url)
    }

    pub async fn groups_attachments_get_metadata(
        &mut self,
        args: GetAttachmentMetaArgs,
    ) -> crate::error::Result<AttachmentMetadata> {
        let group_key_parser = || match args.group_name {
            Some(g) => (args.key, g),
            None => {
                if args.smart {
                    match args.key.split_once('/') {
                        Some((g, key)) => {
                            return (key.to_string(), g.to_string());
                        }
                        None => return (args.key, self.username.clone()),
                    }
                }
                (args.key, self.username.clone())
            }
        };
        let (key, group_name) = group_key_parser();
        self.http_client.get_attachment(&group_name, &key).await
    }

    pub async fn groups_attachments_delete(
        &mut self,
        args: DeleteAttachmentArgs,
    ) -> crate::error::Result<()> {
        let group_key_parser = || match args.group_name {
            Some(g) => (args.key, g),
            None => {
                if args.smart {
                    match args.key.split_once('/') {
                        Some((g, key)) => {
                            return (key.to_string(), g.to_string());
                        }
                        None => return (args.key, self.username.clone()),
                    }
                }
                (args.key, self.username.clone())
            }
        };
        let (key, group_name) = group_key_parser();
        self.http_client
            .delete_attachment(&group_name, &key, false)
            .await
    }

    pub async fn admin_groups_attachments_delete(
        &mut self,
        args: DeleteAttachmentArgs,
    ) -> crate::error::Result<()> {
        let group_key_parser = || match args.group_name {
            Some(g) => (args.key, g),
            None => {
                if args.smart {
                    match args.key.split_once('/') {
                        Some((g, key)) => {
                            return (key.to_string(), g.to_string());
                        }
                        None => return (args.key, self.username.clone()),
                    }
                }
                (args.key, self.username.clone())
            }
        };
        let (key, group_name) = group_key_parser();
        self.http_client
            .delete_attachment(&group_name, &key, true)
            .await
    }

    pub async fn admin_groups_update_storage_quota(
        &mut self,
        args: AdminUpdateGroupStorageQuotaArgs,
    ) -> crate::error::Result<GroupStorageQuotaResp> {
        let req = ChangeGroupStorageQuotaReq {
            storage_quota: args.storage_quota,
        };
        self.http_client
            .admin_update_group_storage_quota(&args.group_name, req)
            .await
    }

    pub async fn admin_users_update_group_quota(
        &mut self,
        args: AdminUpdateUserGroupQuotaArgs,
    ) -> crate::error::Result<UserGroupQuotaResp> {
        let req = ChangeUserGroupQuota {
            group_quota: args.group_quota,
        };
        self.http_client
            .admin_update_user_group_quota(&args.username, req)
            .await
    }

    pub async fn tasks_query(
        &mut self,
        args: QueryTasksArgs,
    ) -> crate::error::Result<TasksQueryResp> {
        self.http_client.query_tasks_by_filter(args.into()).await
    }

    pub async fn attachments_query(
        &mut self,
        args: QueryAttachmentsArgs,
    ) -> crate::error::Result<AttachmentsQueryResp> {
        let group_name = args.group.unwrap_or_else(|| self.username.clone());
        let req = AttachmentsQueryReq {
            key: args.key,
            count: args.count,
            limit: args.limit,
            offset: args.offset,
        };
        self.http_client
            .query_attachments_by_filter(&group_name, req)
            .await
    }

    pub async fn workers_get(
        &mut self,
        args: GetWorkerArgs,
    ) -> crate::error::Result<WorkerQueryResp> {
        self.http_client.get_worker_by_uuid(args.uuid).await
    }

    pub async fn workers_query(
        &mut self,
        args: WorkersQueryArgs,
    ) -> crate::error::Result<WorkersQueryResp> {
        self.http_client.query_workers_by_filter(args.into()).await
    }

    pub async fn groups_get(&mut self, args: GroupGetArgs) -> crate::error::Result<GroupQueryInfo> {
        self.http_client.get_group_by_name(&args.group).await
    }

    pub async fn groups_query(&mut self) -> crate::error::Result<GroupsQueryResp> {
        self.http_client.get_user_groups_roles().await
    }

    pub async fn tasks_submit(
        &mut self,
        args: SubmitTaskArgs,
    ) -> crate::error::Result<SubmitTaskResp> {
        let req = self.gen_submit_task_req(args);
        self.http_client.user_submit_task(req).await
    }

    pub async fn upload_artifact(
        &mut self,
        args: UploadArtifactArgs,
    ) -> crate::error::Result<bool> {
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
        let uuid = args.uuid;
        let req = UploadArtifactReq {
            content_length,
            content_type: args.content_type,
        };
        let resp = self.http_client.get_upload_artifact_resp(uuid, req).await?;
        self.http_client
            .upload_file(resp.url.as_str(), content_length, args.local_file, args.pb)
            .await?;
        Ok(resp.exist)
    }

    pub async fn upload_attachment(
        &mut self,
        args: UploadAttachmentArgs,
    ) -> crate::error::Result<bool> {
        fn get_fname<P: AsRef<std::path::Path>>(p: P) -> crate::error::Result<String> {
            let fname = p
                .as_ref()
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
                .ok_or(crate::error::Error::Custom(
                    "Complete key is not provided or local file is invalid".to_string(),
                ))?;
            Ok(fname)
        }
        let metadata = args
            .local_file
            .metadata()
            .map_err(crate::error::Error::from)?;
        if metadata.is_dir() {
            return Err(crate::error::Error::Custom(
                "Currently we do not support uploading a directory".to_string(),
            ));
        }
        let group_key_parser = || match args.group_name {
            Some(g) => (args.key, g),
            None => {
                if args.smart {
                    if let Some(k) = args.key {
                        match k.split_once('/') {
                            Some((g, key)) => {
                                return (Some(key.to_string()), g.to_string());
                            }
                            None => return (None, k),
                        }
                    }
                }
                (args.key, self.username.clone())
            }
        };
        let (key, group_name) = group_key_parser();
        let key = match key {
            Some(k) => {
                if k.ends_with('/') {
                    let file_name = get_fname(&args.local_file)?;
                    k + &file_name
                } else if k.is_empty() {
                    get_fname(&args.local_file)?
                } else {
                    k
                }
            }
            None => get_fname(&args.local_file)?,
        };
        let key = path_clean::clean(key).display().to_string();
        let content_length = metadata.len();
        let req = UploadAttachmentReq {
            key,
            content_length,
        };
        let resp = self
            .http_client
            .get_upload_attachment_resp(&group_name, req)
            .await?;
        self.http_client
            .upload_file(resp.url.as_str(), content_length, args.local_file, args.pb)
            .await?;
        Ok(resp.exist)
    }

    pub async fn admin_workers_cancel(
        &mut self,
        args: CancelWorkerArgs,
    ) -> crate::error::Result<()> {
        self.http_client
            .admin_cancel_worker_by_uuid(args.uuid, args.force)
            .await
    }

    pub async fn workers_cancel(&mut self, args: CancelWorkerArgs) -> crate::error::Result<()> {
        self.http_client
            .cancel_worker_by_uuid(args.uuid, args.force)
            .await
    }

    pub async fn workers_batch_cancel(
        &mut self,
        args: CancelWorkersArgs,
    ) -> crate::error::Result<WorkersShutdownByFilterResp> {
        self.http_client
            .shutdown_workers_by_filter(args.into())
            .await
    }

    pub async fn workers_batch_cancel_by_uuids(
        &mut self,
        args: CancelWorkersByUuidsArgs,
    ) -> crate::error::Result<WorkersShutdownByUuidsResp> {
        self.http_client
            .shutdown_workers_by_uuids(args.into())
            .await
    }

    pub async fn workers_update_tags(
        &mut self,
        args: WorkerUpdateTagsArgs,
    ) -> crate::error::Result<()> {
        self.http_client
            .replace_worker_tags(args.uuid, args.into())
            .await
    }

    pub async fn workers_update_labels(
        &mut self,
        args: WorkerUpdateLabelsArgs,
    ) -> crate::error::Result<()> {
        self.http_client
            .replace_worker_labels(args.uuid, args.into())
            .await
    }

    pub async fn workers_update_group_roles(
        &mut self,
        args: UpdateWorkerGroupArgs,
    ) -> crate::error::Result<()> {
        self.http_client
            .update_group_worker_roles(args.uuid, args.into())
            .await
    }

    pub async fn workers_remove_group_roles(
        &mut self,
        args: RemoveWorkerGroupArgs,
    ) -> crate::error::Result<()> {
        self.http_client
            .remove_group_worker_roles(args.uuid, args.into())
            .await
    }

    pub async fn tasks_cancel(&mut self, uuid: Uuid) -> crate::error::Result<()> {
        self.http_client.cancel_task_by_uuid(uuid).await
    }

    pub async fn tasks_batch_cancel(
        &mut self,
        args: CancelTasksArgs,
    ) -> crate::error::Result<TasksCancelByFilterResp> {
        self.http_client.cancel_tasks_by_filter(args.into()).await
    }

    pub async fn tasks_batch_cancel_by_uuids(
        &mut self,
        args: CancelTasksByUuidsArgs,
    ) -> crate::error::Result<TasksCancelByUuidsResp> {
        self.http_client.cancel_tasks_by_uuids(args.into()).await
    }

    pub async fn tasks_update_labels(
        &mut self,
        args: UpdateTaskLabelsArgs,
    ) -> crate::error::Result<()> {
        self.http_client
            .update_task_labels(args.uuid, args.into())
            .await
    }

    pub async fn tasks_change(&mut self, args: ChangeTaskArgs) -> crate::error::Result<()> {
        self.http_client.change_task(args.uuid, args.into()).await
    }

    pub async fn groups_update_user_roles(
        &mut self,
        args: GroupUpdateUserArgs,
    ) -> crate::error::Result<()> {
        let req = UpdateUserGroupRoleReq {
            relations: args.roles.into_iter().collect(),
        };
        self.http_client
            .update_user_group_roles(&args.group, req)
            .await
    }

    pub async fn groups_remove_user_roles(
        &mut self,
        args: RemoveUserGroupArgs,
    ) -> crate::error::Result<()> {
        let req = RemoveUserGroupRoleReq {
            users: args.users.into_iter().collect(),
        };
        self.http_client
            .remove_user_group_roles(&args.group, req)
            .await
    }

    pub async fn admin_shutdown_coordinator(
        &mut self,
        args: ShutdownArgs,
    ) -> crate::error::Result<()> {
        let req = ShutdownReq {
            secret: args.secret,
        };
        self.http_client.admin_shutdown_coordinator(req).await
    }

    pub async fn quit(self) {}

    pub async fn tasks_artifacts_upload(
        &mut self,
        args: TaskArtifactUploadArgs,
    ) -> crate::error::Result<bool> {
        self.upload_artifact(args).await
    }

    pub async fn tasks_artifacts_download(
        &mut self,
        args: TaskArtifactDownloadArgs,
    ) -> crate::error::Result<ResourceDownloadInfo> {
        self.download_artifact(args).await
    }

    pub async fn groups_attachments_upload(
        &mut self,
        args: GroupAttachmentUploadArgs,
    ) -> crate::error::Result<bool> {
        self.upload_attachment(args).await
    }

    pub async fn groups_attachments_download(
        &mut self,
        args: GroupAttachmentDownloadArgs,
    ) -> crate::error::Result<ResourceDownloadInfo> {
        self.download_attachment(args).await
    }

    pub async fn batch_download_artifacts_by_filter(
        &mut self,
        args: DownloadArtifactsByFilterArgs,
    ) -> crate::error::Result<Vec<ResourceDownloadInfo>> {
        let output_dir = args
            .output_dir
            .clone()
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let no_download = args.no_download;
        let show_pb = args.pb;
        let concurrent = args.concurrent;

        let resp = self
            .http_client
            .batch_download_artifacts_by_filter(args.into())
            .await?;

        if no_download {
            for item in &resp.downloads {
                tracing::info!("Task {}: {}", item.uuid, item.url);
            }
            return Ok(vec![]);
        }

        // Prepare download list
        let downloads: Vec<(RemoteResourceDownloadResp, std::path::PathBuf)> = resp
            .downloads
            .iter()
            .map(|item| {
                let file_name = format!("{}.tar.gz", item.uuid);
                let output_path = output_dir.join(file_name);
                (
                    RemoteResourceDownloadResp {
                        url: item.url.clone(),
                        size: item.size,
                    },
                    output_path,
                )
            })
            .collect();

        // Collect results for return
        let results: Vec<ResourceDownloadInfo> = resp
            .downloads
            .iter()
            .map(|item| {
                let file_name = format!("{}.tar.gz", item.uuid);
                let output_path = output_dir.join(file_name);
                ResourceDownloadInfo {
                    size: item.size,
                    local_path: output_path,
                }
            })
            .collect();

        // Download concurrently
        self.http_client
            .concurrent_download_files(downloads, concurrent, show_pb)
            .await?;

        Ok(results)
    }

    pub async fn batch_download_artifacts_by_list(
        &mut self,
        args: DownloadArtifactsByListArgs,
    ) -> crate::error::Result<Vec<ResourceDownloadInfo>> {
        let output_dir = args
            .output_dir
            .clone()
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let no_download = args.no_download;
        let show_pb = args.pb;
        let concurrent = args.concurrent;

        let resp = self
            .http_client
            .batch_download_artifacts_by_list(args.into())
            .await?;

        if no_download {
            for item in &resp.downloads {
                tracing::info!("Task {}: {}", item.uuid, item.url);
            }
            return Ok(vec![]);
        }

        // Prepare download list
        let downloads: Vec<(RemoteResourceDownloadResp, std::path::PathBuf)> = resp
            .downloads
            .iter()
            .map(|item| {
                let file_name = format!("{}.tar.gz", item.uuid);
                let output_path = output_dir.join(file_name);
                (
                    RemoteResourceDownloadResp {
                        url: item.url.clone(),
                        size: item.size,
                    },
                    output_path,
                )
            })
            .collect();

        // Collect results for return
        let results: Vec<ResourceDownloadInfo> = resp
            .downloads
            .iter()
            .map(|item| {
                let file_name = format!("{}.tar.gz", item.uuid);
                let output_path = output_dir.join(file_name);
                ResourceDownloadInfo {
                    size: item.size,
                    local_path: output_path,
                }
            })
            .collect();

        // Download concurrently
        self.http_client
            .concurrent_download_files(downloads, concurrent, show_pb)
            .await?;

        Ok(results)
    }

    pub async fn batch_download_attachments_by_filter(
        &mut self,
        args: DownloadAttachmentsByFilterArgs,
    ) -> crate::error::Result<Vec<ResourceDownloadInfo>> {
        let group_name = args.group.clone().unwrap_or_else(|| self.username.clone());
        let output_dir = args
            .output_dir
            .clone()
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let no_download = args.no_download;
        let show_pb = args.pb;
        let concurrent = args.concurrent;

        let resp = self
            .http_client
            .batch_download_attachments_by_filter(&group_name, args.into())
            .await?;

        if no_download {
            for item in &resp.downloads {
                tracing::info!("Attachment {}: {}", item.key, item.url);
            }
            return Ok(vec![]);
        }

        // Prepare download list
        let downloads: Vec<(RemoteResourceDownloadResp, std::path::PathBuf)> = resp
            .downloads
            .iter()
            .map(|item| {
                let fname = std::path::Path::new(&item.key)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(&item.key)
                    .to_string();
                let output_path = output_dir.join(fname);
                (
                    RemoteResourceDownloadResp {
                        url: item.url.clone(),
                        size: item.size,
                    },
                    output_path,
                )
            })
            .collect();

        // Collect results for return
        let results: Vec<ResourceDownloadInfo> = resp
            .downloads
            .iter()
            .map(|item| {
                let fname = std::path::Path::new(&item.key)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(&item.key)
                    .to_string();
                let output_path = output_dir.join(fname);
                ResourceDownloadInfo {
                    size: item.size,
                    local_path: output_path,
                }
            })
            .collect();

        // Download concurrently
        self.http_client
            .concurrent_download_files(downloads, concurrent, show_pb)
            .await?;

        Ok(results)
    }

    pub async fn batch_download_attachments_by_list(
        &mut self,
        args: DownloadAttachmentsByListArgs,
    ) -> crate::error::Result<Vec<ResourceDownloadInfo>> {
        let group_name = args.group.clone().unwrap_or_else(|| self.username.clone());
        let output_dir = args
            .output_dir
            .clone()
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let no_download = args.no_download;
        let show_pb = args.pb;
        let concurrent = args.concurrent;

        let resp = self
            .http_client
            .batch_download_attachments_by_list(&group_name, args.into())
            .await?;

        if no_download {
            for item in &resp.downloads {
                tracing::info!("Attachment {}: {}", item.key, item.url);
            }
            return Ok(vec![]);
        }

        // Prepare download list
        let downloads: Vec<(RemoteResourceDownloadResp, std::path::PathBuf)> = resp
            .downloads
            .iter()
            .map(|item| {
                let fname = std::path::Path::new(&item.key)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(&item.key)
                    .to_string();
                let output_path = output_dir.join(fname);
                (
                    RemoteResourceDownloadResp {
                        url: item.url.clone(),
                        size: item.size,
                    },
                    output_path,
                )
            })
            .collect();

        // Collect results for return
        let results: Vec<ResourceDownloadInfo> = resp
            .downloads
            .iter()
            .map(|item| {
                let fname = std::path::Path::new(&item.key)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(&item.key)
                    .to_string();
                let output_path = output_dir.join(fname);
                ResourceDownloadInfo {
                    size: item.size,
                    local_path: output_path,
                }
            })
            .collect();

        // Download concurrently
        self.http_client
            .concurrent_download_files(downloads, concurrent, show_pb)
            .await?;

        Ok(results)
    }

    pub async fn batch_delete_artifacts_by_filter(
        &mut self,
        args: DeleteArtifactsByFilterArgs,
    ) -> crate::error::Result<ArtifactsDeleteByFilterResp> {
        self.http_client
            .batch_delete_artifacts_by_filter(args.into())
            .await
    }

    pub async fn batch_delete_artifacts_by_list(
        &mut self,
        args: DeleteArtifactsByListArgs,
    ) -> crate::error::Result<ArtifactsDeleteByUuidsResp> {
        self.http_client
            .batch_delete_artifacts_by_list(args.into())
            .await
    }

    pub async fn batch_delete_attachments_by_filter(
        &mut self,
        args: DeleteAttachmentsByFilterArgs,
    ) -> crate::error::Result<AttachmentsDeleteByFilterResp> {
        let group_name = args.group.clone().unwrap_or_else(|| self.username.clone());
        self.http_client
            .batch_delete_attachments_by_filter(&group_name, args.into())
            .await
    }

    pub async fn batch_delete_attachments_by_list(
        &mut self,
        args: DeleteAttachmentsByListArgs,
    ) -> crate::error::Result<AttachmentsDeleteByKeysResp> {
        let group_name = args.group.clone().unwrap_or_else(|| self.username.clone());
        self.http_client
            .batch_delete_attachments_by_list(&group_name, args.into())
            .await
    }

    pub async fn batch_submit_tasks(
        &mut self,
        args: SubmitTasksArgs,
    ) -> crate::error::Result<TasksSubmitResp> {
        let req: TasksSubmitReq = args.try_into()?;
        self.http_client.batch_submit_tasks(req).await
    }

    pub async fn handle_command<T>(&mut self, cmd: T) -> bool
    where
        T: Into<ClientCommand>,
    {
        let cmd = cmd.into();
        match cmd {
            ClientCommand::Admin(admin_args) => match admin_args.command {
                AdminCommands::Users(args) => match args.command {
                    AdminUsersCommands::Create(args) => match self.admin_users_create(args).await {
                        Ok(_) => {
                            tracing::info!("Successfully created user");
                        }
                        Err(e) => {
                            tracing::error!("{}", e);
                        }
                    },
                    AdminUsersCommands::Delete(args) => match self.admin_users_delete(args).await {
                        Ok(_) => {
                            tracing::info!("Successfully deleted user");
                        }
                        Err(e) => {
                            tracing::error!("{}", e);
                        }
                    },
                    AdminUsersCommands::ChangePassword(args) => {
                        match self.admin_change_user_password(args).await {
                            Ok(_) => {
                                tracing::info!("Successfully changed password");
                            }
                            Err(e) => {
                                tracing::error!("{}", e);
                            }
                        }
                    }
                    AdminUsersCommands::GroupQuota(args) => {
                        match self.admin_users_update_group_quota(args).await {
                            Ok(resp) => {
                                tracing::info!(
                                    "Successfully updated user group quota to {}",
                                    resp.group_quota
                                );
                            }
                            Err(e) => {
                                tracing::error!("{}", e);
                            }
                        }
                    }
                },
                AdminCommands::Groups(args) => match args.command {
                    AdminGroupsCommands::StorageQuota(args) => {
                        match self.admin_groups_update_storage_quota(args).await {
                            Ok(resp) => {
                                tracing::info!(
                                    "Successfully updated group storage quota to {}",
                                    format_size(resp.storage_quota.max(0) as u64, DECIMAL)
                                );
                            }
                            Err(e) => {
                                tracing::error!("{}", e);
                            }
                        }
                    }
                    AdminGroupsCommands::Attachments(args) => match args.command {
                        AdminAttachmentsCommands::Delete(args) => {
                            match self.admin_groups_attachments_delete(args).await {
                                Ok(_) => {
                                    tracing::info!("Successfully deleted attachment");
                                }
                                Err(e) => {
                                    tracing::error!("{}", e);
                                }
                            }
                        }
                    },
                },
                AdminCommands::Tasks(args) => match args.command {
                    AdminTasksCommands::Artifacts(args) => match args.command {
                        AdminArtifactsCommands::Delete(args) => {
                            match self.admin_tasks_artifacts_delete(args).await {
                                Ok(_) => {
                                    tracing::info!("Successfully deleted artifact");
                                }
                                Err(e) => {
                                    tracing::error!("{}", e);
                                }
                            }
                        }
                    },
                },
                AdminCommands::Shutdown(args) => {
                    match self.admin_shutdown_coordinator(args).await {
                        Ok(_) => {
                            tracing::info!("Coordinator shutdown successfully");
                            return false;
                        }
                        Err(e) => {
                            tracing::error!("{}", e);
                        }
                    }
                }
                AdminCommands::Workers(args) => match args.command {
                    AdminWorkersCommands::Cancel(args) => {
                        match self.admin_workers_cancel(args).await {
                            Ok(_) => {
                                tracing::info!("Successfully cancelled worker");
                            }
                            Err(e) => {
                                tracing::error!("{}", e);
                            }
                        }
                    }
                },
            },
            ClientCommand::Auth => match self.user_auth().await {
                Ok(username) => {
                    tracing::info!("Currently logged in as {}", username);
                }
                Err(e) => {
                    tracing::error!("{}", e);
                }
            },
            ClientCommand::Login(args) => match self.user_login(args).await {
                Ok(_) => {
                    tracing::info!("Successfully logged in as {}", self.username);
                }
                Err(e) => {
                    tracing::error!("{}", e);
                }
            },
            ClientCommand::Cmd(args) => {
                if !args.command.is_empty() {
                    let shell = std::env::var("SHELL").unwrap_or("/bin/bash".to_string());
                    let mut cmd_handle = std::process::Command::new(shell);
                    cmd_handle.arg("-c");
                    if args.split {
                        cmd_handle.args(args.command);
                    } else {
                        cmd_handle.arg(args.command.join(" "));
                    }
                    let output = cmd_handle
                        .stdin(Stdio::inherit())
                        .stdout(Stdio::inherit())
                        .stderr(Stdio::inherit())
                        .output()
                        .unwrap();
                    std::io::stdout().write_all(&output.stdout).unwrap();
                    std::io::stderr().write_all(&output.stderr).unwrap();
                }
            }

            ClientCommand::Workers(args) => match args.command {
                WorkersCommands::Cancel(args) => match self.workers_cancel(args).await {
                    Ok(_) => {
                        tracing::info!("Worker cancelled successfully");
                    }
                    Err(e) => {
                        tracing::error!("{}", e);
                    }
                },
                WorkersCommands::CancelMany(args) => match self.workers_batch_cancel(args).await {
                    Ok(resp) => {
                        tracing::info!(
                            "Shutdown {} workers in group {}",
                            resp.shutdown_count,
                            resp.group_name
                        );
                    }
                    Err(e) => {
                        tracing::error!("{}", e);
                    }
                },
                WorkersCommands::CancelList(args) => {
                    match self.workers_batch_cancel_by_uuids(args).await {
                        Ok(resp) => {
                            tracing::info!("Shutdown {} workers", resp.shutdown_count);
                            if !resp.failed_uuids.is_empty() {
                                tracing::warn!(
                                    "Failed to shutdown {} workers: {:?}",
                                    resp.failed_uuids.len(),
                                    resp.failed_uuids
                                );
                            }
                        }
                        Err(e) => {
                            tracing::error!("{}", e);
                        }
                    }
                }
                WorkersCommands::UpdateTags(args) => match self.workers_update_tags(args).await {
                    Ok(_) => {
                        tracing::info!("Worker tags updated successfully");
                    }
                    Err(e) => {
                        tracing::error!("{}", e);
                    }
                },
                WorkersCommands::UpdateLabels(args) => match self.workers_update_labels(args).await
                {
                    Ok(_) => {
                        tracing::info!("Worker labels updated successfully");
                    }
                    Err(e) => {
                        tracing::error!("{}", e);
                    }
                },
                WorkersCommands::UpdateRoles(args) => {
                    match self.workers_update_group_roles(args).await {
                        Ok(_) => {
                            tracing::info!("Group worker roles updated successfully");
                        }
                        Err(e) => {
                            tracing::error!("{}", e);
                        }
                    }
                }
                WorkersCommands::RemoveRoles(args) => {
                    match self.workers_remove_group_roles(args).await {
                        Ok(_) => {
                            tracing::info!("Group worker roles removed successfully");
                        }
                        Err(e) => {
                            tracing::error!("{}", e);
                        }
                    }
                }
                WorkersCommands::Get(args) => match self.workers_get(args).await {
                    Ok(resp) => {
                        output_worker_info(&resp.info, &resp.groups);
                    }
                    Err(e) => {
                        tracing::error!("{}", e);
                    }
                },
                WorkersCommands::Query(args) => {
                    let verbose = args.verbose;
                    let counted = args.count;
                    match self.workers_query(args).await {
                        Ok(resp) => {
                            let WorkersQueryResp {
                                count,
                                workers,
                                group_name,
                            } = resp;
                            tracing::info!("Found {} workers in group {}", count, group_name);
                            if !counted {
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
                        }
                        Err(e) => {
                            tracing::error!("{}", e);
                        }
                    }
                }
            },
            ClientCommand::Users(args) => match args.command {
                UsersCommands::ChangePassword(args) => {
                    match self.user_change_password(args).await {
                        Ok(_) => {
                            tracing::info!("User password changed successfully");
                        }
                        Err(e) => {
                            tracing::error!("{}", e);
                        }
                    }
                }
                UsersCommands::Groups => match self.groups_query().await {
                    Ok(resp) => {
                        tracing::info!("Currently in {} groups", resp.groups.len());
                        for (group, role) in resp.groups {
                            tracing::info!("Have {} access for group {}", role, group);
                        }
                    }
                    Err(e) => {
                        tracing::error!("{}", e);
                    }
                },
            },
            ClientCommand::Groups(args) => match args.command {
                GroupsCommands::Create(args) => match self.groups_create(args).await {
                    Ok(_) => {
                        tracing::info!("Successfully created group");
                    }
                    Err(e) => {
                        tracing::error!("{}", e);
                    }
                },
                GroupsCommands::Get(args) => match self.groups_get(args).await {
                    Ok(resp) => {
                        output_group_info(&resp);
                    }
                    Err(e) => {
                        tracing::error!("{}", e);
                    }
                },
                GroupsCommands::UpdateUser(args) => {
                    match self.groups_update_user_roles(args).await {
                        Ok(_) => {
                            tracing::info!("User group roles updated successfully");
                        }
                        Err(e) => {
                            tracing::error!("{}", e);
                        }
                    }
                }
                GroupsCommands::RemoveUser(args) => {
                    match self.groups_remove_user_roles(args).await {
                        Ok(_) => {
                            tracing::info!("User group roles removed successfully");
                        }
                        Err(e) => {
                            tracing::error!("{}", e);
                        }
                    }
                }
                GroupsCommands::Attachments(args) => match args.command {
                    AttachmentsCommands::Delete(args) => {
                        match self.groups_attachments_delete(args).await {
                            Ok(_) => {
                                tracing::info!("Successfully deleted attachment");
                            }
                            Err(e) => {
                                tracing::error!("{}", e);
                            }
                        }
                    }
                    AttachmentsCommands::Upload(args) => match self.upload_attachment(args).await {
                        Ok(exist) => {
                            output_upload_attachment_resp(exist);
                        }
                        Err(e) => {
                            tracing::error!("{}", e);
                        }
                    },
                    AttachmentsCommands::Download(args) => {
                        if args.no_download {
                            match self.get_attachment_download_url(args).await {
                                Ok(url) => {
                                    tracing::info!("Attachment URL: {}", url);
                                }
                                Err(e) => {
                                    tracing::error!("{}", e);
                                }
                            }
                        } else {
                            match self.download_attachment(args).await {
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
                    AttachmentsCommands::Get(args) => {
                        match self.groups_attachments_get_metadata(args).await {
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
                        }
                    }
                    AttachmentsCommands::Query(args) => {
                        let counted = args.count;
                        match self.attachments_query(args).await {
                            Ok(resp) => {
                                let AttachmentsQueryResp {
                                    count,
                                    attachments,
                                    group_name,
                                } = resp;

                                tracing::info!(
                                    "Found {} attachments in group {}",
                                    count,
                                    group_name
                                );
                                if !counted {
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
                            }
                            Err(e) => {
                                tracing::error!("{}", e);
                            }
                        }
                    }
                    AttachmentsCommands::DownloadMany(args) => {
                        match self.batch_download_attachments_by_filter(args).await {
                            Ok(infos) => {
                                tracing::info!("Downloaded {} attachments", infos.len());
                                for info in infos {
                                    tracing::info!(
                                        "  {} ({}B)",
                                        info.local_path.display(),
                                        info.size
                                    );
                                }
                            }
                            Err(e) => {
                                tracing::error!("{}", e);
                            }
                        }
                    }
                    AttachmentsCommands::DownloadList(args) => {
                        match self.batch_download_attachments_by_list(args).await {
                            Ok(infos) => {
                                tracing::info!("Downloaded {} attachments", infos.len());
                                for info in infos {
                                    tracing::info!(
                                        "  {} ({}B)",
                                        info.local_path.display(),
                                        info.size
                                    );
                                }
                            }
                            Err(e) => {
                                tracing::error!("{}", e);
                            }
                        }
                    }
                    AttachmentsCommands::DeleteMany(args) => {
                        match self.batch_delete_attachments_by_filter(args).await {
                            Ok(resp) => {
                                tracing::info!(
                                    "Deleted {} attachments in group {}",
                                    resp.deleted_count,
                                    resp.group_name
                                );
                            }
                            Err(e) => {
                                tracing::error!("{}", e);
                            }
                        }
                    }
                    AttachmentsCommands::DeleteList(args) => {
                        match self.batch_delete_attachments_by_list(args).await {
                            Ok(resp) => {
                                tracing::info!(
                                    "Deleted {} attachments in group {}",
                                    resp.deleted_count,
                                    resp.group_name
                                );
                                if !resp.failed_keys.is_empty() {
                                    tracing::warn!(
                                        "Failed to delete {} attachments: {:?}",
                                        resp.failed_keys.len(),
                                        resp.failed_keys
                                    );
                                }
                            }
                            Err(e) => {
                                tracing::error!("{}", e);
                            }
                        }
                    }
                },
            },
            ClientCommand::Tasks(args) => match args.command {
                TasksCommands::Submit(args) => {
                    let group_name = args.group_name.clone().unwrap_or(self.username.clone());
                    match self.tasks_submit(args).await {
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
                TasksCommands::SubmitMany(args) => match self.batch_submit_tasks(args).await {
                    Ok(resp) => {
                        let mut success_count = 0;
                        let mut error_count = 0;
                        for result in resp.results {
                            match result {
                                Ok(task_resp) => {
                                    success_count += 1;
                                    tracing::info!("Task submitted with uuid {}", task_resp.uuid);
                                }
                                Err(e) => {
                                    error_count += 1;
                                    tracing::error!("Task submission failed: {}", e.msg);
                                }
                            }
                        }
                        tracing::info!(
                            "Batch submission completed: {} succeeded, {} failed",
                            success_count,
                            error_count
                        );
                    }
                    Err(e) => {
                        tracing::error!("{}", e);
                    }
                },
                TasksCommands::Get(args) => match self.tasks_get(args).await {
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
                TasksCommands::Query(args) => {
                    let verbose = args.verbose;
                    let counted = args.count;
                    match self.tasks_query(args).await {
                        Ok(resp) => {
                            tracing::info!("Found {} tasks", resp.count);
                            if !counted {
                                if verbose {
                                    for task in resp.tasks {
                                        output_task_info(&task);
                                    }
                                } else {
                                    for task in resp.tasks {
                                        tracing::info!("{}", task.uuid);
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            tracing::error!("{}", e);
                        }
                    }
                }
                TasksCommands::Cancel(args) => match self.tasks_cancel(args.uuid).await {
                    Ok(_) => {
                        tracing::info!("Task cancelled successfully");
                    }
                    Err(e) => {
                        tracing::error!("{}", e);
                    }
                },
                TasksCommands::CancelMany(args) => match self.tasks_batch_cancel(args).await {
                    Ok(resp) => {
                        tracing::info!(
                            "Cancelled {} tasks in group {}",
                            resp.cancelled_count,
                            resp.group_name
                        );
                    }
                    Err(e) => {
                        tracing::error!("{}", e);
                    }
                },
                TasksCommands::CancelList(args) => {
                    match self.tasks_batch_cancel_by_uuids(args).await {
                        Ok(resp) => {
                            tracing::info!("Cancelled {} tasks", resp.cancelled_count);
                            if !resp.failed_uuids.is_empty() {
                                tracing::warn!(
                                    "Failed to cancel {} tasks: {:?}",
                                    resp.failed_uuids.len(),
                                    resp.failed_uuids
                                );
                            }
                        }
                        Err(e) => {
                            tracing::error!("{}", e);
                        }
                    }
                }
                TasksCommands::UpdateLabels(args) => match self.tasks_update_labels(args).await {
                    Ok(_) => {
                        tracing::info!("Task labels updated successfully");
                    }
                    Err(e) => {
                        tracing::error!("{}", e);
                    }
                },
                TasksCommands::Change(args) => match self.tasks_change(args).await {
                    Ok(_) => {
                        tracing::info!("Task changed successfully");
                    }
                    Err(e) => {
                        tracing::error!("{}", e);
                    }
                },
                TasksCommands::Artifacts(args) => match args.command {
                    ArtifactsCommands::Delete(args) => {
                        match self.tasks_artifacts_delete(args).await {
                            Ok(_) => {
                                tracing::info!("Successfully deleted artifact");
                            }
                            Err(e) => {
                                tracing::error!("{}", e);
                            }
                        }
                    }
                    ArtifactsCommands::Upload(args) => match self.upload_artifact(args).await {
                        Ok(exist) => {
                            output_upload_artifact_resp(exist);
                        }
                        Err(e) => {
                            tracing::error!("{}", e);
                        }
                    },
                    ArtifactsCommands::Download(args) => {
                        if args.no_download {
                            match self.get_artifact_download_url(args).await {
                                Ok(url) => {
                                    tracing::info!("Artifact URL: {}", url);
                                }
                                Err(e) => {
                                    tracing::error!("{}", e);
                                }
                            }
                        } else {
                            match self.download_artifact(args).await {
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
                            }
                        }
                    }
                    ArtifactsCommands::DownloadMany(args) => {
                        match self.batch_download_artifacts_by_filter(args).await {
                            Ok(infos) => {
                                tracing::info!("Downloaded {} artifacts", infos.len());
                                for info in infos {
                                    tracing::info!(
                                        "  {} ({}B)",
                                        info.local_path.display(),
                                        info.size
                                    );
                                }
                            }
                            Err(e) => {
                                tracing::error!("{}", e);
                            }
                        }
                    }
                    ArtifactsCommands::DownloadList(args) => {
                        match self.batch_download_artifacts_by_list(args).await {
                            Ok(infos) => {
                                tracing::info!("Downloaded {} artifacts", infos.len());
                                for info in infos {
                                    tracing::info!(
                                        "  {} ({}B)",
                                        info.local_path.display(),
                                        info.size
                                    );
                                }
                            }
                            Err(e) => {
                                tracing::error!("{}", e);
                            }
                        }
                    }
                    ArtifactsCommands::DeleteMany(args) => {
                        match self.batch_delete_artifacts_by_filter(args).await {
                            Ok(resp) => {
                                tracing::info!("Deleted {} artifacts", resp.deleted_count);
                            }
                            Err(e) => {
                                tracing::error!("{}", e);
                            }
                        }
                    }
                    ArtifactsCommands::DeleteList(args) => {
                        match self.batch_delete_artifacts_by_list(args).await {
                            Ok(resp) => {
                                tracing::info!("Deleted {} artifacts", resp.deleted_count);
                                if !resp.failed_uuids.is_empty() {
                                    tracing::warn!(
                                        "Failed to delete {} artifacts: {:?}",
                                        resp.failed_uuids.len(),
                                        resp.failed_uuids
                                    );
                                }
                            }
                            Err(e) => {
                                tracing::error!("{}", e);
                            }
                        }
                    }
                },
            },
            ClientCommand::Quit => {
                return false;
            }
        }
        true
    }

    fn gen_submit_task_req(&self, args: SubmitTaskArgs) -> SubmitTaskReq {
        let task_spec = TaskSpec::new(
            args.command,
            args.envs,
            args.resources,
            args.terminal_output,
            args.watch,
        );
        let mut req = SubmitTaskReq {
            group_name: args.group_name.unwrap_or(self.username.clone()),
            tags: args.tags.into_iter().collect(),
            labels: args.labels.into_iter().collect(),
            timeout: args.timeout,
            priority: args.priority,
            task_spec,
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

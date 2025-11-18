use figment::value::magic::RelativePathBuf;
use reqwest::Client;
use url::Url;
use uuid::Uuid;

use crate::{
    entity::content::ArtifactContentType,
    error::{ApiError, AuthError, Error},
    schema::*,
};

use super::http::MitoHttpClient;

/// A wrapper around MitoHttpClient that automatically re-authenticates on 401 errors
pub struct PersistentMitoHttpClient {
    inner: MitoHttpClient,
    username: Option<String>,
    password: Option<String>,
    credential_path: Option<RelativePathBuf>,
    retain: bool,
}

impl PersistentMitoHttpClient {
    pub fn new(coordinator_addr: Url) -> Self {
        Self {
            inner: MitoHttpClient::new(coordinator_addr),
            username: None,
            password: None,
            credential_path: None,
            retain: false,
        }
    }

    pub async fn connect(
        &mut self,
        credential_path: Option<RelativePathBuf>,
        user: Option<String>,
        password: Option<String>,
        retain: bool,
    ) -> crate::error::Result<String> {
        // Store credentials for future re-authentication
        self.username = user.clone();
        self.password = password.clone();
        self.credential_path = credential_path.clone();
        self.retain = retain;

        // Perform initial connection
        self.inner
            .connect(credential_path, user, password, retain)
            .await
    }

    /// Re-authenticate using stored credentials
    async fn reauthenticate(&mut self) -> crate::error::Result<()> {
        self.inner
            .connect(
                self.credential_path.clone(),
                self.username.clone(),
                self.password.clone(),
                self.retain,
            )
            .await?;
        Ok(())
    }

    /// Check if an error is a 401 authentication error
    fn is_auth_error(err: &Error) -> bool {
        match err {
            Error::ApiError(ApiError::AuthError(AuthError::WrongCredentials)) => true,
            Error::RequestError(crate::error::RequestError::ClientError(
                crate::error::ClientError::Inner(status, _),
            )) if *status == reqwest::StatusCode::UNAUTHORIZED => true,
            _ => false,
        }
    }

    pub fn client(&self) -> &Client {
        self.inner.client()
    }

    pub fn client_mut(&mut self) -> &mut Client {
        self.inner.client_mut()
    }

    pub fn url(&self) -> &Url {
        self.inner.url()
    }

    pub fn url_mut(&mut self) -> &mut Url {
        self.inner.url_mut()
    }
}

// Macro to wrap methods with retry logic
macro_rules! impl_with_retry {
    // For methods that take &mut self and return Result
    ($(pub async fn $name:ident(&mut self $(, $arg:ident: $ty:ty)*) -> crate::error::Result<$ret:ty> { $field:ident })*) => {
        $(
            pub async fn $name(&mut self $(, $arg: $ty)*) -> crate::error::Result<$ret> {
                match self.inner.$field($($arg),*).await {
                    Ok(result) => Ok(result),
                    Err(err) if Self::is_auth_error(&err) => {
                        // Try to re-authenticate once
                        self.reauthenticate().await?;
                        // Retry the operation
                        self.inner.$field($($arg),*).await
                    }
                    Err(err) => Err(err),
                }
            }
        )*
    };
}

impl PersistentMitoHttpClient {
    impl_with_retry! {
        pub async fn get_redis_connection_info(&mut self) -> crate::error::Result<RedisConnectionInfo> { get_redis_connection_info }
        pub async fn user_auth(&mut self) -> crate::error::Result<String> { user_auth }
    }

    pub async fn user_login(&mut self, req: UserLoginReq) -> crate::error::Result<()> {
        match self.inner.user_login(req.clone()).await {
            Ok(result) => {
                // Update stored username if login succeeds
                self.username = Some(req.username);
                Ok(result)
            }
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.user_login(req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn admin_change_password(
        &mut self,
        username: String,
        req: AdminChangePasswordReq,
    ) -> crate::error::Result<()> {
        match self.inner.admin_change_password(username.clone(), req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.admin_change_password(username, req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn user_change_password(
        &mut self,
        username: String,
        req: UserChangePasswordReq,
    ) -> crate::error::Result<()> {
        match self.inner.user_change_password(username.clone(), req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.user_change_password(username, req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn admin_create_user(&mut self, req: CreateUserReq) -> crate::error::Result<()> {
        match self.inner.admin_create_user(req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.admin_create_user(req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn admin_delete_user(&mut self, username: String) -> crate::error::Result<()> {
        match self.inner.admin_delete_user(username.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.admin_delete_user(username).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn admin_cancel_worker_by_uuid(
        &mut self,
        uuid: Uuid,
        force: bool,
    ) -> crate::error::Result<()> {
        match self.inner.admin_cancel_worker_by_uuid(uuid, force).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.admin_cancel_worker_by_uuid(uuid, force).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn user_create_group(&mut self, req: CreateGroupReq) -> crate::error::Result<()> {
        match self.inner.user_create_group(req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.user_create_group(req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn get_task_by_uuid(&mut self, uuid: Uuid) -> crate::error::Result<TaskQueryResp> {
        match self.inner.get_task_by_uuid(uuid).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.get_task_by_uuid(uuid).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn get_artifact_download_resp(
        &mut self,
        uuid: Uuid,
        content_type: ArtifactContentType,
    ) -> crate::error::Result<RemoteResourceDownloadResp> {
        match self.inner.get_artifact_download_resp(uuid, content_type).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.get_artifact_download_resp(uuid, content_type).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn get_attachment_download_resp(
        &mut self,
        group_name: &str,
        key: &str,
    ) -> crate::error::Result<RemoteResourceDownloadResp> {
        match self.inner.get_attachment_download_resp(group_name, key).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.get_attachment_download_resp(group_name, key).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn download_file(
        &mut self,
        resp: &RemoteResourceDownloadResp,
        local_path: impl AsRef<std::path::Path>,
        show_pb: bool,
    ) -> crate::error::Result<()> {
        self.inner.download_file(resp, local_path, show_pb).await
    }

    pub async fn concurrent_download_files(
        &self,
        downloads: Vec<(RemoteResourceDownloadResp, std::path::PathBuf)>,
        concurrent: usize,
        show_pb: bool,
    ) -> crate::error::Result<()> {
        self.inner.concurrent_download_files(downloads, concurrent, show_pb).await
    }

    pub async fn batch_download_artifacts_by_filter(
        &mut self,
        req: ArtifactsDownloadByFilterReq,
    ) -> crate::error::Result<ArtifactsDownloadListResp> {
        match self.inner.batch_download_artifacts_by_filter(req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.batch_download_artifacts_by_filter(req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn batch_download_artifacts_by_list(
        &mut self,
        req: ArtifactsDownloadByUuidsReq,
    ) -> crate::error::Result<ArtifactsDownloadListResp> {
        match self.inner.batch_download_artifacts_by_list(req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.batch_download_artifacts_by_list(req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn batch_download_attachments_by_filter(
        &mut self,
        group_name: &str,
        req: AttachmentsDownloadByFilterReq,
    ) -> crate::error::Result<AttachmentsDownloadListResp> {
        match self.inner.batch_download_attachments_by_filter(group_name, req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.batch_download_attachments_by_filter(group_name, req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn batch_download_attachments_by_list(
        &mut self,
        group_name: &str,
        req: AttachmentsDownloadByKeysReq,
    ) -> crate::error::Result<AttachmentsDownloadListResp> {
        match self.inner.batch_download_attachments_by_list(group_name, req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.batch_download_attachments_by_list(group_name, req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn delete_artifact(
        &mut self,
        uuid: Uuid,
        content_type: ArtifactContentType,
        admin: bool,
    ) -> crate::error::Result<()> {
        match self.inner.delete_artifact(uuid, content_type, admin).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.delete_artifact(uuid, content_type, admin).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn delete_attachment(
        &mut self,
        group_name: &str,
        key: &str,
        admin: bool,
    ) -> crate::error::Result<()> {
        match self.inner.delete_attachment(group_name, key, admin).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.delete_attachment(group_name, key, admin).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn get_attachment(
        &mut self,
        group_name: &str,
        key: &str,
    ) -> crate::error::Result<AttachmentMetadata> {
        match self.inner.get_attachment(group_name, key).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.get_attachment(group_name, key).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn admin_update_user_group_quota(
        &mut self,
        username: &str,
        req: ChangeUserGroupQuota,
    ) -> crate::error::Result<UserGroupQuotaResp> {
        match self.inner.admin_update_user_group_quota(username, req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.admin_update_user_group_quota(username, req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn admin_update_group_storage_quota(
        &mut self,
        group_name: &str,
        req: ChangeGroupStorageQuotaReq,
    ) -> crate::error::Result<GroupStorageQuotaResp> {
        match self.inner.admin_update_group_storage_quota(group_name, req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.admin_update_group_storage_quota(group_name, req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn query_tasks_by_filter(
        &mut self,
        req: TasksQueryReq,
    ) -> crate::error::Result<TasksQueryResp> {
        match self.inner.query_tasks_by_filter(req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.query_tasks_by_filter(req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn query_attachments_by_filter(
        &mut self,
        group_name: &str,
        req: AttachmentsQueryReq,
    ) -> crate::error::Result<AttachmentsQueryResp> {
        match self.inner.query_attachments_by_filter(group_name, req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.query_attachments_by_filter(group_name, req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn get_worker_by_uuid(
        &mut self,
        uuid: Uuid,
    ) -> crate::error::Result<WorkerQueryResp> {
        match self.inner.get_worker_by_uuid(uuid).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.get_worker_by_uuid(uuid).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn query_workers_by_filter(
        &mut self,
        req: WorkersQueryReq,
    ) -> crate::error::Result<WorkersQueryResp> {
        match self.inner.query_workers_by_filter(req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.query_workers_by_filter(req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn get_group_by_name(
        &mut self,
        group_name: &str,
    ) -> crate::error::Result<GroupQueryInfo> {
        match self.inner.get_group_by_name(group_name).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.get_group_by_name(group_name).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn get_user_groups_roles(&mut self) -> crate::error::Result<GroupsQueryResp> {
        match self.inner.get_user_groups_roles().await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.get_user_groups_roles().await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn user_submit_task(
        &mut self,
        req: SubmitTaskReq,
    ) -> crate::error::Result<SubmitTaskResp> {
        match self.inner.user_submit_task(req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.user_submit_task(req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn upload_file(
        &mut self,
        url: &str,
        content_length: u64,
        local_path: impl AsRef<std::path::Path>,
        show_pb: bool,
    ) -> crate::error::Result<()> {
        self.inner.upload_file(url, content_length, local_path, show_pb).await
    }

    pub async fn get_upload_artifact_resp(
        &mut self,
        uuid: Uuid,
        req: UploadArtifactReq,
    ) -> crate::error::Result<UploadArtifactResp> {
        match self.inner.get_upload_artifact_resp(uuid, req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.get_upload_artifact_resp(uuid, req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn get_upload_attachment_resp(
        &mut self,
        group_name: &str,
        req: UploadAttachmentReq,
    ) -> crate::error::Result<UploadAttachmentResp> {
        match self.inner.get_upload_attachment_resp(group_name, req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.get_upload_attachment_resp(group_name, req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn cancel_worker_by_uuid(
        &mut self,
        uuid: Uuid,
        force: bool,
    ) -> crate::error::Result<()> {
        match self.inner.cancel_worker_by_uuid(uuid, force).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.cancel_worker_by_uuid(uuid, force).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn shutdown_workers_by_filter(
        &mut self,
        req: WorkersShutdownByFilterReq,
    ) -> crate::error::Result<WorkersShutdownByFilterResp> {
        match self.inner.shutdown_workers_by_filter(req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.shutdown_workers_by_filter(req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn shutdown_workers_by_uuids(
        &mut self,
        req: WorkersShutdownByUuidsReq,
    ) -> crate::error::Result<WorkersShutdownByUuidsResp> {
        match self.inner.shutdown_workers_by_uuids(req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.shutdown_workers_by_uuids(req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn replace_worker_tags(
        &mut self,
        uuid: Uuid,
        req: ReplaceWorkerTagsReq,
    ) -> crate::error::Result<()> {
        match self.inner.replace_worker_tags(uuid, req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.replace_worker_tags(uuid, req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn replace_worker_labels(
        &mut self,
        uuid: Uuid,
        req: ReplaceWorkerLabelsReq,
    ) -> crate::error::Result<()> {
        match self.inner.replace_worker_labels(uuid, req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.replace_worker_labels(uuid, req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn update_group_worker_roles(
        &mut self,
        uuid: Uuid,
        req: UpdateGroupWorkerRoleReq,
    ) -> crate::error::Result<()> {
        match self.inner.update_group_worker_roles(uuid, req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.update_group_worker_roles(uuid, req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn remove_group_worker_roles(
        &mut self,
        uuid: Uuid,
        req: RemoveGroupWorkerRoleReq,
    ) -> crate::error::Result<()> {
        match self.inner.remove_group_worker_roles(uuid, req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.remove_group_worker_roles(uuid, req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn cancel_task_by_uuid(&mut self, uuid: Uuid) -> crate::error::Result<()> {
        match self.inner.cancel_task_by_uuid(uuid).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.cancel_task_by_uuid(uuid).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn cancel_tasks_by_filter(
        &mut self,
        req: TasksCancelByFilterReq,
    ) -> crate::error::Result<TasksCancelByFilterResp> {
        match self.inner.cancel_tasks_by_filter(req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.cancel_tasks_by_filter(req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn cancel_tasks_by_uuids(
        &mut self,
        req: TasksCancelByUuidsReq,
    ) -> crate::error::Result<TasksCancelByUuidsResp> {
        match self.inner.cancel_tasks_by_uuids(req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.cancel_tasks_by_uuids(req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn update_task_labels(
        &mut self,
        uuid: Uuid,
        req: UpdateTaskLabelsReq,
    ) -> crate::error::Result<()> {
        match self.inner.update_task_labels(uuid, req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.update_task_labels(uuid, req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn change_task(
        &mut self,
        uuid: Uuid,
        req: ChangeTaskReq,
    ) -> crate::error::Result<()> {
        match self.inner.change_task(uuid, req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.change_task(uuid, req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn update_user_group_roles(
        &mut self,
        group_name: &str,
        req: UpdateUserGroupRoleReq,
    ) -> crate::error::Result<()> {
        match self.inner.update_user_group_roles(group_name, req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.update_user_group_roles(group_name, req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn remove_user_group_roles(
        &mut self,
        group_name: &str,
        req: RemoveUserGroupRoleReq,
    ) -> crate::error::Result<()> {
        match self.inner.remove_user_group_roles(group_name, req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.remove_user_group_roles(group_name, req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn batch_delete_artifacts_by_filter(
        &mut self,
        req: ArtifactsDeleteByFilterReq,
    ) -> crate::error::Result<ArtifactsDeleteByFilterResp> {
        match self.inner.batch_delete_artifacts_by_filter(req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.batch_delete_artifacts_by_filter(req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn batch_delete_artifacts_by_list(
        &mut self,
        req: ArtifactsDeleteByUuidsReq,
    ) -> crate::error::Result<ArtifactsDeleteByUuidsResp> {
        match self.inner.batch_delete_artifacts_by_list(req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.batch_delete_artifacts_by_list(req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn batch_delete_attachments_by_filter(
        &mut self,
        group_name: &str,
        req: AttachmentsDeleteByFilterReq,
    ) -> crate::error::Result<AttachmentsDeleteByFilterResp> {
        match self.inner.batch_delete_attachments_by_filter(group_name, req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.batch_delete_attachments_by_filter(group_name, req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn batch_delete_attachments_by_list(
        &mut self,
        group_name: &str,
        req: AttachmentsDeleteByKeysReq,
    ) -> crate::error::Result<AttachmentsDeleteByKeysResp> {
        match self.inner.batch_delete_attachments_by_list(group_name, req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.batch_delete_attachments_by_list(group_name, req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn batch_submit_tasks(
        &mut self,
        req: TasksSubmitReq,
    ) -> crate::error::Result<TasksSubmitResp> {
        match self.inner.batch_submit_tasks(req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.batch_submit_tasks(req).await
            }
            Err(err) => Err(err),
        }
    }

    pub async fn admin_shutdown_coordinator(
        &mut self,
        req: ShutdownReq,
    ) -> crate::error::Result<()> {
        match self.inner.admin_shutdown_coordinator(req.clone()).await {
            Ok(result) => Ok(result),
            Err(err) if Self::is_auth_error(&err) => {
                self.reauthenticate().await?;
                self.inner.admin_shutdown_coordinator(req).await
            }
            Err(err) => Err(err),
        }
    }
}

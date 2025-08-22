use std::path::PathBuf;

use figment::value::magic::RelativePathBuf;
use reqwest::Client;
use url::Url;
use uuid::Uuid;

use crate::{
    entity::content::ArtifactContentType,
    error::{get_error_from_resp, map_reqwest_err, RequestError},
    schema::*,
    service::auth::cred::{get_user_credential, modify_or_append_credential},
};

pub struct MitoHttpClient {
    http_client: Client,
    url: Url,
    credential: String,
    credential_path: PathBuf,
}

impl MitoHttpClient {
    pub fn new(mut coordinator_addr: Url) -> Self {
        let http_client = Client::new();
        coordinator_addr.set_path("/");
        Self {
            http_client,
            url: coordinator_addr,
            credential: String::new(),
            credential_path: PathBuf::new(),
        }
    }

    pub async fn connect(
        &mut self,
        credential_path: Option<RelativePathBuf>,
        user: Option<String>,
        password: Option<String>,
        retain: bool,
    ) -> crate::error::Result<String> {
        let client_credential_path = credential_path
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
            credential_path.as_ref(),
            &self.http_client,
            self.url.clone(),
            user,
            password,
            retain,
        )
        .await?;
        self.credential_path = client_credential_path;
        self.credential = credential;
        Ok(username)
    }

    pub fn client(&self) -> &Client {
        &self.http_client
    }

    pub fn client_mut(&mut self) -> &mut Client {
        &mut self.http_client
    }

    pub fn url(&self) -> &Url {
        &self.url
    }

    pub fn url_mut(&mut self) -> &mut Url {
        &mut self.url
    }

    pub async fn get_redis_connection_info(&mut self) -> crate::error::Result<RedisConnectionInfo> {
        self.url.set_path("redis");
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

    pub async fn user_auth(&mut self) -> crate::error::Result<String> {
        self.url.set_path("auth");
        let resp = self
            .http_client
            .get(self.url.as_str())
            .bearer_auth(&self.credential)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            let resp_name = resp.text().await.map_err(RequestError::from)?;
            Ok(resp_name)
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn user_login(&mut self, req: UserLoginReq) -> crate::error::Result<()> {
        self.url.set_path("login");
        let resp = self
            .http_client
            .post(self.url.as_str())
            .json(&req)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            let resp = resp
                .json::<UserLoginResp>()
                .await
                .map_err(RequestError::from)?;
            self.credential = resp.token;
            if self.credential_path.exists() {
                if let Some(parent) = self.credential_path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                modify_or_append_credential(&self.credential_path, &req.username, &self.credential)
                    .await?;
            }
            Ok(())
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn admin_change_password(
        &mut self,
        username: String,
        req: AdminChangePasswordReq,
    ) -> crate::error::Result<()> {
        self.url
            .set_path(&format!("admin/users/{username}/password"));
        let resp = self
            .http_client
            .post(self.url.as_str())
            .bearer_auth(&self.credential)
            .json(&req)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn user_change_password(
        &mut self,
        username: String,
        req: UserChangePasswordReq,
    ) -> crate::error::Result<()> {
        self.url.set_path(&format!("users/{username}/password"));
        let resp = self
            .http_client
            .post(self.url.as_str())
            .bearer_auth(&self.credential)
            .json(&req)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            let resp = resp
                .json::<UserChangePasswordResp>()
                .await
                .map_err(RequestError::from)?;
            self.credential = resp.token;
            if self.credential_path.exists() {
                if let Some(parent) = self.credential_path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                modify_or_append_credential(&self.credential_path, &username, &self.credential)
                    .await?;
            }
            Ok(())
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn admin_create_user(&mut self, req: CreateUserReq) -> crate::error::Result<()> {
        self.url.set_path("admin/users");
        let resp = self
            .http_client
            .post(self.url.as_str())
            .bearer_auth(&self.credential)
            .json(&req)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn admin_delete_user(&mut self, req: DeleteUserReq) -> crate::error::Result<()> {
        self.url.set_path("admin/users");
        let resp = self
            .http_client
            .delete(self.url.as_str())
            .bearer_auth(&self.credential)
            .json(&req)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn admin_cancel_worker_by_uuid(
        &mut self,
        uuid: Uuid,
        force: bool,
    ) -> crate::error::Result<()> {
        self.url.set_path(&format!("admin/workers/{uuid}"));
        if force {
            self.url.set_query(Some("op=force"))
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

    pub async fn user_create_group(&mut self, req: CreateGroupReq) -> crate::error::Result<()> {
        self.url.set_path("groups");
        let resp = self
            .http_client
            .post(self.url.as_str())
            .bearer_auth(&self.credential)
            .json(&req)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn get_task_by_uuid(&mut self, uuid: Uuid) -> crate::error::Result<TaskQueryResp> {
        self.url.set_path(&format!("tasks/{uuid}"));
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

    pub async fn get_artifact_download_resp(
        &mut self,
        uuid: Uuid,
        content_type: ArtifactContentType,
    ) -> crate::error::Result<RemoteResourceDownloadResp> {
        let content_serde_val = serde_json::to_value(content_type)?;
        let content_serde_str = content_serde_val.as_str().unwrap_or("result");
        self.url.set_path(&format!(
            "tasks/{uuid}/download/artifacts/{content_serde_str}"
        ));
        let resp = self
            .http_client
            .get(self.url.as_str())
            .bearer_auth(&self.credential)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            let r = resp
                .json::<RemoteResourceDownloadResp>()
                .await
                .map_err(RequestError::from)?;
            Ok(r)
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn get_attachment_download_resp(
        &mut self,
        group_name: &str,
        key: &str,
    ) -> crate::error::Result<RemoteResourceDownloadResp> {
        self.url
            .set_path(&format!("groups/{group_name}/download/attachments/{key}"));
        let resp = self
            .http_client
            .get(self.url.as_str())
            .bearer_auth(&self.credential)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            let r = resp
                .json::<RemoteResourceDownloadResp>()
                .await
                .map_err(RequestError::from)?;
            Ok(r)
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn download_file(
        &mut self,
        resp: &RemoteResourceDownloadResp,
        local_path: impl AsRef<std::path::Path>,
        show_pb: bool,
    ) -> crate::error::Result<()> {
        crate::service::s3::download_file(&self.http_client, resp, local_path, show_pb).await
    }

    pub async fn delete_artifact(
        &mut self,
        uuid: Uuid,
        content_type: ArtifactContentType,
        admin: bool,
    ) -> crate::error::Result<()> {
        let content_serde_val = serde_json::to_value(content_type)?;
        let content_serde_str = content_serde_val.as_str().unwrap_or("result");
        if admin {
            self.url
                .set_path(&format!("admin/tasks/{uuid}/artifacts/{content_serde_str}"));
        } else {
            self.url
                .set_path(&format!("tasks/{uuid}/artifacts/{content_serde_str}"));
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

    pub async fn delete_attachment(
        &mut self,
        group_name: &str,
        key: &str,
        admin: bool,
    ) -> crate::error::Result<()> {
        if admin {
            self.url
                .set_path(&format!("admin/groups/{group_name}/attachments/{key}"));
        } else {
            self.url
                .set_path(&format!("groups/{group_name}/attachments/{key}"));
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
    pub async fn get_attachment(
        &mut self,
        group_name: &str,
        key: &str,
    ) -> crate::error::Result<AttachmentMetadata> {
        self.url
            .set_path(&format!("groups/{group_name}/attachments/{key}"));
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

    pub async fn admin_update_group_storage_quota(
        &mut self,
        group_name: &str,
        req: ChangeGroupStorageQuotaReq,
    ) -> crate::error::Result<GroupStorageQuotaResp> {
        self.url
            .set_path(&format!("admin/groups/{group_name}/storage-quota"));
        let resp = self
            .http_client
            .post(self.url.as_str())
            .bearer_auth(&self.credential)
            .json(&req)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            let update_resp = resp
                .json::<GroupStorageQuotaResp>()
                .await
                .map_err(RequestError::from)?;
            Ok(update_resp)
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn query_tasks_by_filter(
        &mut self,
        req: TasksQueryReq,
    ) -> crate::error::Result<TasksQueryResp> {
        self.url.set_path("tasks/query");
        let resp = self
            .http_client
            .post(self.url.as_str())
            .bearer_auth(&self.credential)
            .json(&req)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            let resp = resp
                .json::<TasksQueryResp>()
                .await
                .map_err(RequestError::from)?;
            Ok(resp)
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn query_attachments_by_filter(
        &mut self,
        group_name: &str,
        req: AttachmentsQueryReq,
    ) -> crate::error::Result<AttachmentsQueryResp> {
        self.url
            .set_path(&format!("groups/{group_name}/attachments/query"));
        let resp = self
            .http_client
            .post(self.url.as_str())
            .bearer_auth(&self.credential)
            .json(&req)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            let resp = resp
                .json::<AttachmentsQueryResp>()
                .await
                .map_err(RequestError::from)?;
            Ok(resp)
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn get_worker_by_uuid(
        &mut self,
        uuid: Uuid,
    ) -> crate::error::Result<WorkerQueryResp> {
        self.url.set_path(&format!("workers/{uuid}"));
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

    pub async fn query_workers_by_filter(
        &mut self,
        req: WorkersQueryReq,
    ) -> crate::error::Result<WorkersQueryResp> {
        self.url.set_path("workers/query");
        let resp = self
            .http_client
            .post(self.url.as_str())
            .bearer_auth(&self.credential)
            .json(&req)
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

    pub async fn get_group_by_name(
        &mut self,
        group_name: &str,
    ) -> crate::error::Result<GroupQueryInfo> {
        self.url.set_path(&format!("groups/{group_name}"));
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

    pub async fn get_user_groups_roles(&mut self) -> crate::error::Result<GroupsQueryResp> {
        self.url.set_path("users/groups");
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

    pub async fn user_submit_task(
        &mut self,
        req: SubmitTaskReq,
    ) -> crate::error::Result<SubmitTaskResp> {
        self.url.set_path("tasks");
        let resp = self
            .http_client
            .post(self.url.as_str())
            .bearer_auth(&self.credential)
            .json(&req)
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

    pub async fn upload_file(
        &mut self,
        url: &str,
        content_length: u64,
        local_path: impl AsRef<std::path::Path>,
        show_pb: bool,
    ) -> crate::error::Result<()> {
        crate::service::s3::upload_file(&self.http_client, url, content_length, local_path, show_pb)
            .await
    }

    pub async fn get_upload_artifact_resp(
        &mut self,
        uuid: Uuid,
        req: UploadArtifactReq,
    ) -> crate::error::Result<UploadArtifactResp> {
        self.url.set_path(&format!("tasks/{uuid}/artifacts"));
        let resp = self
            .http_client
            .post(self.url.as_str())
            .bearer_auth(&self.credential)
            .json(&req)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            let resp = resp
                .json::<UploadArtifactResp>()
                .await
                .map_err(RequestError::from)?;
            Ok(resp)
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn get_upload_attachment_resp(
        &mut self,
        group_name: &str,
        req: UploadAttachmentReq,
    ) -> crate::error::Result<UploadAttachmentResp> {
        self.url
            .set_path(&format!("groups/{group_name}/attachments"));
        let resp = self
            .http_client
            .post(self.url.as_str())
            .bearer_auth(&self.credential)
            .json(&req)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            let resp = resp
                .json::<UploadAttachmentResp>()
                .await
                .map_err(RequestError::from)?;
            Ok(resp)
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn cancel_worker_by_uuid(
        &mut self,
        uuid: Uuid,
        force: bool,
    ) -> crate::error::Result<()> {
        self.url.set_path(&format!("workers/{uuid}"));
        if force {
            self.url.set_query(Some("op=force"))
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
        req: ReplaceWorkerTagsReq,
    ) -> crate::error::Result<()> {
        self.url.set_path(&format!("workers/{uuid}/tags"));
        let resp = self
            .http_client
            .put(self.url.as_str())
            .bearer_auth(&self.credential)
            .json(&req)
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
        req: UpdateGroupWorkerRoleReq,
    ) -> crate::error::Result<()> {
        self.url.set_path(&format!("workers/{uuid}/groups"));
        let resp = self
            .http_client
            .put(self.url.as_str())
            .bearer_auth(&self.credential)
            .json(&req)
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
        req: RemoveGroupWorkerRoleReq,
    ) -> crate::error::Result<()> {
        self.url.set_path(&format!("workers/{uuid}/groups"));
        let resp = self
            .http_client
            .delete(self.url.as_str())
            .bearer_auth(&self.credential)
            .json(&req)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn cancel_task_by_uuid(&mut self, uuid: Uuid) -> crate::error::Result<()> {
        self.url.set_path(&format!("tasks/{uuid}"));
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
        req: UpdateTaskLabelsReq,
    ) -> crate::error::Result<()> {
        self.url.set_path(&format!("tasks/{uuid}/labels"));
        let resp = self
            .http_client
            .put(self.url.as_str())
            .bearer_auth(&self.credential)
            .json(&req)
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
        req: ChangeTaskReq,
    ) -> crate::error::Result<()> {
        self.url.set_path(&format!("tasks/{uuid}"));
        let resp = self
            .http_client
            .put(self.url.as_str())
            .bearer_auth(&self.credential)
            .json(&req)
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
        group_name: &str,
        req: UpdateUserGroupRoleReq,
    ) -> crate::error::Result<()> {
        self.url.set_path(&format!("groups/{group_name}/users"));
        let resp = self
            .http_client
            .put(self.url.as_str())
            .bearer_auth(&self.credential)
            .json(&req)
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
        group_name: &str,
        req: RemoveUserGroupRoleReq,
    ) -> crate::error::Result<()> {
        self.url.set_path(&format!("groups/{group_name}/users"));
        let resp = self
            .http_client
            .delete(self.url.as_str())
            .bearer_auth(&self.credential)
            .json(&req)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }

    pub async fn admin_shutdown_coordinator(
        &mut self,
        req: ShutdownReq,
    ) -> crate::error::Result<()> {
        self.url.set_path("admin/shutdown");
        let resp = self
            .http_client
            .post(self.url.as_str())
            .bearer_auth(&self.credential)
            .json(&req)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(get_error_from_resp(resp).await.into())
        }
    }
}

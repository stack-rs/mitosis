use std::path::Path;
use std::time::Duration;

use aws_sdk_s3::{error::SdkError, presigning::PresigningConfig, Client};
use futures::StreamExt;
use indicatif::{ProgressBar, ProgressState, ProgressStyle};
use reqwest::{header::CONTENT_LENGTH, Response};
use sea_orm::{
    prelude::*,
    sea_query::{Expr, Query},
    FromQueryResult, Set, TransactionTrait,
};
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use crate::schema::{
    AttachmentMetadata, AttachmentsQueryResp, CountQuery, RemoteResourceDownloadResp,
};
use crate::{config::InfraPool, error::S3Error};
use crate::{
    entity::StoredTaskModel,
    error::{map_reqwest_err, ApiError, RequestError},
};
use crate::{
    entity::{
        active_tasks as ActiveTask, archived_tasks as ArchivedTask, artifacts as Artifact,
        attachments as Attachment, content::ArtifactContentType, groups as Group,
        role::UserGroupRole, user_group as UserGroup, users as User,
    },
    error::AuthError,
};
use crate::{
    entity::{content::AttachmentContentType, state::GroupState},
    error::Error,
    schema::{AttachmentQueryInfo, AttachmentsQueryReq},
};

pub async fn create_bucket(client: &Client, bucket_name: &str) -> Result<(), S3Error> {
    match client.create_bucket().bucket(bucket_name).send().await {
        Ok(_) => {
            tracing::info!("Bucket {} created", bucket_name);
            Ok(())
        }
        Err(SdkError::ServiceError(e))
            if e.err().is_bucket_already_exists() || e.err().is_bucket_already_owned_by_you() =>
        {
            tracing::info!("Bucket {} already exists", bucket_name);
            Ok(())
        }
        Err(e) => Err(S3Error::CreateBucketError(e)),
    }
}

pub async fn get_presigned_upload_link<T: Into<String>>(
    client: &Client,
    bucket: &str,
    key: T,
    length: i64,
) -> Result<String, S3Error> {
    if length <= 0 {
        return Err(S3Error::InvalidContentLength(length));
    }
    // Restrict the link to be valid for at most 60 minutes
    let expires = Duration::from_secs(3600);
    let resp = client
        .put_object()
        .bucket(bucket)
        .key(key)
        .content_length(length)
        .presigned(PresigningConfig::expires_in(expires)?)
        .await?;
    Ok(resp.uri().to_string())
}

pub async fn get_presigned_download_link<T: Into<String>>(
    client: &Client,
    bucket: &str,
    key: T,
    length: i64,
) -> Result<String, S3Error> {
    // At least valid for 1 hour and at most valid for 1 day
    let expires = Duration::from_secs(86400.min(3600.max(length as u64 / 1000000)));
    let resp = client
        .get_object()
        .bucket(bucket)
        .key(key)
        .presigned(PresigningConfig::expires_in(expires)?)
        .await?;
    Ok(resp.uri().to_string())
}

pub async fn get_artifact(
    pool: &InfraPool,
    uuid: Uuid,
    content_type: ArtifactContentType,
) -> Result<RemoteResourceDownloadResp, crate::error::Error> {
    let artifact = Artifact::Entity::find()
        .filter(Artifact::Column::TaskId.eq(uuid))
        .filter(Artifact::Column::ContentType.eq(content_type))
        .one(&pool.db)
        .await?
        .ok_or(crate::error::ApiError::NotFound(format!(
            "Artifact with uuid {uuid} and content type {content_type}"
        )))?;
    let key = format!("{uuid}/{content_type}");
    let url =
        get_presigned_download_link(&pool.s3, "mitosis-artifacts", key, artifact.size).await?;
    Ok(RemoteResourceDownloadResp {
        url,
        size: artifact.size,
    })
}

pub(crate) async fn group_upload_artifact(
    pool: &InfraPool,
    task: StoredTaskModel,
    content_type: ArtifactContentType,
    content_length: u64,
) -> Result<String, crate::error::Error> {
    let now = TimeDateTimeWithTimeZone::now_utc();
    let content_length = content_length as i64;
    let (uuid, group_id) = match task {
        StoredTaskModel::Active(ref task) => (task.uuid, task.group_id),
        StoredTaskModel::Archived(ref task) => (task.uuid, task.group_id),
    };
    // Check if group is active and has enough storage quota
    let group = Group::Entity::find_by_id(group_id)
        .one(&pool.db)
        .await?
        .ok_or(ApiError::InvalidRequest(
            "Group for the task not found".to_string(),
        ))?;
    if group.state != GroupState::Active {
        return Err(ApiError::InvalidRequest("Group is not active".to_string()).into());
    }
    let s3_client = pool.s3.clone();
    let url = pool
        .db
        .transaction::<_, String, Error>(|txn| {
            Box::pin(async move {
                // Update the task to reflect the artifact upload
                match task {
                    StoredTaskModel::Active(task) => {
                        let updated_task = ActiveTask::ActiveModel {
                            id: Set(task.id),
                            updated_at: Set(now),
                            ..Default::default()
                        };
                        updated_task.update(txn).await?;
                    }
                    StoredTaskModel::Archived(task) => {
                        let updated_task = ArchivedTask::ActiveModel {
                            id: Set(task.id),
                            updated_at: Set(now),
                            ..Default::default()
                        };
                        updated_task.update(txn).await?;
                    }
                }
                let artifact = Artifact::Entity::find()
                    .filter(Artifact::Column::TaskId.eq(uuid))
                    .filter(Artifact::Column::ContentType.eq(content_type))
                    .one(txn)
                    .await?;
                let s3_object_key = format!("{uuid}/{content_type}");
                let url: String;
                // Check group storage quota and allocate storage for the artifact
                match artifact {
                    Some(artifact) => {
                        let recorded_content_length = content_length.max(artifact.size);
                        let new_storage_used =
                            group.storage_used + (recorded_content_length - artifact.size);
                        if new_storage_used > group.storage_quota {
                            return Err(ApiError::QuotaExceeded.into());
                        }
                        url = get_presigned_upload_link(
                            &s3_client,
                            "mitosis-artifacts",
                            s3_object_key,
                            content_length,
                        )
                        .await
                        .map_err(ApiError::from)?;
                        let artifact = Artifact::ActiveModel {
                            id: Set(artifact.id),
                            size: Set(recorded_content_length),
                            updated_at: Set(now),
                            ..Default::default()
                        };
                        artifact.update(txn).await?;
                        let group = Group::ActiveModel {
                            id: Set(group_id),
                            storage_used: Set(new_storage_used),
                            updated_at: Set(now),
                            ..Default::default()
                        };
                        group.update(txn).await?;
                    }
                    None => {
                        let new_storage_used = group.storage_used + content_length;
                        if new_storage_used > group.storage_quota {
                            return Err(ApiError::QuotaExceeded.into());
                        }
                        url = get_presigned_upload_link(
                            &s3_client,
                            "mitosis-artifacts",
                            s3_object_key,
                            content_length,
                        )
                        .await
                        .map_err(ApiError::from)?;
                        let artifact = Artifact::ActiveModel {
                            task_id: Set(uuid),
                            content_type: Set(content_type),
                            size: Set(content_length),
                            created_at: Set(now),
                            updated_at: Set(now),
                            ..Default::default()
                        };
                        artifact.insert(txn).await?;
                        let group = Group::ActiveModel {
                            id: Set(group_id),
                            storage_used: Set(new_storage_used),
                            updated_at: Set(now),
                            ..Default::default()
                        };
                        group.update(txn).await?;
                    }
                }
                Ok(url)
            })
        })
        .await?;
    Ok(url)
}

pub async fn user_upload_artifact(
    pool: &InfraPool,
    user_id: i64,
    uuid: Uuid,
    content_type: ArtifactContentType,
    content_length: u64,
) -> Result<String, crate::error::Error> {
    // Find the task and group
    let (group_id, task) = match ActiveTask::Entity::find()
        .filter(ActiveTask::Column::Uuid.eq(uuid))
        .one(&pool.db)
        .await?
    {
        Some(task) => (task.group_id, StoredTaskModel::Active(task)),
        None => {
            let task = ArchivedTask::Entity::find()
                .filter(ArchivedTask::Column::Uuid.eq(uuid))
                .one(&pool.db)
                .await?
                .ok_or(ApiError::NotFound(format!("Task {uuid} not found")))?;
            (task.group_id, StoredTaskModel::Archived(task))
        }
    };
    // Check if user has permission to upload artifact to the task
    let user_group = UserGroup::Entity::find()
        .filter(UserGroup::Column::UserId.eq(user_id))
        .filter(UserGroup::Column::GroupId.eq(group_id))
        .one(&pool.db)
        .await?
        .ok_or(AuthError::PermissionDenied)?;
    if user_group.role == UserGroupRole::Read {
        return Err(AuthError::PermissionDenied.into());
    }
    group_upload_artifact(pool, task, content_type, content_length).await
}

#[derive(FromQueryResult)]
struct GroupInfo {
    id: i64,
    group_name: String,
}

pub async fn get_attachment(
    pool: &InfraPool,
    uuid: Uuid,
    key: String,
) -> Result<RemoteResourceDownloadResp, crate::error::Error> {
    let builder = pool.db.get_database_backend();
    let active_group_name_stmt = Query::select()
        .column((Group::Entity, Group::Column::Id))
        .column((Group::Entity, Group::Column::GroupName))
        .from(Group::Entity)
        .join(
            sea_orm::JoinType::Join,
            ActiveTask::Entity,
            Expr::col((ActiveTask::Entity, ActiveTask::Column::GroupId))
                .eq(Expr::col((Group::Entity, Group::Column::Id))),
        )
        .and_where(Expr::col((ActiveTask::Entity, ActiveTask::Column::Uuid)).eq(uuid))
        .limit(1)
        .to_owned();
    let GroupInfo {
        id: group_id,
        group_name,
    } = match GroupInfo::find_by_statement(builder.build(&active_group_name_stmt))
        .one(&pool.db)
        .await?
    {
        Some(g) => g,
        None => {
            let archived_group_name_stmt = Query::select()
                .column((Group::Entity, Group::Column::GroupName))
                .from(Group::Entity)
                .join(
                    sea_orm::JoinType::Join,
                    ArchivedTask::Entity,
                    Expr::col((ArchivedTask::Entity, ArchivedTask::Column::GroupId))
                        .eq(Expr::col((Group::Entity, Group::Column::Id))),
                )
                .and_where(Expr::col((ArchivedTask::Entity, ArchivedTask::Column::Uuid)).eq(uuid))
                .limit(1)
                .to_owned();
            match GroupInfo::find_by_statement(builder.build(&archived_group_name_stmt))
                .one(&pool.db)
                .await?
            {
                Some(g) => g,
                None => {
                    return Err(
                        crate::error::ApiError::NotFound(format!("Task with uuid {uuid}")).into(),
                    )
                }
            }
        }
    };
    let attachment = Attachment::Entity::find()
        .filter(Attachment::Column::GroupId.eq(group_id))
        .filter(Attachment::Column::Key.eq(key.clone()))
        .one(&pool.db)
        .await?
        .ok_or(crate::error::ApiError::NotFound(format!(
            "Attachment of group {group_name} and key {key}"
        )))?;
    let s3_key = format!("{group_name}/{key}");
    let url = get_presigned_download_link(&pool.s3, "mitosis-attachments", s3_key, attachment.size)
        .await?;
    Ok(RemoteResourceDownloadResp {
        url,
        size: attachment.size,
    })
}

pub async fn user_get_attachment_db(
    pool: &InfraPool,
    user_id: i64,
    group_name: String,
    key: String,
) -> Result<AttachmentMetadata, crate::error::Error> {
    let group_id = Group::Entity::find()
        .filter(Group::Column::GroupName.eq(group_name.clone()))
        .one(&pool.db)
        .await?
        .ok_or(crate::error::ApiError::NotFound(format!(
            "Attachment of group {group_name} and key {key}"
        )))?
        .id;
    UserGroup::Entity::find()
        .filter(UserGroup::Column::UserId.eq(user_id))
        .filter(UserGroup::Column::GroupId.eq(group_id))
        .one(&pool.db)
        .await?
        .ok_or(AuthError::PermissionDenied)?;
    let attachment = Attachment::Entity::find()
        .filter(Attachment::Column::GroupId.eq(group_id))
        .filter(Attachment::Column::Key.eq(key.clone()))
        .one(&pool.db)
        .await?
        .ok_or(crate::error::ApiError::NotFound(format!(
            "Attachment of group {group_name} and key {key}"
        )))?;
    Ok(AttachmentMetadata {
        content_type: attachment.content_type,
        size: attachment.size,
        created_at: attachment.created_at,
        updated_at: attachment.updated_at,
    })
}

pub async fn user_get_attachment(
    pool: &InfraPool,
    user_id: i64,
    group_name: String,
    key: String,
) -> Result<RemoteResourceDownloadResp, crate::error::Error> {
    let group_id = Group::Entity::find()
        .filter(Group::Column::GroupName.eq(group_name.clone()))
        .one(&pool.db)
        .await?
        .ok_or(crate::error::ApiError::NotFound(format!(
            "Attachment of group {group_name} and key {key}"
        )))?
        .id;
    UserGroup::Entity::find()
        .filter(UserGroup::Column::UserId.eq(user_id))
        .filter(UserGroup::Column::GroupId.eq(group_id))
        .one(&pool.db)
        .await?
        .ok_or(AuthError::PermissionDenied)?;
    let attachment = Attachment::Entity::find()
        .filter(Attachment::Column::GroupId.eq(group_id))
        .filter(Attachment::Column::Key.eq(key.clone()))
        .one(&pool.db)
        .await?
        .ok_or(crate::error::ApiError::NotFound(format!(
            "Attachment of group {group_name} and key {key}"
        )))?;
    let s3_key = format!("{group_name}/{key}");
    let url = get_presigned_download_link(&pool.s3, "mitosis-attachments", s3_key, attachment.size)
        .await?;
    Ok(RemoteResourceDownloadResp {
        url,
        size: attachment.size,
    })
}

fn check_attachment_key(key: &str) -> bool {
    key.len() >= 1024 || key.contains("/./") || key.contains("/../") || key.contains("//")
}

pub async fn user_upload_attachment(
    user_id: i64,
    pool: &InfraPool,
    group_name: String,
    key: String,
    content_length: u64,
) -> Result<String, crate::error::Error> {
    tracing::debug!(
        "Uploading attachment to group {} with key {} and size {}",
        group_name,
        key,
        content_length
    );
    let s3_object_key = format!("{group_name}/{key}");
    if check_attachment_key(&s3_object_key) {
        return Err(ApiError::InvalidRequest(
            "Invalid attachment key. Should be a relative path pointing to a single location without \"./\" or \"../\""
                .to_string(),
        )
        .into());
    }
    let content_length = content_length as i64;
    let s3_client = pool.s3.clone();
    let now = TimeDateTimeWithTimeZone::now_utc();
    let uri = pool
        .db
        .transaction::<_, String, crate::error::Error>(|txn| {
            Box::pin(async move {
                let group = Group::Entity::find()
                    .filter(Group::Column::GroupName.eq(group_name.clone()))
                    .one(txn)
                    .await?
                    .ok_or(ApiError::NotFound(format!("Group {group_name}")))?;
                if group.state != GroupState::Active {
                    return Err(ApiError::InvalidRequest("Group is not active".to_string()).into());
                }
                let user_group = UserGroup::Entity::find()
                    .filter(UserGroup::Column::UserId.eq(user_id))
                    .filter(UserGroup::Column::GroupId.eq(group.id))
                    .one(txn)
                    .await?
                    .ok_or(AuthError::PermissionDenied)?;
                if user_group.role == UserGroupRole::Read {
                    return Err(AuthError::PermissionDenied.into());
                }
                let attachment = Attachment::Entity::find()
                    .filter(Attachment::Column::GroupId.eq(group.id))
                    .filter(Attachment::Column::Key.eq(key.clone()))
                    .one(txn)
                    .await?;
                let url: String;
                match attachment {
                    Some(attachment) => {
                        let recorded_content_length = content_length.max(attachment.size);
                        let new_storage_used =
                            group.storage_used + (recorded_content_length - attachment.size);
                        if new_storage_used > group.storage_quota {
                            return Err(ApiError::QuotaExceeded.into());
                        }
                        url = get_presigned_upload_link(
                            &s3_client,
                            "mitosis-attachments",
                            s3_object_key,
                            content_length,
                        )
                        .await
                        .map_err(ApiError::from)?;
                        let attachment = Attachment::ActiveModel {
                            id: Set(attachment.id),
                            size: Set(recorded_content_length),
                            updated_at: Set(now),
                            ..Default::default()
                        };
                        attachment.update(txn).await?;
                        let group = Group::ActiveModel {
                            id: Set(group.id),
                            storage_used: Set(new_storage_used),
                            updated_at: Set(now),
                            ..Default::default()
                        };
                        group.update(txn).await?;
                    }
                    None => {
                        let new_storage_used = group.storage_used + content_length;
                        if new_storage_used > group.storage_quota {
                            return Err(ApiError::QuotaExceeded.into());
                        }
                        url = get_presigned_upload_link(
                            &s3_client,
                            "mitosis-attachments",
                            s3_object_key,
                            content_length,
                        )
                        .await
                        .map_err(ApiError::from)?;
                        let attachment = Attachment::ActiveModel {
                            group_id: Set(group.id),
                            key: Set(key.clone()),
                            content_type: Set(AttachmentContentType::NoSet),
                            size: Set(content_length),
                            created_at: Set(now),
                            updated_at: Set(now),
                            ..Default::default()
                        };
                        attachment.insert(txn).await?;
                        let group = Group::ActiveModel {
                            id: Set(group.id),
                            storage_used: Set(new_storage_used),
                            updated_at: Set(now),
                            ..Default::default()
                        };
                        group.update(txn).await?;
                    }
                }
                Ok(url)
            })
        })
        .await?;
    Ok(uri)
}

// show_pb is used to show progress bar
pub async fn download_file(
    client: &reqwest::Client,
    resp: &RemoteResourceDownloadResp,
    local_path: impl AsRef<Path>,
    show_pb: bool,
) -> crate::error::Result<()> {
    tracing::debug!(
        "Downloading file from {} to {:?}",
        resp.url,
        local_path.as_ref()
    );
    let total_size = resp.size as u64;
    let mut pb = None;
    let mut downloaded: u64 = 0;
    let mut resp = client
        .get(&resp.url)
        .send()
        .await
        .map_err(map_reqwest_err)?;
    if resp.status().is_success() {
        if let Some(parent_dir) = local_path.as_ref().parent() {
            tokio::fs::create_dir_all(parent_dir).await?;
        }
        if show_pb {
            let inner_pb = ProgressBar::new(total_size);
            inner_pb.set_style(ProgressStyle::with_template("{msg}\n{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta})")
        .unwrap()
        .with_key("eta", |state: &ProgressState, w: &mut dyn std::fmt::Write| write!(w, "{:.1}s", state.eta().as_secs_f64()).unwrap())
            .progress_chars("=>-"),
        );
            inner_pb.set_message(format!(
                "Downloading to {}",
                local_path.as_ref().to_string_lossy()
            ));
            pb = Some(inner_pb);
        }
        let mut file = tokio::fs::File::create(local_path).await?;
        while let Some(chunk) = resp.chunk().await.map_err(RequestError::from)? {
            file.write_all(&chunk).await?;
            downloaded = std::cmp::min(downloaded + (chunk.len() as u64), total_size);
            if let Some(ref pb) = pb {
                pb.set_position(downloaded);
            }
        }
        file.flush().await?;
        if let Some(ref pb) = pb {
            pb.finish();
        }
        Ok(())
    } else {
        let msg = get_xml_error_message(resp).await?;
        Err(S3Error::Custom(msg).into())
    }
}

pub async fn upload_file(
    client: &reqwest::Client,
    url: &str,
    content_length: u64,
    local_path: impl AsRef<Path>,
    show_pb: bool,
) -> crate::error::Result<()> {
    let file = tokio::fs::File::open(local_path.as_ref()).await?;
    let mut pb = None;
    let mut uploaded: u64 = 0;
    if show_pb {
        let inner_pb = ProgressBar::new(content_length);
        inner_pb.set_style(ProgressStyle::with_template("{msg}\n{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta})")
        .unwrap()
        .with_key("eta", |state: &ProgressState, w: &mut dyn std::fmt::Write| write!(w, "{:.1}s", state.eta().as_secs_f64()).unwrap())
            .progress_chars("=>-"),
        );
        inner_pb.set_message(format!(
            "Uploading from {}",
            local_path.as_ref().to_string_lossy()
        ));
        pb = Some(inner_pb);
    }
    let mut reader_stream = ReaderStream::new(file);
    let async_stream = async_stream::stream! {
        while let Some(chunk) = reader_stream.next().await {
            if let Ok(chunk) = &chunk {
                uploaded = std::cmp::min(uploaded + (chunk.len() as u64), content_length);
                if let Some(ref pb) = pb {
                    pb.set_position(uploaded);
                    if uploaded >= content_length {
                        pb.finish();
                    }
                }
            }
            yield chunk;
        }
    };
    let upload_file = client
        .put(url)
        .header(CONTENT_LENGTH, content_length)
        .body(reqwest::Body::wrap_stream(async_stream))
        .send()
        .await
        .map_err(map_reqwest_err)?;
    if upload_file.status().is_success() {
        Ok(())
    } else {
        let msg = get_xml_error_message(upload_file).await?;
        Err(S3Error::Custom(msg).into())
    }
}

pub(crate) async fn get_xml_error_message(resp: Response) -> crate::error::Result<String> {
    let body = resp.text().await.map_err(RequestError::from)?;
    let xml = roxmltree::Document::parse(&body)?;
    match xml
        .descendants()
        .find(|n| n.has_tag_name("Message"))
        .and_then(|node| node.text())
    {
        Some(msg) => Ok(msg.to_string()),
        None => Ok(body),
    }
}

#[derive(FromQueryResult)]
struct UserGroupRoleQueryRes {
    role: UserGroupRole,
}

async fn check_task_list_query(
    user_id: i64,
    pool: &InfraPool,
    query: &mut AttachmentsQueryReq,
) -> crate::error::Result<()> {
    match query.group_name {
        Some(ref group_name) => {
            let builder = pool.db.get_database_backend();
            let role_stmt = Query::select()
                .column((UserGroup::Entity, UserGroup::Column::Role))
                .from(UserGroup::Entity)
                .join(
                    sea_orm::JoinType::Join,
                    Group::Entity,
                    Expr::col((Group::Entity, Group::Column::Id))
                        .eq(Expr::col((UserGroup::Entity, UserGroup::Column::GroupId))),
                )
                .and_where(Expr::col((UserGroup::Entity, UserGroup::Column::UserId)).eq(user_id))
                .and_where(
                    Expr::col((Group::Entity, Group::Column::GroupName)).eq(group_name.clone()),
                )
                .to_owned();
            let role = UserGroupRoleQueryRes::find_by_statement(builder.build(&role_stmt))
                .one(&pool.db)
                .await?
                .map(|r| r.role);
            if role.is_none() {
                return Err(Error::ApiError(crate::error::ApiError::InvalidRequest(
                    format!("Group with name {group_name} not found or user is not in the group"),
                )));
            }
        }
        None => {
            let username = User::Entity::find()
                .filter(User::Column::Id.eq(user_id))
                .one(&pool.db)
                .await?
                .ok_or(Error::ApiError(crate::error::ApiError::NotFound(
                    "User".to_string(),
                )))?
                .username;
            tracing::debug!("No group name specified, use username {} instead", username);
            query.group_name = Some(username);
        }
    }
    Ok(())
}

pub async fn query_attachment_list(
    user_id: i64,
    pool: &InfraPool,
    mut query: AttachmentsQueryReq,
) -> Result<AttachmentsQueryResp, crate::error::Error> {
    check_task_list_query(user_id, pool, &mut query).await?;
    let key_prefix = query.key_prefix.take().unwrap_or_default();
    let group_name = query.group_name.unwrap();
    let mut attachment_stmt = Query::select();
    if query.count {
        attachment_stmt.expr(Expr::col((Attachment::Entity, Attachment::Column::Id)).count());
    } else {
        attachment_stmt.columns([
            (Attachment::Entity, Attachment::Column::Key),
            (Attachment::Entity, Attachment::Column::ContentType),
            (Attachment::Entity, Attachment::Column::Size),
            (Attachment::Entity, Attachment::Column::CreatedAt),
            (Attachment::Entity, Attachment::Column::UpdatedAt),
        ]);
    }
    attachment_stmt
        .from(Attachment::Entity)
        .join(
            sea_orm::JoinType::Join,
            Group::Entity,
            Expr::col((Group::Entity, Group::Column::Id))
                .eq(Expr::col((Attachment::Entity, Attachment::Column::GroupId))),
        )
        .and_where(Expr::col((Group::Entity, Group::Column::GroupName)).eq(group_name.clone()))
        .and_where(
            Expr::col((Attachment::Entity, Attachment::Column::Key))
                .like(format!("%{key_prefix}%")),
        );
    if let Some(limit) = query.limit {
        attachment_stmt.limit(limit);
    }
    if let Some(offset) = query.offset {
        attachment_stmt.offset(offset);
    }
    let builder = pool.db.get_database_backend();
    let resp = if query.count {
        let count = CountQuery::find_by_statement(builder.build(&attachment_stmt))
            .one(&pool.db)
            .await?
            .map(|c| c.count)
            .unwrap_or(0) as u64;
        AttachmentsQueryResp {
            count,
            attachments: vec![],
            group_name,
        }
    } else {
        let attachments = AttachmentQueryInfo::find_by_statement(builder.build(&attachment_stmt))
            .all(&pool.db)
            .await?;
        AttachmentsQueryResp {
            count: attachments.len() as u64,
            attachments,
            group_name,
        }
    };
    Ok(resp)
}

use std::path::Path;
use std::time::Duration;

use aws_sdk_s3::{error::SdkError, presigning::PresigningConfig, Client};
use reqwest::Response;
use sea_orm::{
    prelude::*,
    sea_query::{Expr, Query},
    FromQueryResult, Set, TransactionTrait,
};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::error::{map_reqwest_err, ApiError, RequestError};
use crate::schema::RemoteResourceDownloadResp;
use crate::{config::InfraPool, error::S3Error};
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
    // Restrict the link to be valid for at most 15 minutes
    let expires = Duration::from_secs(900);
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
    // At least valid for 3 days and at most valid for 7 days
    let expires = Duration::from_secs(604800.min(259200.max(length as u64 / 1000000)));
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
            "Artifact with uuid {} and content type {}",
            uuid, content_type
        )))?;
    let key = format!("{}/{}", uuid, content_type);
    let url =
        get_presigned_download_link(&pool.s3, "mitosis-artifacts", key, artifact.size).await?;
    Ok(RemoteResourceDownloadResp {
        url,
        size: artifact.size,
    })
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
                    return Err(crate::error::ApiError::NotFound(format!(
                        "Task with uuid {}",
                        uuid
                    ))
                    .into())
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
            "Attachment of group {} and key {}",
            group_name, key
        )))?;
    let s3_key = format!("{}/{}", group_name, key);
    let url = get_presigned_download_link(&pool.s3, "mitosis-attachments", s3_key, attachment.size)
        .await?;
    Ok(RemoteResourceDownloadResp {
        url,
        size: attachment.size,
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
            "Attachment of group {} and key {}",
            group_name, key
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
            "Attachment of group {} and key {}",
            group_name, key
        )))?;
    let s3_key = format!("{}/{}", group_name, key);
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
    let s3_object_key = format!("{}/{}", group_name, key);
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
                    .ok_or(ApiError::NotFound(format!("Group {}", group_name)))?;
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

pub async fn download_file(
    client: &reqwest::Client,
    resp: &RemoteResourceDownloadResp,
    local_path: impl AsRef<Path>,
) -> crate::error::Result<()> {
    tracing::debug!(
        "Downloading file from {} to {:?}",
        resp.url,
        local_path.as_ref()
    );
    let mut resp = client
        .get(&resp.url)
        .send()
        .await
        .map_err(map_reqwest_err)?;
    if resp.status().is_success() {
        if let Some(parent_dir) = local_path.as_ref().parent() {
            tokio::fs::create_dir_all(parent_dir).await?;
        }
        let mut file = tokio::fs::File::create(local_path).await?;
        while let Some(chunk) = resp.chunk().await.map_err(RequestError::from)? {
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        Ok(())
    } else {
        let msg = get_xml_error_message(resp).await?;
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
                    format!(
                        "Group with name {} not found or user is not in the group",
                        group_name
                    ),
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
) -> Result<Vec<AttachmentQueryInfo>, crate::error::Error> {
    check_task_list_query(user_id, pool, &mut query).await?;
    let key_prefix = query.key_prefix.take().unwrap_or_default();
    let group_name = query.group_name.unwrap();
    let mut attachment_stmt = Query::select();
    attachment_stmt
        .columns([
            (Attachment::Entity, Attachment::Column::Key),
            (Attachment::Entity, Attachment::Column::ContentType),
            (Attachment::Entity, Attachment::Column::Size),
            (Attachment::Entity, Attachment::Column::CreatedAt),
            (Attachment::Entity, Attachment::Column::UpdatedAt),
        ])
        .from(Attachment::Entity)
        .join(
            sea_orm::JoinType::Join,
            Group::Entity,
            Expr::col((Group::Entity, Group::Column::Id))
                .eq(Expr::col((Attachment::Entity, Attachment::Column::GroupId))),
        )
        .and_where(Expr::col((Group::Entity, Group::Column::GroupName)).eq(group_name))
        .and_where(
            Expr::col((Attachment::Entity, Attachment::Column::Key))
                .like(format!("{}%", key_prefix)),
        );
    if let Some(limit) = query.limit {
        attachment_stmt.limit(limit);
    }
    if let Some(offset) = query.offset {
        attachment_stmt.offset(offset);
    }
    let builder = pool.db.get_database_backend();
    let attachments = AttachmentQueryInfo::find_by_statement(builder.build(&attachment_stmt))
        .all(&pool.db)
        .await?;
    Ok(attachments)
}

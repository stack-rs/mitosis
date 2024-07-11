use std::path::Path;
use std::time::Duration;

use aws_sdk_s3::{error::SdkError, presigning::PresigningConfig, Client};
use sea_orm::{
    prelude::*,
    sea_query::{Expr, Query},
    FromQueryResult, Set, TransactionTrait,
};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::entity::{
    active_tasks as ActiveTask, archived_tasks as ArchivedTask, artifacts as Artifact,
    attachments as Attachment, content::ArtifactContentType, groups as Group,
};
use crate::entity::{content::AttachmentContentType, state::GroupState};
use crate::error::{get_error_from_resp, map_reqwest_err, ApiError, RequestError};
use crate::schema::RemoteResourceDownloadResp;
use crate::{config::InfraPool, error::S3Error};

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
    // At least valid for 1 day and at most valid for 7 days
    let expires = Duration::from_secs(604800.min(86400.max(length as u64 / 1000000)));
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
    // At least valid for 3 days and at most valid for 10 days
    let expires = Duration::from_secs(864000.min(259200.max(length as u64 / 1000000)));
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
    name: String,
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
        name: group_name,
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

pub async fn user_upload_attachment(
    pool: &InfraPool,
    group_name: String,
    key: String,
    content_length: i64,
) -> Result<String, crate::error::Error> {
    tracing::debug!(
        "Uploading attachment to group {} with key {} and size {}",
        group_name,
        key,
        content_length
    );
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
                let attachment = Attachment::Entity::find()
                    .filter(Attachment::Column::GroupId.eq(group.id))
                    .filter(Attachment::Column::Key.eq(key.clone()))
                    .one(txn)
                    .await?;
                match attachment {
                    Some(attachment) => {
                        let new_storage_used =
                            group.storage_used + content_length - attachment.size;
                        if new_storage_used > group.storage_quota {
                            return Err(ApiError::QuotaExceeded.into());
                        }
                        let attachment = Attachment::ActiveModel {
                            id: Set(attachment.id),
                            group_id: Set(group.id),
                            key: Set(key.clone()),
                            content_type: Set(AttachmentContentType::NoSet),
                            size: Set(content_length),
                            created_at: Set(now),
                            updated_at: Set(now),
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
                let key = format!("{}/{}", group_name, key);
                Ok(get_presigned_upload_link(
                    &s3_client,
                    "mitosis-attachments",
                    key,
                    content_length,
                )
                .await?)
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
        Ok(())
    } else {
        Err(get_error_from_resp(resp).await.into())
    }
}

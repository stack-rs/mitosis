use std::path::Path;
use std::time::Duration;

use aws_sdk_s3::{error::SdkError, presigning::PresigningConfig, Client};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::entity::{artifacts as Artifact, content::ArtifactContentType};
use crate::error::{get_error_from_resp, map_reqwest_err, RequestError};
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

pub async fn download_file(
    client: &reqwest::Client,
    resp: &RemoteResourceDownloadResp,
    local_path: impl AsRef<Path>,
) -> crate::error::Result<()> {
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

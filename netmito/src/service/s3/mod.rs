use std::time::Duration;

use aws_sdk_s3::{error::SdkError, presigning::PresigningConfig, Client};

use crate::error::S3Error;

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
    key: T,
    length: i64,
) -> Result<String, S3Error> {
    if length <= 0 {
        return Err(S3Error::InvalidContentLength(length));
    }
    // At least valid for 1 day
    let expires = Duration::from_secs(86400.max(length as u64 / 1000000));
    let resp = client
        .put_object()
        .bucket("mitosis-tasks")
        .key(key)
        .content_length(length)
        .presigned(PresigningConfig::expires_in(expires)?)
        .await?;
    Ok(resp.uri().to_string())
}

pub async fn get_presigned_download_link<T: Into<String>>(
    client: &Client,
    key: T,
    length: i64,
) -> Result<String, S3Error> {
    // At least valid for 3 days
    let expires = Duration::from_secs(259200.max(length as u64 / 1000000));
    let resp = client
        .get_object()
        .bucket("mitosis-tasks")
        .key(key)
        .presigned(PresigningConfig::expires_in(expires)?)
        .await?;
    Ok(resp.uri().to_string())
}

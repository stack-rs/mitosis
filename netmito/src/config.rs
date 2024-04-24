use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use aws_sdk_s3::{
    config::{Credentials, Region},
    Client as S3Client,
};
use clap::Args;
use figment::{
    providers::{Env, Format, Serialized, Toml},
    value::magic::RelativePathBuf,
    Figment,
};
use once_cell::sync::OnceCell;
use sea_orm::DatabaseConnection;
use serde::{Deserialize, Serialize};
use time::Duration;

pub const DEFAULT_COORDINATOR_ADDR: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 5000);

#[derive(Deserialize, Serialize, Debug)]
pub(crate) struct CoordinatorConfig {
    pub(crate) bind: SocketAddr,
    pub(crate) db_url: String,
    pub(crate) s3_url: String,
    pub(crate) s3_access_key: String,
    pub(crate) s3_secret_key: String,
    pub(crate) admin_username: String,
    pub(crate) admin_password: String,
    pub(crate) access_token_private_path: RelativePathBuf,
    pub(crate) access_token_public_path: RelativePathBuf,
    #[serde(with = "humantime_serde")]
    pub(crate) access_token_expires_in: std::time::Duration,
}

#[derive(Args, Debug, Serialize)]
#[command(rename_all = "kebab-case")]
pub struct CoordinatorConfigCli {
    /// The address to bind to
    #[arg(short, long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub bind: Option<String>,
    /// The path of the config file
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub config: Option<String>,
    /// The database URL
    #[arg(long = "db")]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub db_url: Option<String>,
    /// The S3 URL
    #[arg(long = "s3")]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub s3_url: Option<String>,
    /// The S3 access key
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub s3_access_key: Option<String>,
    /// The S3 secret key
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub s3_secret_key: Option<String>,
    /// The admin username
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub admin_username: Option<String>,
    /// The admin password
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub admin_password: Option<String>,
    /// The path to the private key, default to `private.pem`
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub access_token_private_path: Option<String>,
    /// The path to the public key, default to `public.pem`
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub access_token_public_path: Option<String>,
    /// The access token expiration time, default to 7 days
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub access_token_expires_in: Option<String>,
}

impl Default for CoordinatorConfig {
    fn default() -> Self {
        Self {
            bind: DEFAULT_COORDINATOR_ADDR,
            db_url: "postgres://mitosis:mitosis@localhost/mitosis".to_string(),
            s3_url: "http://localhost:9000".to_string(),
            s3_access_key: "mitosis_access".to_string(),
            s3_secret_key: "mitosis_secret".to_string(),
            admin_username: "mitosis_admin".to_string(),
            admin_password: "mitosis_admin".to_string(),
            access_token_private_path: "private.pem".to_string().into(),
            access_token_public_path: "public.pem".to_string().into(),
            access_token_expires_in: std::time::Duration::from_secs(60 * 60 * 24 * 7),
        }
    }
}

impl CoordinatorConfig {
    pub(crate) fn new(cli: &mut CoordinatorConfigCli) -> crate::error::Result<Self> {
        Ok(Figment::new()
            .merge(Serialized::from(Self::default(), "coordinator"))
            .merge(Toml::file(cli.config.as_deref().unwrap_or("config.toml")).nested())
            .merge(Env::prefixed("MITO_").profile("coordinator"))
            .merge(Serialized::from(cli, "coordinator"))
            .select("coordinator")
            .extract()?)
    }

    pub(crate) async fn build_infra_pool(&self) -> crate::error::Result<InfraPool> {
        let db = sea_orm::Database::connect(&self.db_url).await?;
        let credential = Credentials::new(
            &self.s3_access_key,
            &self.s3_secret_key,
            None,
            None,
            "mitosis",
        );
        let config: aws_sdk_s3::Config = aws_sdk_s3::Config::builder()
            .credentials_provider(credential)
            .endpoint_url(self.s3_url.clone())
            .region(Region::from_static("mitosis"))
            .force_path_style(true)
            .build();
        let s3 = S3Client::from_conf(config);
        Ok(InfraPool { db, s3 })
    }

    pub(crate) async fn build_admin_user(&self) -> crate::error::Result<InitAdminUser> {
        if self.admin_password.len() > 255 || self.admin_username.len() > 255 {
            Err(crate::error::Error::InvalidConfig(figment::Error::from(
                "username or password too long",
            )))
        } else {
            Ok(InitAdminUser {
                username: self.admin_username.clone(),
                password: self.admin_password.clone(),
            })
        }
    }

    pub(crate) async fn build_server_config(&self) -> crate::error::Result<ServerConfig> {
        Ok(ServerConfig {
            bind: self.bind,
            token_expires_in: Duration::try_from(self.access_token_expires_in)
                .map_err(|e| figment::Error::from(e.to_string()))?,
        })
    }

    pub(crate) async fn build_private_key(&self) -> crate::error::Result<Vec<u8>> {
        Ok(tokio::fs::read(&self.access_token_private_path.relative()).await?)
    }

    pub(crate) async fn build_public_key(&self) -> crate::error::Result<Vec<u8>> {
        Ok(tokio::fs::read(&self.access_token_public_path.relative()).await?)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct InfraPool {
    pub(crate) db: DatabaseConnection,
    pub(crate) s3: S3Client,
}

#[derive(Debug)]
pub(crate) struct ServerConfig {
    pub(crate) bind: SocketAddr,
    pub(crate) token_expires_in: Duration,
}

#[derive(Debug)]
pub(crate) struct InitAdminUser {
    pub(crate) username: String,
    pub(crate) password: String,
}

pub(crate) static ServerConfig: OnceCell<ServerConfig> = OnceCell::new();
pub(crate) static InitAdminUser: OnceCell<InitAdminUser> = OnceCell::new();
pub(crate) static PrivateKey: OnceCell<Vec<u8>> = OnceCell::new();
pub(crate) static PublicKey: OnceCell<Vec<u8>> = OnceCell::new();

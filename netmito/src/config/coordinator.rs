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
use jsonwebtoken::{DecodingKey, EncodingKey};
use once_cell::sync::OnceCell;
use redis::{acl::Rule, AsyncCommands};
use sea_orm::DatabaseConnection;
use serde::{Deserialize, Serialize};
use time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, Layer};

use crate::{
    error::Error,
    service::worker::{HeartbeatOp, HeartbeatQueue, TaskDispatcher, TaskDispatcherOp},
};

use super::TracingGuard;

pub const DEFAULT_COORDINATOR_ADDR: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 5000);

#[derive(Deserialize, Serialize, Debug)]
pub struct CoordinatorConfig {
    pub(crate) bind: SocketAddr,
    pub(crate) db_url: String,
    pub(crate) s3_url: String,
    pub(crate) s3_access_key: String,
    pub(crate) s3_secret_key: String,
    pub(crate) redis_url: Option<String>,
    pub(crate) redis_worker_password: Option<String>,
    pub(crate) redis_client_password: Option<String>,
    pub(crate) admin_user: String,
    pub(crate) admin_password: String,
    pub(crate) access_token_private_path: RelativePathBuf,
    pub(crate) access_token_public_path: RelativePathBuf,
    #[serde(with = "humantime_serde")]
    pub(crate) access_token_expires_in: std::time::Duration,
    #[serde(with = "humantime_serde")]
    pub(crate) heartbeat_timeout: std::time::Duration,
    pub(crate) log_path: Option<RelativePathBuf>,
    pub(crate) file_log: bool,
}

#[derive(Args, Debug, Serialize, Default)]
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
    /// The Redis URL
    #[arg(long = "redis")]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub redis_url: Option<String>,
    /// The Redis worker password
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub redis_worker_password: Option<String>,
    /// The Redis client password
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub redis_client_password: Option<String>,
    /// The admin username
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub admin_user: Option<String>,
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
    /// The heartbeat timeout, default to 600 seconds
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub heartbeat_timeout: Option<String>,
    /// The log file path. If not specified, then the default rolling log file path would be used.
    /// If specified, then the log file would be exactly at the path specified.
    #[arg(long)]
    #[serde(skip_serializing_if = "::std::option::Option::is_none")]
    pub log_path: Option<String>,
    /// Enable logging to file
    #[arg(long)]
    pub file_log: bool,
}

impl Default for CoordinatorConfig {
    fn default() -> Self {
        Self {
            bind: DEFAULT_COORDINATOR_ADDR,
            db_url: "postgres://mitosis:mitosis@localhost/mitosis".to_string(),
            redis_url: None,
            redis_worker_password: None,
            redis_client_password: None,
            s3_url: "http://localhost:9000".to_string(),
            s3_access_key: "mitosis_access".to_string(),
            s3_secret_key: "mitosis_secret".to_string(),
            admin_user: "mitosis_admin".to_string(),
            admin_password: "mitosis_admin".to_string(),
            access_token_private_path: "private.pem".to_string().into(),
            access_token_public_path: "public.pem".to_string().into(),
            access_token_expires_in: std::time::Duration::from_secs(60 * 60 * 24 * 7),
            heartbeat_timeout: std::time::Duration::from_secs(600),
            log_path: None,
            file_log: false,
        }
    }
}

impl CoordinatorConfig {
    pub fn new(cli: &CoordinatorConfigCli) -> crate::error::Result<Self> {
        Ok(Figment::new()
            .merge(Serialized::from(Self::default(), "coordinator"))
            .merge(Toml::file(cli.config.as_deref().unwrap_or("config.toml")).nested())
            .merge(Env::prefixed("MITO_").profile("coordinator"))
            .merge(Serialized::from(cli, "coordinator"))
            .select("coordinator")
            .extract()?)
    }

    pub fn build_worker_task_queue(
        &self,
        cancel_token: CancellationToken,
        rx: UnboundedReceiver<TaskDispatcherOp>,
    ) -> TaskDispatcher {
        TaskDispatcher::new(cancel_token, rx)
    }

    pub fn build_worker_heartbeat_queue(
        &self,
        cancel_token: CancellationToken,
        pool: InfraPool,
        rx: UnboundedReceiver<HeartbeatOp>,
    ) -> HeartbeatQueue {
        HeartbeatQueue::new(cancel_token, self.heartbeat_timeout, pool, rx)
    }

    pub async fn build_redis_connection_info(
        &self,
    ) -> crate::error::Result<Option<RedisConnectionInfo>> {
        match self.redis_url {
            Some(ref redis_url) => {
                let client = redis::Client::open(redis_url.clone())?;
                let mut conn = client.get_multiplexed_tokio_connection().await?;
                let rules = [Rule::Reset];
                let _: String = conn.acl_setuser_rules("mitosis_worker", &rules).await?;
                let _: String = conn.acl_setuser_rules("mitosis_client", &rules).await?;
                let worker_pass = {
                    if let Some(worker_pass) = &self.redis_worker_password {
                        worker_pass.clone()
                    } else {
                        conn.acl_genpass().await?
                    }
                };
                let rules = [
                    Rule::On,
                    Rule::AddPass(worker_pass.clone()),
                    Rule::Pattern("task:*".to_string()),
                    Rule::Other("&task:*".to_string()),
                    Rule::AddCategory("read".to_string()),
                    Rule::AddCategory("write".to_string()),
                    Rule::AddCategory("connection".to_string()),
                    Rule::AddCategory("pubsub".to_string()),
                    Rule::RemoveCategory("dangerous".to_string()),
                ];
                let client_pass = {
                    if let Some(client_pass) = &self.redis_client_password {
                        client_pass.clone()
                    } else {
                        conn.acl_genpass().await?
                    }
                };
                let _: String = conn.acl_setuser_rules("mitosis_worker", &rules).await?;
                let rules = [
                    Rule::On,
                    Rule::AddPass(client_pass.clone()),
                    Rule::Pattern("task:*".to_string()),
                    Rule::Other("&task:*".to_string()),
                    Rule::AddCategory("read".to_string()),
                    Rule::AddCategory("pubsub".to_string()),
                    Rule::AddCategory("connection".to_string()),
                    Rule::RemoveCategory("dangerous".to_string()),
                ];
                let _: String = conn.acl_setuser_rules("mitosis_client", &rules).await?;
                let conn_info = client.get_connection_info();
                Ok(Some(RedisConnectionInfo::new(
                    conn_info.addr.clone(),
                    worker_pass,
                    client_pass,
                )))
            }
            None => Ok(None),
        }
    }

    pub async fn build_infra_pool(
        &self,
        worker_task_queue_tx: UnboundedSender<TaskDispatcherOp>,
        worker_heartbeat_queue_tx: UnboundedSender<HeartbeatOp>,
    ) -> crate::error::Result<InfraPool> {
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
        Ok(InfraPool {
            db,
            s3,
            worker_task_queue_tx,
            worker_heartbeat_queue_tx,
        })
    }

    pub fn build_admin_user(&self) -> crate::error::Result<InitAdminUser> {
        if self.admin_password.len() > 255 || self.admin_user.len() > 255 {
            Err(crate::error::Error::ConfigError(figment::Error::from(
                "username or password too long",
            )))
        } else {
            Ok(InitAdminUser {
                username: self.admin_user.clone(),
                password: self.admin_password.clone(),
            })
        }
    }

    pub fn build_server_config(&self) -> crate::error::Result<ServerConfig> {
        Ok(ServerConfig {
            bind: self.bind,
            token_expires_in: Duration::try_from(self.access_token_expires_in)
                .map_err(|e| figment::Error::from(e.to_string()))?,
        })
    }

    pub async fn build_jwt_encoding_key(&self) -> crate::error::Result<EncodingKey> {
        let private_key = tokio::fs::read(&self.access_token_private_path.relative()).await?;
        Ok(EncodingKey::from_ed_pem(&private_key)?)
    }

    pub async fn build_jwt_decoding_key(&self) -> crate::error::Result<DecodingKey> {
        let public_key = tokio::fs::read(&self.access_token_public_path.relative()).await?;
        Ok(DecodingKey::from_ed_pem(&public_key)?)
    }

    pub fn setup_tracing_subscriber(&self) -> crate::error::Result<TracingGuard> {
        if self.file_log {
            let file_logger = self
                .log_path
                .as_ref()
                .and_then(|p| {
                    let path = p.relative();
                    let dir = path.parent();
                    let file_name = path.file_name();
                    match (dir, file_name) {
                        (Some(dir), Some(file_name)) => {
                            Some(tracing_appender::rolling::never(dir, file_name))
                        }
                        _ => None,
                    }
                })
                .or_else(|| {
                    dirs::cache_dir()
                        .map(|mut p| {
                            p.push("mitosis");
                            p.push("coordinator");
                            p
                        })
                        .map(|dir| {
                            tracing_appender::rolling::daily(dir, format!("{}.log", self.bind))
                        })
                })
                .ok_or(Error::ConfigError(figment::Error::from(
                    "log path not valid and cache directory not found",
                )))?;
            let (non_blocking, guard) = tracing_appender::non_blocking(file_logger);
            let env_filter = tracing_subscriber::EnvFilter::try_from_env("MITO_FILE_LOG")
                .unwrap_or_else(|_| "netmito=info".into());
            let coordinator_guard = tracing_subscriber::registry()
                .with(
                    tracing_subscriber::fmt::layer().with_filter(
                        tracing_subscriber::EnvFilter::try_from_default_env()
                            .unwrap_or_else(|_| "netmito=info".into()),
                    ),
                )
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_writer(non_blocking)
                        .with_filter(env_filter),
                )
                .set_default();
            Ok(TracingGuard {
                subscriber_guard: Some(coordinator_guard),
                file_guard: Some(guard),
            })
        } else {
            let coordinator_guard = tracing_subscriber::registry()
                .with(
                    tracing_subscriber::fmt::layer().with_filter(
                        tracing_subscriber::EnvFilter::try_from_default_env()
                            .unwrap_or_else(|_| "netmito=info".into()),
                    ),
                )
                .set_default();
            Ok(TracingGuard {
                subscriber_guard: Some(coordinator_guard),
                file_guard: None,
            })
        }
    }
}

#[derive(Debug, Clone)]
pub struct InfraPool {
    pub db: DatabaseConnection,
    pub s3: S3Client,
    pub worker_task_queue_tx: UnboundedSender<TaskDispatcherOp>,
    pub worker_heartbeat_queue_tx: UnboundedSender<HeartbeatOp>,
}

#[derive(Debug)]
pub struct ServerConfig {
    pub bind: SocketAddr,
    pub token_expires_in: Duration,
}

#[derive(Debug)]
pub struct InitAdminUser {
    pub username: String,
    pub password: String,
}

#[derive(Debug)]
pub struct RedisConnectionInfo {
    addr: redis::ConnectionAddr,
    worker_pass: String,
    client_pass: String,
}

impl RedisConnectionInfo {
    pub fn new(addr: redis::ConnectionAddr, worker_pass: String, client_pass: String) -> Self {
        Self {
            addr,
            worker_pass,
            client_pass,
        }
    }

    pub fn worker_url(&self) -> String {
        format!(
            "redis://mitosis_worker:{}@{}/?protocol=resp3",
            self.worker_pass, self.addr
        )
    }

    pub fn client_url(&self) -> String {
        format!(
            "redis://mitosis_client:{}@{}/?protocol=resp3",
            self.client_pass, self.addr
        )
    }
}

pub(crate) static SERVER_CONFIG: OnceCell<ServerConfig> = OnceCell::new();
pub(crate) static INIT_ADMIN_USER: OnceCell<InitAdminUser> = OnceCell::new();
pub(crate) static ENCODING_KEY: OnceCell<EncodingKey> = OnceCell::new();
pub(crate) static DECODING_KEY: OnceCell<DecodingKey> = OnceCell::new();
pub(crate) static REDIS_CONNECTION_INFO: OnceCell<RedisConnectionInfo> = OnceCell::new();

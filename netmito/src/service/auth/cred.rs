use std::path::PathBuf;

use base64::{engine::general_purpose, Engine as _};
use figment::value::magic::RelativePathBuf;
use reqwest::Client;
use tokio::io::AsyncBufReadExt;
use url::Url;

use crate::{
    error::{ApiError, Error, ErrorMsg, RequestError},
    schema::UserLoginReq,
    service::auth::fill_user_login,
};

fn normalize_coordinator_addr(url: &Url) -> String {
    url.origin().ascii_serialization()
}

fn encode_coordinator_addr(url: &Url) -> String {
    general_purpose::STANDARD.encode(normalize_coordinator_addr(url))
}

fn parse_cached_credential(line: &str) -> Option<(&str, &str, &str)> {
    let mut parts = line.split(':');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(coordinator_addr), Some(username), Some(credential), None) => {
            Some((coordinator_addr, username, credential))
        }
        _ => None,
    }
}

pub trait GetPathBuf {
    fn get_path_buf(&self) -> PathBuf;
}

impl GetPathBuf for RelativePathBuf {
    fn get_path_buf(&self) -> PathBuf {
        self.relative()
    }
}

impl GetPathBuf for PathBuf {
    fn get_path_buf(&self) -> PathBuf {
        self.into()
    }
}

impl GetPathBuf for std::path::Path {
    fn get_path_buf(&self) -> PathBuf {
        self.to_path_buf()
    }
}

// pub fn validate_cred(token: &str, username: Option<&String>) -> bool {
//     match decode_base64(token) {
//         Ok(token) => {
//             let (_, message) = expect_two!(token.rsplitn(2, '.'));
//             let (payload, _) = expect_two!(message.rsplitn(2, '.'));
//             if let Ok(claims) = general_purpose::URL_SAFE_NO_PAD.decode(payload) {
//                 let claims: TokenClaims = serde_json::from_slice(&claims).unwrap();
//                 let now = OffsetDateTime::now_utc();
//                 // Check if credential is expired
//                 if claims.exp < now {
//                     tracing::warn!("Credential expired");
//                     return false;
//                 }
//                 // If username specified, check if it matches the username in credential
//                 if let Some(username) = username {
//                     if claims.sub != *username {
//                         tracing::warn!("Username mismatch with credential");
//                         return false;
//                     }
//                 }
//                 true
//             } else {
//                 false
//             }
//         }
//         Err(_) => false,
//     }
// }

async fn read_lines<P>(
    filename: P,
) -> std::io::Result<tokio::io::Lines<tokio::io::BufReader<tokio::fs::File>>>
where
    P: AsRef<std::path::Path>,
{
    let file = tokio::fs::File::open(filename).await?;
    Ok(tokio::io::BufReader::new(file).lines())
}

async fn extract_credential(
    coordinator_addr: &str,
    user: Option<&String>,
    lines: &mut tokio::io::Lines<tokio::io::BufReader<tokio::fs::File>>,
) -> std::io::Result<Option<(String, String)>> {
    while let Some(line) = lines.next_line().await? {
        let Some((cached_coordinator_addr, username, credential)) = parse_cached_credential(&line)
        else {
            continue;
        };
        if cached_coordinator_addr != coordinator_addr {
            continue;
        }
        if let Some(user) = user {
            if username != user.as_str() {
                continue;
            }
        }
        return Ok(Some((username.to_owned(), credential.to_owned())));
    }
    Ok(None)
}

pub(crate) async fn modify_or_append_credential(
    cred_path: &std::path::PathBuf,
    coordinator_url: &Url,
    username: &str,
    token: &str,
) -> crate::error::Result<()> {
    let coordinator_addr = encode_coordinator_addr(coordinator_url);
    let credential_line = format!("{coordinator_addr}:{username}:{token}");

    if cred_path.exists() {
        let mut lines = read_lines(cred_path).await?;
        let mut new_lines = Vec::new();
        let mut found = false;
        while let Some(line) = lines.next_line().await? {
            let Some((cached_coordinator_addr, cached_username, _)) =
                parse_cached_credential(&line)
            else {
                new_lines.push(line);
                continue;
            };
            if cached_coordinator_addr == coordinator_addr.as_str() && cached_username == username {
                new_lines.push(credential_line.clone());
                found = true;
            } else {
                new_lines.push(line);
            }
        }
        if !found {
            new_lines.push(credential_line);
        }
        tokio::fs::write(cred_path, new_lines.join("\n")).await?;
    } else {
        tokio::fs::write(cred_path, credential_line).await?;
    }
    Ok(())
}

// The return value is a tuple of username and token
pub async fn get_user_credential(
    cred_path: Option<&RelativePathBuf>,
    client: &Client,
    mut url: Url,
    user: Option<String>,
    password: Option<String>,
    retain: bool,
) -> crate::error::Result<(String, String)> {
    let coordinator_addr = encode_coordinator_addr(&url);

    // Try to load credential from file
    let cred_path = cred_path
        .map(|p| p.relative())
        .or_else(|| {
            dirs::config_dir().map(|mut p| {
                p.push("mitosis");
                p.push("credentials");
                p
            })
        })
        .ok_or(Error::ConfigError(Box::new(figment::Error::from(
            "credential path not found",
        ))))?;
    // Check if the credential is valid
    if cred_path.exists() {
        if let Ok(mut lines) = read_lines(&cred_path).await {
            if let Some((username, cred)) =
                extract_credential(&coordinator_addr, user.as_ref(), &mut lines).await?
            {
                url.set_path("auth");
                let resp = client
                    .get(url.as_str())
                    .bearer_auth(&cred)
                    .send()
                    .await
                    .map_err(|e| {
                        if e.is_request() && e.is_connect() {
                            url.set_path("");
                            RequestError::ConnectionError(url.to_string())
                        } else {
                            e.into()
                        }
                    })?;
                if resp.status().is_success() {
                    let resp_name = resp.text().await.map_err(RequestError::from)?;
                    if resp_name == username {
                        return Ok((username, cred));
                    }
                } else if resp.status().is_server_error() {
                    return Err(ApiError::InternalServerError.into());
                }
            }
        }
    }
    // Local credential not found or invalid, need to login
    tracing::warn!("Local credential not found or invalid, need to login");
    let req = fill_user_login(user, password, retain)?;
    url.set_path("login");
    let resp = client
        .post(url.as_str())
        .json(&req)
        .send()
        .await
        .map_err(|e| {
            if e.is_request() && e.is_connect() {
                url.set_path("");
                RequestError::ConnectionError(url.to_string())
            } else {
                e.into()
            }
        })?;
    if resp.status().is_success() {
        let resp = resp
            .json::<crate::schema::UserLoginResp>()
            .await
            .map_err(RequestError::from)?;
        let token = resp.token;
        if let Some(parent) = cred_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        modify_or_append_credential(&cred_path, &url, &req.username, &token).await?;
        Ok((req.username, token))
    } else {
        let resp = resp.json::<ErrorMsg>().await.map_err(RequestError::from)?;
        Err(Error::Custom(resp.msg))
    }
}

// This function is currently nowhere used, but it is kept for future potential use
pub async fn refresh_user_credential<T>(
    cred_path: Option<&T>,
    client: &Client,
    url: &mut Url,
    user_login: &UserLoginReq,
) -> crate::error::Result<String>
where
    T: GetPathBuf,
{
    url.set_path("login");
    let resp = client
        .post(url.as_str())
        .json(&user_login)
        .send()
        .await
        .map_err(|e| {
            if e.is_request() && e.is_connect() {
                url.set_path("");
                RequestError::ConnectionError(url.to_string())
            } else {
                e.into()
            }
        })?;
    if resp.status().is_success() {
        let resp = resp
            .json::<crate::schema::UserLoginResp>()
            .await
            .map_err(RequestError::from)?;
        let token = resp.token;
        if let Some(cred_path) = cred_path {
            let cred_path = cred_path.get_path_buf();
            if cred_path.exists() {
                if let Some(parent) = cred_path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                modify_or_append_credential(&cred_path, url, &user_login.username, &token).await?;
            }
        }
        Ok(token)
    } else {
        let resp = resp.json::<ErrorMsg>().await.map_err(RequestError::from)?;
        Err(Error::Custom(resp.msg))
    }
}

use std::{io::Write, path::PathBuf};

use figment::value::magic::RelativePathBuf;
use reqwest::Client;

use tokio::io::AsyncBufReadExt;
use url::Url;

use crate::{
    error::{ApiError, Error, ErrorMsg, RequestError},
    schema::UserLoginReq,
};

macro_rules! expect_two {
    ($iter:expr) => {{
        let mut i = $iter;
        match (i.next(), i.next(), i.next()) {
            (Some(first), Some(second), None) => Some((first, second)),
            _ => None,
        }
    }};
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
    user: Option<&String>,
    lines: &mut tokio::io::Lines<tokio::io::BufReader<tokio::fs::File>>,
) -> std::io::Result<Option<(String, String)>> {
    match user {
        // Specify the user, let us try to find the credential for the user
        Some(user) => {
            let prefix = format!("{user}:");
            while let Some(line) = lines.next_line().await? {
                if line.starts_with(&prefix) {
                    if let Some((username, token)) = expect_two!(line.splitn(2, ':')) {
                        return Ok(Some((username.to_owned(), token.to_owned())));
                    }
                }
            }
            Ok(None)
        }
        // No user specified, just use the first line
        None => {
            if let Some(line) = lines.next_line().await? {
                if let Some((username, token)) = expect_two!(line.splitn(2, ':')) {
                    return Ok(Some((username.to_owned(), token.to_owned())));
                }
            }
            Ok(None)
        }
    }
}

pub(crate) async fn modify_or_append_credential(
    cred_path: &std::path::PathBuf,
    username: &String,
    token: &String,
) -> std::io::Result<()> {
    if cred_path.exists() {
        let mut lines = read_lines(cred_path).await?;
        let mut new_lines = Vec::new();
        let prefix = format!("{username}:");
        let mut found = false;
        while let Some(line) = lines.next_line().await? {
            if line.starts_with(&prefix) {
                new_lines.push(format!("{username}:{token}"));
                found = true;
            } else {
                new_lines.push(line);
            }
        }
        if !found {
            new_lines.push(format!("{username}:{token}"));
        }
        tokio::fs::write(cred_path, new_lines.join("\n")).await?;
    } else {
        tokio::fs::write(cred_path, format!("{username}:{token}")).await?;
    }
    Ok(())
}

// The return value is a tuple of username and token
pub async fn get_user_credential(
    cred_path: Option<&RelativePathBuf>,
    client: &Client,
    mut url: Url,
    user: Option<&String>,
    password: Option<&String>,
) -> crate::error::Result<(String, String)> {
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
            if let Some((username, cred)) = extract_credential(user, &mut lines).await? {
                url.set_path("user/auth");
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
                    return Ok((username, cred));
                } else if resp.status().is_server_error() {
                    return Err(ApiError::InternalServerError.into());
                }
            }
        }
    }
    // Local credential not found or invalid, need to login
    tracing::warn!("Local credential not found or invalid, need to login");
    let username = user
        .map(|u| {
            println!("Username: {u}");
            Ok::<_, std::io::Error>(u.clone())
        })
        .unwrap_or_else(|| {
            let mut user = String::new();
            print!("Username: ");
            std::io::stdout().flush()?;
            std::io::stdin().read_line(&mut user)?;
            user.pop();
            Ok(user)
        })?;
    let md5_password = password
        .map(|p| Ok::<_, std::io::Error>(md5::compute(p.as_bytes()).0))
        .unwrap_or_else(|| {
            let password = rpassword::prompt_password("Password: ")?;
            Ok(md5::compute(password.as_bytes()).0)
        })?;

    let req = UserLoginReq {
        username: username.clone(),
        md5_password,
    };
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
        modify_or_append_credential(&cred_path, &username, &token).await?;
        Ok((username, token))
    } else {
        let resp = resp.json::<ErrorMsg>().await.map_err(RequestError::from)?;
        Err(Error::Custom(resp.msg))
    }
}

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
                modify_or_append_credential(&cred_path, &user_login.username, &token).await?;
            }
        }
        Ok(token)
    } else {
        let resp = resp.json::<ErrorMsg>().await.map_err(RequestError::from)?;
        Err(Error::Custom(resp.msg))
    }
}

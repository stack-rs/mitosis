use std::path::PathBuf;

use figment::value::magic::RelativePathBuf;
use reqwest::Client;

use url::Url;

use crate::{
    error::{ApiError, Error, ErrorMsg, RequestError},
    schema::UserLoginReq,
    service::auth::{credential_store::CredentialStore, fill_user_login},
};

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

// The return value is a tuple of username and token
pub async fn get_user_credential(
    cred_path: Option<&RelativePathBuf>,
    client: &Client,
    url: Url,
    user: Option<String>,
    password: Option<String>,
    refresh: bool,
) -> crate::error::Result<(String, String)> {
    let credential_store = CredentialStore::new(cred_path.map(|cred_path| cred_path.relative()))?;

    get_user_credential_with_store(&credential_store, client, url, user, password, refresh).await
}

pub(crate) async fn get_user_credential_with_store(
    credential_store: &CredentialStore,
    client: &Client,
    mut url: Url,
    user: Option<String>,
    password: Option<String>,
    refresh: bool,
) -> crate::error::Result<(String, String)> {
    if let Some((username, cred)) = credential_store.read_jwt(&url, user.as_deref()).await? {
        let request = if refresh {
            url.set_path("refresh");
            client.post(url.as_str())
        } else {
            url.set_path("auth");
            client.get(url.as_str())
        };
        let resp = request.bearer_auth(&cred).send().await.map_err(|e| {
            if e.is_request() && e.is_connect() {
                url.set_path("");
                RequestError::ConnectionError(url.to_string())
            } else {
                e.into()
            }
        })?;
        let status = resp.status();
        if status.is_success() {
            if refresh {
                let resp = resp
                    .json::<crate::schema::UserLoginResp>()
                    .await
                    .map_err(RequestError::from)?;
                let token = resp.token;
                credential_store.write_jwt(&url, &username, &token).await?;
                return Ok((username, token));
            } else {
                let resp_name = resp.text().await.map_err(RequestError::from)?;
                if resp_name == username {
                    return Ok((username, cred));
                }
            }
        } else if status.is_server_error() {
            return Err(ApiError::InternalServerError.into());
        }
    }

    tracing::warn!("Local credential not found or invalid, need to login");
    let req = fill_user_login(user, password, refresh)?;
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
        credential_store
            .write_jwt(&url, &req.username, &token)
            .await?;
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
            let credential_store = CredentialStore::new(Some(cred_path.get_path_buf()))?;
            credential_store
                .write_jwt(&*url, &user_login.username, &token)
                .await?;
        }
        Ok(token)
    } else {
        let resp = resp.json::<ErrorMsg>().await.map_err(RequestError::from)?;
        Err(Error::Custom(resp.msg))
    }
}

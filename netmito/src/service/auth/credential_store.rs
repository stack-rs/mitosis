use std::{io, path::PathBuf};

use base64::{engine::general_purpose, Engine as _};
use tokio::io::AsyncBufReadExt;
use url::Url;

use crate::error::Error;

pub(crate) struct CredentialStore {
    credential_path: PathBuf,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CredentialKind {
    Jwt,
    Login,
}

struct ParsedCredential<'a> {
    encoded_origin: &'a str,
    username: &'a str,
    value: ParsedCredentialValue<'a>,
}

enum ParsedCredentialValue<'a> {
    Jwt(&'a str),
    Login([u8; 16]),
}

impl ParsedCredentialValue<'_> {
    fn kind(&self) -> CredentialKind {
        match self {
            Self::Jwt(_) => CredentialKind::Jwt,
            Self::Login(_) => CredentialKind::Login,
        }
    }
}

enum StoredCredential {
    Jwt { username: String, token: String },
    Login([u8; 16]),
}

fn encode_origin(coordinator_url: &Url) -> String {
    general_purpose::STANDARD.encode(coordinator_url.origin().ascii_serialization())
}

fn parse_credential(line: &str) -> Option<ParsedCredential<'_>> {
    let mut fields = line.split(':');
    let (Some(encoded_origin), Some(username), Some(typed_credential), None) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return None;
    };

    let (kind, value) = typed_credential.split_once('=')?;
    let value = match kind {
        "jwt" => ParsedCredentialValue::Jwt(value),
        "login" => {
            let md5_password = general_purpose::STANDARD
                .decode(value)
                .ok()?
                .try_into()
                .ok()?;
            ParsedCredentialValue::Login(md5_password)
        }
        _ => return None,
    };

    Some(ParsedCredential {
        encoded_origin,
        username,
        value,
    })
}

impl CredentialStore {
    pub(crate) fn new(credential_path: Option<PathBuf>) -> crate::error::Result<Self> {
        let credential_path = credential_path
            .or_else(|| {
                dirs::config_dir().map(|mut path| {
                    path.push("mitosis");
                    path.push("credentials");
                    path
                })
            })
            .ok_or(Error::ConfigError(Box::new(figment::Error::from(
                "credential path not found",
            ))))?;

        Ok(Self { credential_path })
    }

    pub(crate) async fn read_jwt(
        &self,
        coordinator_url: &Url,
        username: Option<&str>,
    ) -> io::Result<Option<(String, String)>> {
        match self
            .read(coordinator_url, username, CredentialKind::Jwt)
            .await?
        {
            Some(StoredCredential::Jwt { username, token }) => Ok(Some((username, token))),
            _ => Ok(None),
        }
    }

    pub(crate) async fn write_jwt(
        &self,
        coordinator_url: &Url,
        username: &str,
        token: &str,
    ) -> io::Result<()> {
        let encoded_origin = encode_origin(coordinator_url);
        let replacement = format!("{encoded_origin}:{username}:jwt={token}");

        self.rewrite(
            &encoded_origin,
            username,
            CredentialKind::Jwt,
            Some(replacement),
        )
        .await
    }

    #[allow(dead_code)]
    pub(crate) async fn read_login(
        &self,
        coordinator_url: &Url,
        username: &str,
    ) -> io::Result<Option<[u8; 16]>> {
        match self
            .read(coordinator_url, Some(username), CredentialKind::Login)
            .await?
        {
            Some(StoredCredential::Login(md5_password)) => Ok(Some(md5_password)),
            _ => Ok(None),
        }
    }

    #[allow(dead_code)]
    pub(crate) async fn write_login(
        &self,
        coordinator_url: &Url,
        username: &str,
        md5_password: &[u8; 16],
    ) -> io::Result<()> {
        let encoded_origin = encode_origin(coordinator_url);
        let md5_password = general_purpose::STANDARD.encode(md5_password);
        let replacement = format!("{encoded_origin}:{username}:login={md5_password}");

        self.rewrite(
            &encoded_origin,
            username,
            CredentialKind::Login,
            Some(replacement),
        )
        .await
    }

    pub(crate) async fn remove_jwt(&self, coordinator_url: &Url, username: &str) -> io::Result<()> {
        let encoded_origin = encode_origin(coordinator_url);

        self.rewrite(&encoded_origin, username, CredentialKind::Jwt, None)
            .await
    }

    async fn read(
        &self,
        coordinator_url: &Url,
        requested_username: Option<&str>,
        kind: CredentialKind,
    ) -> io::Result<Option<StoredCredential>> {
        let Ok(file) = tokio::fs::File::open(&self.credential_path).await else {
            return Ok(None);
        };

        let encoded_origin = encode_origin(coordinator_url);
        let mut lines = tokio::io::BufReader::new(file).lines();

        while let Some(line) = lines.next_line().await? {
            let Some(credential) = parse_credential(&line) else {
                continue;
            };

            let ParsedCredential {
                encoded_origin: stored_origin,
                username,
                value,
            } = credential;

            if stored_origin != encoded_origin.as_str() || value.kind() != kind {
                continue;
            }

            if let Some(requested_username) = requested_username {
                if requested_username != username {
                    continue;
                }
            }

            return Ok(Some(match value {
                ParsedCredentialValue::Jwt(token) => StoredCredential::Jwt {
                    username: username.to_owned(),
                    token: token.to_owned(),
                },
                ParsedCredentialValue::Login(md5_password) => StoredCredential::Login(md5_password),
            }));
        }

        Ok(None)
    }

    async fn rewrite(
        &self,
        encoded_origin: &str,
        username: &str,
        kind: CredentialKind,
        replacement: Option<String>,
    ) -> io::Result<()> {
        if replacement.is_some() {
            if let Some(parent) = self
                .credential_path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                tokio::fs::create_dir_all(parent).await?;
            }
        }

        let file = match tokio::fs::File::open(&self.credential_path).await {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if let Some(replacement) = replacement.as_deref() {
                    tokio::fs::write(&self.credential_path, replacement).await?;
                }
                return Ok(());
            }
            Err(error) => return Err(error),
        };

        let mut lines = tokio::io::BufReader::new(file).lines();
        let mut new_lines = Vec::new();
        let mut found = false;

        while let Some(line) = lines.next_line().await? {
            let matches = match parse_credential(&line) {
                Some(credential) => {
                    credential.encoded_origin == encoded_origin
                        && credential.username == username
                        && credential.value.kind() == kind
                }
                None => false,
            };

            if matches {
                found = true;
                if let Some(replacement) = replacement.as_ref() {
                    new_lines.push(replacement.clone());
                }
            } else {
                new_lines.push(line);
            }
        }

        if !found {
            if let Some(replacement) = replacement {
                new_lines.push(replacement);
            }
        }

        if new_lines.is_empty() {
            tokio::fs::remove_file(&self.credential_path).await?;
        } else {
            tokio::fs::write(&self.credential_path, new_lines.join("\n")).await?;
        }

        Ok(())
    }
}

use std::{io, path::Path};

use base64::{engine::general_purpose, Engine as _};
use tokio::io::AsyncBufReadExt;
use url::Url;

pub(crate) enum Credential {
    Jwt {
        username: String,
        token: String,
    },
    #[allow(dead_code)]
    Login {
        username: String,
        md5_password: [u8; 16],
    },
}

pub(crate) enum CredentialRead<'a> {
    Jwt {
        username: Option<&'a str>,
    },
    #[allow(dead_code)]
    Login {
        username: &'a str,
    },
}

pub(crate) enum CredentialWrite<'a> {
    StoreJwt {
        username: &'a str,
        token: &'a str,
    },
    #[allow(dead_code)]
    StoreLogin {
        username: &'a str,
        md5_password: &'a [u8; 16],
    },
    RemoveJwt {
        username: &'a str,
    },
}

pub(crate) struct CredentialStore;

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
    pub(crate) async fn read(
        credential_path: &Path,
        coordinator_url: &Url,
        request: CredentialRead<'_>,
    ) -> io::Result<Option<Credential>> {
        let Ok(file) = tokio::fs::File::open(credential_path).await else {
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

            if stored_origin != encoded_origin.as_str() {
                continue;
            }

            match (&request, value) {
                (
                    CredentialRead::Jwt {
                        username: requested,
                    },
                    ParsedCredentialValue::Jwt(token),
                ) if match requested {
                    Some(requested) => *requested == username,
                    None => true,
                } =>
                {
                    return Ok(Some(Credential::Jwt {
                        username: username.to_owned(),
                        token: token.to_owned(),
                    }));
                }
                (
                    CredentialRead::Login {
                        username: requested,
                    },
                    ParsedCredentialValue::Login(md5_password),
                ) if *requested == username => {
                    return Ok(Some(Credential::Login {
                        username: username.to_owned(),
                        md5_password,
                    }));
                }
                _ => {}
            }
        }

        Ok(None)
    }

    pub(crate) async fn write(
        credential_path: &Path,
        coordinator_url: &Url,
        operation: CredentialWrite<'_>,
    ) -> io::Result<()> {
        let encoded_origin = encode_origin(coordinator_url);
        let (username, kind, replacement) = match operation {
            CredentialWrite::StoreJwt { username, token } => (
                username,
                CredentialKind::Jwt,
                Some(format!("{encoded_origin}:{username}:jwt={token}")),
            ),
            CredentialWrite::StoreLogin {
                username,
                md5_password,
            } => {
                let md5_password = general_purpose::STANDARD.encode(md5_password);
                (
                    username,
                    CredentialKind::Login,
                    Some(format!("{encoded_origin}:{username}:login={md5_password}")),
                )
            }
            CredentialWrite::RemoveJwt { username } => (username, CredentialKind::Jwt, None),
        };

        if replacement.is_some() {
            if let Some(parent) = credential_path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                tokio::fs::create_dir_all(parent).await?;
            }
        }

        let file = match tokio::fs::File::open(credential_path).await {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if let Some(replacement) = replacement.as_deref() {
                    tokio::fs::write(credential_path, replacement).await?;
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
                    credential.encoded_origin == encoded_origin.as_str()
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
            tokio::fs::remove_file(credential_path).await?;
        } else {
            tokio::fs::write(credential_path, new_lines.join("\n")).await?;
        }

        Ok(())
    }
}

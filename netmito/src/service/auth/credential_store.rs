use std::{io, path::PathBuf};

use tokio::io::AsyncBufReadExt;
use url::Url;

use crate::error::Error;

pub(crate) struct CredentialStore {
    credential_path: PathBuf,
}

struct ParsedCredential<'a> {
    origin: &'a str,
    username: &'a str,
    token: &'a str,
}

fn normalize_origin(coordinator_url: &Url) -> String {
    coordinator_url.origin().ascii_serialization()
}

fn parse_credential(line: &str) -> Option<ParsedCredential<'_>> {
    let mut fields = line.split(',');
    let (Some(origin), Some(username), Some(token), None) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return None;
    };

    Some(ParsedCredential {
        origin,
        username,
        token,
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
        self.read(coordinator_url, username).await
    }

    pub(crate) async fn write_jwt(
        &self,
        coordinator_url: &Url,
        username: &str,
        token: &str,
    ) -> io::Result<()> {
        let origin = normalize_origin(coordinator_url);
        let replacement = format!("{origin},{username},{token}");

        self.rewrite(&origin, username, Some(replacement)).await
    }

    pub(crate) async fn remove_jwt(&self, coordinator_url: &Url, username: &str) -> io::Result<()> {
        let origin = normalize_origin(coordinator_url);

        self.rewrite(&origin, username, None).await
    }

    async fn read(
        &self,
        coordinator_url: &Url,
        requested_username: Option<&str>,
    ) -> io::Result<Option<(String, String)>> {
        let Ok(file) = tokio::fs::File::open(&self.credential_path).await else {
            return Ok(None);
        };

        let origin = normalize_origin(coordinator_url);
        let mut lines = tokio::io::BufReader::new(file).lines();

        while let Some(line) = lines.next_line().await? {
            let Some(credential) = parse_credential(&line) else {
                continue;
            };

            let ParsedCredential {
                origin: stored_origin,
                username,
                token,
            } = credential;

            if stored_origin != origin.as_str() {
                continue;
            }

            if let Some(requested_username) = requested_username {
                if requested_username != username {
                    continue;
                }
            }

            return Ok(Some((username.to_owned(), token.to_owned())));
        }

        Ok(None)
    }

    async fn rewrite(
        &self,
        origin: &str,
        username: &str,
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
                Some(credential) => credential.origin == origin && credential.username == username,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn normalized_origin(url: &str) -> String {
        normalize_origin(&Url::parse(url).expect("test URL should be valid"))
    }

    #[test]
    fn normalize_equivalent_http_origins() {
        for url in [
            "http://127.0.0.1/",
            "http://127.0.0.1",
            "http://127.0.0.1:80",
            "http://127.0.0.1:80/",
        ] {
            assert_eq!(normalized_origin(url), "http://127.0.0.1");
        }
    }

    #[test]
    fn normalize_origin_preserves_identity_differences() {
        assert_eq!(
            normalized_origin("https://example.com:443/"),
            "https://example.com"
        );
        assert_eq!(
            normalized_origin("http://example.com:5000/"),
            "http://example.com:5000"
        );
        assert_ne!(
            normalized_origin("http://example.com"),
            normalized_origin("https://example.com")
        );
        assert_ne!(
            normalized_origin("http://example.com"),
            normalized_origin("http://example.org")
        );
        assert_eq!(
            normalized_origin("http://example.com/path?q=1#section"),
            "http://example.com"
        );
        assert_ne!(
            normalized_origin("http://example.com:5000"),
            normalized_origin("http://example.com:5001")
        );
        assert_ne!(
            normalized_origin("http://localhost"),
            normalized_origin("http://127.0.0.1")
        );
    }

    #[test]
    fn parse_comma_separated_credential() {
        let credential = parse_credential("http://127.0.0.1,user_a,encoded-token==")
            .expect("credential should be valid");

        assert_eq!(credential.origin, "http://127.0.0.1");
        assert_eq!(credential.username, "user_a");
        assert_eq!(credential.token, "encoded-token==");

        assert!(parse_credential("user_a:legacy-token").is_none());
        assert!(parse_credential("http://127.0.0.1,user_a").is_none());
        assert!(parse_credential("http://127.0.0.1,user_a,token,extra").is_none());
    }
}

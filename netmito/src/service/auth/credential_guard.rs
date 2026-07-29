use std::path::PathBuf;

use tokio::{fs, io};
use url::Url;

pub(crate) struct CredentialGuard {
    /// `None` when no location could be resolved at all, in which case credentials
    /// are only stored in-memory
    credential_path: Option<PathBuf>,
    origin: String,
    username: Option<String>,
    credential: Option<String>,
}

#[derive(Clone)]
pub(crate) struct ParsedCredential {
    pub(crate) origin: String,
    pub(crate) username: String,
    pub(crate) token: String,
}

fn normalize_origin(coordinator_url: &Url) -> String {
    coordinator_url.origin().ascii_serialization()
}

fn parse_credential(line: &str) -> Option<ParsedCredential> {
    let mut fields = line.split(',');
    let (Some(origin), Some(username), Some(token), None) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return None;
    };

    Some(ParsedCredential {
        origin: origin.to_string(),
        username: username.to_string(),
        token: token.to_string(),
    })
}

impl CredentialGuard {
    /// The caller creates the instance. We try to resolve the credential path.
    ///
    /// If the file isn't fully read/write-able, we warn it, but we will still record user's token
    /// in-memory in self.active_credential.
    ///
    /// Upon new(), we will immediately try to parse the file and try to load the credential into
    /// self.active_credential. If the credential is not found, the active_credential just stays
    /// `None`.
    pub(crate) async fn new(credential_path: Option<PathBuf>, coordinator_url: &Url) -> Self {
        let credential_path = credential_path.or_else(|| {
            dirs::config_dir().map(|mut path| {
                path.push("mitosis");
                path.push("credentials");
                path
            })
        });

        let mut credential_guard = Self {
            credential_path,
            origin: normalize_origin(coordinator_url),
            username: None,
            credential: None,
        };

        credential_guard.credential = credential_guard.load_credential_file().await;

        credential_guard
    }

    /// The caller wants to access the credential for current username
    pub(crate) fn get_credential(&self) -> Option<ParsedCredential> {
        match (&self.username, &self.credential) {
            (Some(name), Some(cred)) => Some(ParsedCredential {
                origin: self.origin.clone(),
                username: name.clone(),
                token: cred.clone(),
            }),
            _ => None,
        }
    }

    /// The caller wants to switch to this username. The returned value is
    /// the stored credential for the new user.
    pub(crate) async fn load_credential(&mut self, username: String) -> Option<ParsedCredential> {
        // Reloading for the active user would drop a credential that we failed to persist.
        match &self.username {
            Some(name) if *name == username => {}
            _ => {
                self.username = Some(username);
                self.credential = self.load_credential_file().await;
            }
        }

        self.get_credential()
    }

    /// The caller wants to update the active credential for current user
    pub(crate) async fn save_credential(&mut self, username: Option<String>, cred: &str) {
        if let Err(error) = self.update_credential_file(username, Some(cred)).await {
            tracing::warn!("Failed to save the credential: {error}.");
        }

        self.credential = Some(cred.to_string());
    }

    /// The caller wants to drop the credential of the current user. It is forgotten in memory
    /// whether or not the credential file could be updated.
    pub(crate) async fn remove_credential(&mut self, username: Option<String>) {
        if let Err(error) = self.update_credential_file(username, None).await {
            tracing::warn!("Failed to delete the credential: {error}.");
        }

        self.credential = None;
    }

    /// Set the credential of the active user in the credential file to `token`, or drop it when
    /// there is no token. The file and its directory are created when a credential is written; a
    /// credential that is not stored in the first place, like a store with no file at all, needs
    /// no rewrite.
    async fn update_credential_file(
        &self,
        username: Option<String>,
        token: Option<&str>,
    ) -> io::Result<()> {
        let username = match username.as_ref().or(self.username.as_ref()) {
            Some(name) => name,
            None => {
                tracing::error!("Username not set for manipulating credential file");
                return Ok(());
            }
        };
        let Some(credential_path) = self.credential_path.as_deref() else {
            return Ok(());
        };

        let mut lines: Vec<String> = match fs::read_to_string(credential_path).await {
            Ok(contents) => contents.lines().map(str::to_string).collect(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error),
        };

        let stored = lines.iter().position(|line| {
            parse_credential(line)
                .is_some_and(|stored| stored.origin == self.origin && stored.username == *username)
        });

        match (stored, token) {
            (Some(index), Some(token)) => {
                lines[index] = format!("{},{},{}", self.origin, username, token)
            }
            (Some(index), None) => {
                lines.remove(index);
            }
            (None, Some(token)) => lines.push(format!("{},{},{}", self.origin, username, token)),
            // There is no such credential to remove, so the file needs no rewrite.
            (None, None) => return Ok(()),
        }

        if let Some(parent) = credential_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).await?;
        }

        let mut contents = lines.join("\n");
        if !contents.is_empty() {
            contents.push('\n');
        }

        fs::write(credential_path, contents).await
    }

    /// Look up the stored credential of the active user. A file we cannot read is reported and
    /// then treated as if it held no matching credential.
    async fn load_credential_file(&self) -> Option<String> {
        let username = match &self.username {
            Some(name) => name,
            None => {
                tracing::error!("Username not set for loading credential");
                return None;
            }
        };
        let credential_path = self.credential_path.as_deref()?;

        let contents = match fs::read_to_string(credential_path).await {
            Ok(contents) => contents,
            Err(error) => {
                if error.kind() != io::ErrorKind::NotFound {
                    tracing::warn!(
                        "Failed to read credential file {}: {error}",
                        credential_path.display()
                    );
                }
                return None;
            }
        };

        contents
            .lines()
            .filter_map(parse_credential)
            .find(|credential| credential.origin == self.origin && credential.username == *username)
            .map(|cred| cred.token)
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

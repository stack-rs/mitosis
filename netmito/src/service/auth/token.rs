use std::borrow::Cow;

use base64::{engine::general_purpose, Engine as _};
use jsonwebtoken::EncodingKey;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::{
    error::{ApiError, DecodeTokenError},
    schema::WorkerTokenLifetime,
};

#[derive(Debug, Serialize, Deserialize)]
pub struct TokenClaims<'a> {
    /// username
    pub sub: Cow<'a, str>,
    /// expiry time
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "jwt_numeric_date_opt"
    )]
    pub exp: Option<OffsetDateTime>,
    /// random number
    pub sign: i64,
}

pub fn generate_token<T>(username: T, sign: i64) -> crate::error::Result<String>
where
    T: AsRef<str>,
{
    let token_ttl = crate::config::SERVER_CONFIG
        .get()
        .ok_or(crate::error::Error::Custom(
            "server config not found".to_string(),
        ))?;
    let claims = TokenClaims {
        sub: Cow::from(username.as_ref()),
        exp: Some(OffsetDateTime::now_utc() + token_ttl.token_expires_in),
        sign,
    };

    let encoding_key = crate::config::ENCODING_KEY
        .get()
        .ok_or(crate::error::Error::Custom(
            "encoding key not found".to_string(),
        ))?;
    encode_token(&claims, encoding_key)
}

pub fn generate_worker_token<T>(
    username: T,
    sign: i64,
    lifetime: WorkerTokenLifetime,
) -> crate::error::Result<String>
where
    T: AsRef<str>,
{
    let exp = match lifetime {
        WorkerTokenLifetime::Default => {
            let token_ttl = crate::config::SERVER_CONFIG
                .get()
                .ok_or(crate::error::Error::Custom(
                    "server config not found".to_string(),
                ))?
                .token_expires_in;
            Some(OffsetDateTime::now_utc() + token_ttl)
        }
        WorkerTokenLifetime::Duration(ttl) => {
            let token_ttl = time::Duration::try_from(ttl).map_err(|_| {
                ApiError::InvalidRequest(format!(
                    "Invalid lifetime {}",
                    humantime_serde::re::humantime::format_duration(ttl)
                ))
            })?;
            Some(OffsetDateTime::now_utc() + token_ttl)
        }
        WorkerTokenLifetime::Never => None,
    };
    let claims = TokenClaims {
        sub: Cow::from(username.as_ref()),
        exp,
        sign,
    };

    let encoding_key = crate::config::ENCODING_KEY
        .get()
        .ok_or(crate::error::Error::Custom(
            "encoding key not found".to_string(),
        ))?;
    encode_token(&claims, encoding_key)
}

pub fn encode_token(
    claims: &TokenClaims,
    encoding_key: &EncodingKey,
) -> crate::error::Result<String> {
    let header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::EdDSA);
    let token = jsonwebtoken::encode(&header, claims, encoding_key)?;
    let token_base64 = general_purpose::STANDARD.encode(token);
    Ok(token_base64)
}

pub fn verify_token(token: &str) -> crate::error::Result<TokenClaims<'_>> {
    let token = general_purpose::STANDARD
        .decode(token)
        .map_err(DecodeTokenError::from)?;
    let token = String::from_utf8(token).map_err(DecodeTokenError::from)?;
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::EdDSA);
    validation.required_spec_claims.remove("exp");
    let decoding_key = crate::config::DECODING_KEY
        .get()
        .ok_or(crate::error::Error::Custom(
            "decoding key not found".to_string(),
        ))?;
    let decoded = jsonwebtoken::decode::<TokenClaims>(&token, decoding_key, &validation)
        .map_err(DecodeTokenError::from)?;
    Ok(decoded.claims)
}

mod jwt_numeric_date_opt {
    use serde::{self, Deserialize, Deserializer, Serializer};
    use time::OffsetDateTime;

    pub fn serialize<S>(date: &Option<OffsetDateTime>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match date {
            Some(date) => serializer.serialize_some(&date.unix_timestamp()),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<OffsetDateTime>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let timestamp = Option::<i64>::deserialize(deserializer)?;
        timestamp
            .map(|timestamp| {
                OffsetDateTime::from_unix_timestamp(timestamp)
                    .map_err(|_| serde::de::Error::custom("invalid Unix timestamp value"))
            })
            .transpose()
    }
}

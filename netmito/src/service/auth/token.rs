use std::borrow::Cow;

use base64::{engine::general_purpose, Engine as _};
use jsonwebtoken::EncodingKey;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::DecodeTokenError;

#[derive(Debug, Serialize, Deserialize)]
pub struct TokenClaims<'a> {
    /// username
    pub sub: Cow<'a, str>,
    /// expiry time
    #[serde(with = "jwt_numeric_date")]
    pub exp: OffsetDateTime,
    /// random number
    pub sign: i64,
}

pub fn generate_token(username: &String, sign: i64) -> crate::error::Result<String> {
    let token_ttl = crate::config::SERVER_CONFIG
        .get()
        .ok_or(crate::error::Error::Custom(
            "server config not found".to_string(),
        ))?;
    let claims = TokenClaims {
        sub: Cow::from(username),
        exp: OffsetDateTime::now_utc() + token_ttl.token_expires_in,
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

pub fn verify_token(token: &str) -> crate::error::Result<TokenClaims> {
    let token = general_purpose::STANDARD
        .decode(token)
        .map_err(DecodeTokenError::from)?;
    let token = String::from_utf8(token).map_err(DecodeTokenError::from)?;
    let validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::EdDSA);
    let decoding_key = crate::config::DECODING_KEY
        .get()
        .ok_or(crate::error::Error::Custom(
            "decoding key not found".to_string(),
        ))?;
    let decoded = jsonwebtoken::decode::<TokenClaims>(&token, decoding_key, &validation)
        .map_err(DecodeTokenError::from)?;
    Ok(decoded.claims)
}

mod jwt_numeric_date {
    use serde::{self, Deserialize, Deserializer, Serializer};
    use time::OffsetDateTime;
    /// Serializes an OffsetDateTime to a Unix timestamp (milliseconds since 1970/1/1T00:00:00T)
    pub fn serialize<S>(date: &OffsetDateTime, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let timestamp = date.unix_timestamp();
        serializer.serialize_i64(timestamp)
    }

    /// Attempts to deserialize an i64 and use as a Unix timestamp
    pub fn deserialize<'de, D>(deserializer: D) -> Result<OffsetDateTime, D::Error>
    where
        D: Deserializer<'de>,
    {
        OffsetDateTime::from_unix_timestamp(i64::deserialize(deserializer)?)
            .map_err(|_| serde::de::Error::custom("invalid Unix timestamp value"))
    }
}

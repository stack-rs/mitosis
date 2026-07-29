use std::borrow::Cow;

use base64::{engine::general_purpose, Engine as _};
use jsonwebtoken::EncodingKey;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::{ApiError, DecodeTokenError};

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

/// Generate a worker token expiring after `lifetime`, or one that never expires if it is `None`.
pub fn generate_worker_token<T>(
    username: T,
    sign: i64,
    lifetime: Option<std::time::Duration>,
) -> crate::error::Result<String>
where
    T: AsRef<str>,
{
    let exp = lifetime
        .map(|ttl| {
            let token_ttl = time::Duration::try_from(ttl).map_err(|_| {
                ApiError::InvalidRequest(format!(
                    "Invalid lifetime {}",
                    humantime_serde::re::humantime::format_duration(ttl)
                ))
            })?;
            Ok::<_, ApiError>(OffsetDateTime::now_utc() + token_ttl)
        })
        .transpose()?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Validation};

    // Ed25519 test keypair, local to this module only. Never touches
    // `crate::config::ENCODING_KEY`/`DECODING_KEY` (process-global `OnceCell`s) so these
    // tests carry no shared state and can't affect or be affected by other tests.
    const TEST_PRIVATE_KEY_PEM: &[u8] = b"-----BEGIN PRIVATE KEY-----\n\
MC4CAQAwBQYDK2VwBCIEIE4+s/pbE45hkcW3aMQOwcwQdGsE8fWZ6Zr8PbKHYtHa\n\
-----END PRIVATE KEY-----\n";
    const TEST_PUBLIC_KEY_PEM: &[u8] = b"-----BEGIN PUBLIC KEY-----\n\
MCowBQYDK2VwAyEAa+75592cq25dhxP5a9zZMvLn+7yXyq6cUj2xmjB2PW4=\n\
-----END PUBLIC KEY-----\n";

    fn test_encoding_key() -> EncodingKey {
        EncodingKey::from_ed_pem(TEST_PRIVATE_KEY_PEM).expect("valid test private key")
    }

    fn test_decoding_key() -> DecodingKey {
        DecodingKey::from_ed_pem(TEST_PUBLIC_KEY_PEM).expect("valid test public key")
    }

    fn encode_test_claims(exp: Option<OffsetDateTime>) -> String {
        let claims = TokenClaims {
            sub: Cow::from("test_user"),
            exp,
            sign: 42,
        };
        encode_token(&claims, &test_encoding_key()).expect("token should encode")
    }

    // Learning test helper: mirrors `verify_token`'s decode/validation logic exactly
    // (same base64 unwrap + same `Validation` setup: EdDSA, "exp" removed from
    // `required_spec_claims`), but takes the decoding key as a parameter instead of
    // reading the global `crate::config::DECODING_KEY`. Keep in sync with `verify_token`
    // above if its validation setup ever changes.
    fn decode_test_claims<'a>(
        token: &'a str,
        decoding_key: &DecodingKey,
    ) -> crate::error::Result<TokenClaims<'a>> {
        let token = general_purpose::STANDARD
            .decode(token)
            .map_err(DecodeTokenError::from)?;
        let token = String::from_utf8(token).map_err(DecodeTokenError::from)?;
        let mut validation = Validation::new(Algorithm::EdDSA);
        validation.required_spec_claims.remove("exp");
        let decoded = jsonwebtoken::decode::<TokenClaims>(&token, decoding_key, &validation)
            .map_err(DecodeTokenError::from)?;
        Ok(decoded.claims)
    }

    #[test]
    fn verify_token_accepts_missing_exp() {
        let token = encode_test_claims(None);

        let claims = decode_test_claims(&token, &test_decoding_key())
            .expect("token without exp should verify");

        assert_eq!(claims.sub, "test_user");
        assert_eq!(claims.sign, 42);
        assert!(claims.exp.is_none());
    }

    #[test]
    fn verify_token_accepts_valid_future_exp() {
        let exp = OffsetDateTime::now_utc() + time::Duration::hours(1);
        let token = encode_test_claims(Some(exp));

        let claims = decode_test_claims(&token, &test_decoding_key())
            .expect("token with future exp should verify");

        assert!(claims.exp.is_some());
        assert_eq!(claims.exp.unwrap().unix_timestamp(), exp.unix_timestamp());
    }

    #[test]
    fn verify_token_rejects_expired_exp() {
        let exp = OffsetDateTime::now_utc() - time::Duration::hours(1);
        let token = encode_test_claims(Some(exp));

        let result = decode_test_claims(&token, &test_decoding_key());

        assert!(
            result.is_err(),
            "expired token should be rejected, got {result:?}"
        );
    }
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
        Option::<i64>::deserialize(deserializer)?
            .map(|timestamp| {
                OffsetDateTime::from_unix_timestamp(timestamp)
                    .map_err(|_| serde::de::Error::custom("invalid Unix timestamp value"))
            })
            .transpose()
    }
}

use axum::http::{HeaderMap, HeaderValue};
use sha2::{Digest, Sha256};

use crate::error::HttpError;

pub const INGEST_TOKEN_HEADER: &str = "x-eal-ingest-token";
const INGEST_TOKEN_DIGEST_ENV: &str = "EAL_INGEST_TOKEN_SHA256";
const MIN_TOKEN_BYTES: usize = 32;
const MAX_TOKEN_BYTES: usize = 512;

pub fn is_configured() -> bool {
    configured_digest().is_ok()
}

pub fn authorize(headers: &HeaderMap) -> Result<(), HttpError> {
    let expected = configured_digest().map_err(|_| HttpError::ingest_auth_required())?;
    let token = headers
        .get(INGEST_TOKEN_HEADER)
        .ok_or_else(invalid_credentials)?;
    if !token_matches(&expected, token) {
        return Err(invalid_credentials());
    }
    Ok(())
}

fn configured_digest() -> Result<[u8; 32], ()> {
    let value = std::env::var(INGEST_TOKEN_DIGEST_ENV).map_err(|_| ())?;
    parse_configured_digest(&value).map_err(|_| ())
}

fn parse_configured_digest(value: &str) -> Result<[u8; 32], &'static str> {
    let value = value.trim();
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("ingest token digest must contain exactly 64 hexadecimal characters");
    }
    let mut digest = [0_u8; 32];
    hex::decode_to_slice(value, &mut digest)
        .map_err(|_| "ingest token digest must be valid hexadecimal")?;
    Ok(digest)
}

fn token_matches(expected: &[u8; 32], token: &HeaderValue) -> bool {
    let token = token.as_bytes();
    if !(MIN_TOKEN_BYTES..=MAX_TOKEN_BYTES).contains(&token.len()) {
        return false;
    }
    let actual: [u8; 32] = Sha256::digest(token).into();
    fixed_time_eq(expected, &actual)
}

fn fixed_time_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn invalid_credentials() -> HttpError {
    HttpError::unauthorized("invalid crawler credentials")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_configuration_is_exact_and_bounded() {
        assert!(parse_configured_digest(&"ab".repeat(32)).is_ok());
        assert!(parse_configured_digest("ab").is_err());
        assert!(parse_configured_digest(&"xz".repeat(32)).is_err());
    }

    #[test]
    fn token_verification_hashes_the_secret_and_rejects_short_values() {
        let token = HeaderValue::from_static("0123456789abcdef0123456789abcdef");
        let expected: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        assert!(token_matches(&expected, &token));
        assert!(!token_matches(
            &expected,
            &HeaderValue::from_static("too-short")
        ));
        assert!(!token_matches(
            &expected,
            &HeaderValue::from_static("0123456789abcdef0123456789abcdeg")
        ));
    }

    #[test]
    fn fixed_length_digest_comparison_detects_any_difference() {
        let left = [7_u8; 32];
        let mut right = left;
        assert!(fixed_time_eq(&left, &right));
        right[31] ^= 1;
        assert!(!fixed_time_eq(&left, &right));
    }
}

// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

use crate::config::{ApiKeyEntry, Config, Role};
use axum::http::request::Parts;
use chrono::Utc;
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub role: Role,
    pub namespace: String,
    pub exp: usize,
    pub iat: usize,
    #[serde(default)]
    pub iss: Option<String>,
    #[serde(default)]
    pub aud: Option<String>,
}

/// Derive an opaque key ID from a raw API key: SHA-256 truncated to 16 hex chars.
/// This is used everywhere instead of the raw key to prevent secret leakage.
pub fn key_id(raw_key: &str) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(raw_key.as_bytes());
    hex::encode(&hash[..8])
}

pub fn create_jwt(config: &Config, api_key_entry: &ApiKeyEntry) -> anyhow::Result<String> {
    let now = Utc::now().timestamp() as usize;
    let claims = Claims {
        sub: key_id(&api_key_entry.key),
        role: api_key_entry.role,
        namespace: api_key_entry.namespace.clone(),
        exp: now + config.auth.jwt_expiry_secs as usize,
        iat: now,
        iss: Some("memoryoss".to_string()),
        aud: Some("memoryoss".to_string()),
    };
    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(config.auth.jwt_secret.as_bytes()),
    )?;
    Ok(token)
}

pub fn validate_jwt(config: &Config, token: &str) -> anyhow::Result<Claims> {
    let mut validation = Validation::new(jsonwebtoken::Algorithm::HS256);
    validation.validate_exp = true;
    validation.leeway = 0;
    validation.set_required_spec_claims(&["exp", "iss", "aud"]);
    validation.set_issuer(&["memoryoss"]);
    validation.set_audience(&["memoryoss"]);
    let token_data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(config.auth.jwt_secret.as_bytes()),
        &validation,
    )?;
    Ok(token_data.claims)
}

/// Constant-time API key comparison to prevent timing attacks.
/// A7 FIX: iterate ALL keys to prevent position-based timing leak.
pub fn find_api_key<'a>(config: &'a Config, key: &str) -> Option<&'a ApiKeyEntry> {
    use sha2::{Digest, Sha256};
    let key_hash = Sha256::digest(key.as_bytes());
    let mut result: Option<&ApiKeyEntry> = None;
    for e in &config.auth.api_keys {
        let entry_hash = Sha256::digest(e.key.as_bytes());
        let eq = entry_hash
            .as_slice()
            .iter()
            .zip(key_hash.as_slice().iter())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0;
        if eq {
            result = Some(e);
        }
    }
    result
}

pub fn extract_bearer_token(parts: &Parts) -> Option<&str> {
    parts
        .headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

#[cfg(test)]
mod tests {
    use super::{Claims, create_jwt, validate_jwt};
    use crate::config::{ApiKeyEntry, Config, Role};

    fn test_config() -> Config {
        let mut cfg = Config::default();
        cfg.auth.jwt_secret = "test-secret-that-is-at-least-32-characters-long".to_string();
        cfg
    }

    #[test]
    fn create_and_validate_jwt_sets_audience() {
        let cfg = test_config();
        let entry = ApiKeyEntry {
            key: "ek_test".to_string(),
            role: Role::Admin,
            namespace: "default".to_string(),
        };
        let token = create_jwt(&cfg, &entry).expect("token");
        let claims = validate_jwt(&cfg, &token).expect("claims");
        assert_eq!(claims.aud.as_deref(), Some("memoryoss"));
    }

    #[test]
    fn validate_jwt_rejects_missing_audience() {
        let cfg = test_config();
        let now = chrono::Utc::now().timestamp() as usize;
        let claims = Claims {
            sub: "sub".to_string(),
            role: Role::Admin,
            namespace: "default".to_string(),
            exp: now + 3600,
            iat: now,
            iss: Some("memoryoss".to_string()),
            aud: None,
        };
        let token = jsonwebtoken::encode(
            &jsonwebtoken::Header::default(),
            &claims,
            &jsonwebtoken::EncodingKey::from_secret(cfg.auth.jwt_secret.as_bytes()),
        )
        .expect("encode");
        assert!(validate_jwt(&cfg, &token).is_err());
    }
}

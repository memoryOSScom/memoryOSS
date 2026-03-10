// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 memoryOSS Contributors

use crate::config::LimitsConfig;

#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("content is empty")]
    EmptyContent,
    #[error("content exceeds maximum size ({size} > {max} bytes)")]
    ContentTooLarge { size: usize, max: usize },
    #[error("too many tags ({count} > {max})")]
    TooManyTags { count: usize, max: usize },
    #[error("tag too long ({length} > {max} bytes)")]
    TagTooLong { length: usize, max: usize },
    #[error("tag is empty")]
    EmptyTag,
    #[error("namespace too long ({length} > {max} bytes)")]
    NamespaceTooLong { length: usize, max: usize },
    #[error("namespace contains invalid characters (must be alphanumeric, -, _)")]
    InvalidNamespace,
    #[error("query is empty")]
    EmptyQuery,
}

pub fn validate_content(content: &str, limits: &LimitsConfig) -> Result<(), ValidationError> {
    if content.trim().is_empty() {
        return Err(ValidationError::EmptyContent);
    }
    if content.len() > limits.max_content_bytes {
        return Err(ValidationError::ContentTooLarge {
            size: content.len(),
            max: limits.max_content_bytes,
        });
    }
    Ok(())
}

pub fn validate_tags(tags: &[String], limits: &LimitsConfig) -> Result<(), ValidationError> {
    if tags.len() > limits.max_tags {
        return Err(ValidationError::TooManyTags {
            count: tags.len(),
            max: limits.max_tags,
        });
    }
    for tag in tags {
        if tag.is_empty() {
            return Err(ValidationError::EmptyTag);
        }
        if tag.len() > limits.max_tag_length {
            return Err(ValidationError::TagTooLong {
                length: tag.len(),
                max: limits.max_tag_length,
            });
        }
    }
    Ok(())
}

pub fn validate_namespace(ns: &str, limits: &LimitsConfig) -> Result<(), ValidationError> {
    if ns.len() > limits.max_namespace_length {
        return Err(ValidationError::NamespaceTooLong {
            length: ns.len(),
            max: limits.max_namespace_length,
        });
    }
    if !ns
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err(ValidationError::InvalidNamespace);
    }
    Ok(())
}

pub fn validate_query(query: &str, limits: &LimitsConfig) -> Result<(), ValidationError> {
    if query.trim().is_empty() {
        return Err(ValidationError::EmptyQuery);
    }
    if query.len() > limits.max_content_bytes {
        return Err(ValidationError::ContentTooLarge {
            size: query.len(),
            max: limits.max_content_bytes,
        });
    }
    Ok(())
}

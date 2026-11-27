//! Structural metadata validation for the pure computation layer.
use respire_protocol::{Evidence, ExtraFields, MemoryMeta, RequestScope, Scope, Verification};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetadataError {
    pub code: &'static str,
    pub message: &'static str,
}

impl std::fmt::Display for MetadataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for MetadataError {}

fn invalid(message: &'static str) -> MetadataError {
    MetadataError {
        code: "invalid_metadata",
        message,
    }
}

pub fn validate_extra_fields(fields: &ExtraFields) -> Result<(), MetadataError> {
    if fields.is_empty() {
        Ok(())
    } else {
        Err(MetadataError {
            code: "unsupported_metadata",
            message: "unknown metadata fields",
        })
    }
}

pub fn validate_revision(value: &str) -> Result<(), MetadataError> {
    match value.strip_prefix("sha256:") {
        Some(hex)
            if hex.len() == 64
                && hex
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) =>
        {
            Ok(())
        }
        _ => Err(invalid(
            "revision must be sha256 followed by 64 lowercase hexadecimal digits",
        )),
    }
}

pub fn validate_full_id(value: &str) -> Result<(), MetadataError> {
    let valid = value.len() == 36
        && value.bytes().enumerate().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        });
    if valid {
        Ok(())
    } else {
        Err(invalid("source ID must be a complete UUID"))
    }
}

pub fn validate_timestamp(value: &str) -> Result<(), MetadataError> {
    let b = value.as_bytes();
    if b.len() != 24
        || !b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            10 => *c == b'T',
            13 | 16 => *c == b':',
            19 => *c == b'.',
            23 => *c == b'Z',
            _ => c.is_ascii_digit(),
        })
    {
        return Err(invalid("time must be YYYY-MM-DDTHH:MM:SS.SSSZ"));
    }
    let number = |start: usize, end: usize| {
        b[start..end]
            .iter()
            .fold(0u32, |n, c| n * 10 + u32::from(*c - b'0'))
    };
    let year = number(0, 4);
    let month = number(5, 7);
    let day = number(8, 10);
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        _ => 0,
    };
    if day == 0 || day > days || number(11, 13) > 23 || number(14, 16) > 59 || number(17, 19) > 59 {
        return Err(invalid("invalid UTC calendar date or time"));
    }
    Ok(())
}

pub fn validate_scope(value: &Scope) -> Result<(), MetadataError> {
    validate_extra_fields(&value.unknown_fields)?;
    for value in [&value.from, &value.until].into_iter().flatten() {
        if value != "*" {
            validate_timestamp(value)?;
        }
    }
    if let (Some(from), Some(until)) = (&value.from, &value.until) {
        if from != "*" && until != "*" && from >= until {
            return Err(invalid("scope requires from < until"));
        }
    }
    Ok(())
}

pub fn validate_request_scope(value: &RequestScope) -> Result<(), MetadataError> {
    validate_extra_fields(&value.unknown_fields)?;
    if [&value.project, &value.device, &value.version]
        .into_iter()
        .flatten()
        .any(|v| v == "*")
    {
        return Err(invalid("request scope cannot contain a wildcard"));
    }
    validate_timestamp(&value.at)
}

pub fn validate_evidence(value: &Evidence) -> Result<(), MetadataError> {
    let (locator, revision, fields) = match value {
        Evidence::Memory {
            id,
            revision,
            unknown_fields,
        } => {
            validate_full_id(id)?;
            (id, revision, unknown_fields)
        }
        Evidence::User {
            locator,
            revision,
            unknown_fields,
        }
        | Evidence::Primary {
            locator,
            revision,
            unknown_fields,
        }
        | Evidence::Run {
            locator,
            revision,
            unknown_fields,
        } => (locator, revision, unknown_fields),
    };
    validate_extra_fields(fields)?;
    if locator.trim().is_empty() {
        return Err(invalid("evidence locator cannot be empty"));
    }
    validate_revision(revision)
}

/// Structural validation only; confirmed_by is not proof of authority.
pub fn validate_verification(value: &Verification) -> Result<(), MetadataError> {
    validate_extra_fields(&value.unknown_fields)?;
    validate_extra_fields(&value.confirmed_by.unknown_fields)?;
    validate_revision(&value.record_revision)?;
    validate_timestamp(&value.confirmed_at)?;
    validate_scope(&value.scope)?;
    if value.claim.trim().is_empty()
        || value.method.trim().is_empty()
        || value.evidence.is_empty()
        || value.confirmed_by.id.trim().is_empty()
        || value.confirmed_by.event_id.trim().is_empty()
    {
        return Err(invalid(
            "verification requires claim, method, evidence and confirmation identity",
        ));
    }
    let mut unique = BTreeSet::new();
    for evidence in &value.evidence {
        validate_evidence(evidence)?;
        let encoded = serde_json::to_string(evidence).map_err(|_| invalid("invalid evidence"))?;
        if !unique.insert(encoded) {
            return Err(invalid("duplicate evidence"));
        }
    }
    Ok(())
}

pub fn validate_metadata(value: &MemoryMeta) -> Result<(), MetadataError> {
    match value.memory_contract {
        0 if value == &MemoryMeta::default() => return Ok(()),
        0 => return Err(invalid("contract 0 cannot contain metadata")),
        1 => {}
        _ => {
            return Err(MetadataError {
                code: "unsupported_metadata",
                message: "unknown memory contract",
            })
        }
    }
    if value
        .summary
        .as_ref()
        .is_some_and(|s| s.chars().count() > 120)
    {
        return Err(invalid("summary exceeds 120 Unicode scalars"));
    }
    let mut ids = BTreeSet::new();
    for source in &value.source_refs {
        validate_extra_fields(&source.unknown_fields)?;
        validate_full_id(&source.id)?;
        validate_revision(&source.revision)?;
        if !ids.insert(&source.id) {
            return Err(invalid("duplicate source ID"));
        }
    }
    for value in [&value.valid_until, &value.review_at].into_iter().flatten() {
        validate_timestamp(value)?;
    }
    if let Some(artifact) = &value.artifact {
        validate_extra_fields(&artifact.unknown_fields)?;
    }
    if let Some(verification) = &value.verification {
        validate_verification(verification)?;
    }
    Ok(())
}

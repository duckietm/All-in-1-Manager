//! The official signed plugin index: `index.json` (schema 1), its Ed25519 signature `index.json.sig` and the
//! freshness rules (rollback and expiry). The Manager installs a plugin from the index only when [`verify`] and
//! [`Index::parse`] pass and [`check_fresh`] says [`Freshness::Current`]; use [`verify_and_parse`] for the first two.

#![forbid(unsafe_code)]

pub mod schema;
pub mod sign;

pub use schema::{check_fresh, Entry, Freshness, Index, Revoked, Source, MAX_INDEX_BYTES, MAX_PACKAGE_BYTES, MAX_PLUGINS, SCHEMA};
pub use sign::{key_id, sign, verify, KeyId};

use anyhow::Result;

/// Verifies the signature of `index_bytes` with one of the `trusted` keys ([`verify`]), then parses the very same
/// bytes ([`Index::parse`]). Callers should use this instead of calling `verify` and `Index::parse` separately, so
/// the bytes that were verified are always the bytes that were parsed (never a second read or a re-serialisation).
pub fn verify_and_parse(index_bytes: &[u8], sig_text: &str, trusted: &[(&str, &str)], url_prefix: &str) -> Result<(Index, KeyId)> {
    let key = verify(index_bytes, sig_text, trusted)?;
    let index = Index::parse(index_bytes, url_prefix)?;
    Ok((index, key))
}

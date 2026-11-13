//! The official signed plugin index: `index.json` (schema 1), its Ed25519 signature `index.json.sig` and the
//! freshness rules (rollback and expiry). The Manager installs a plugin from the index only when [`verify`] and
//! [`Index::parse`] pass and [`check_fresh`] says [`Freshness::Current`].

#![forbid(unsafe_code)]

pub mod schema;
pub mod sign;

pub use schema::{check_fresh, Entry, Freshness, Index, Revoked, Source, MAX_INDEX_BYTES, MAX_PACKAGE_BYTES, MAX_PLUGINS, SCHEMA};
pub use sign::{key_id, sign, verify, KeyId};

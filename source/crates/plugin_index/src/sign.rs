//! Ed25519 signature of `index.json` and the `index.json.sig` text format.
//!
//! The `.sig` file is exactly two lines, each ending in `\n`:
//! ```text
//! <standard base64, padded, of the 64-byte signature>
//! key: <key id>
//! ```
//! The key id is the first 16 lowercase hex characters of SHA-256 over the 32 raw public-key bytes.

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};
use std::fmt;

/// Length of a key id in hex characters.
const KEY_ID_LEN: usize = 16;

/// A key id: the first 16 lowercase hex characters of SHA-256 of an Ed25519 public key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeyId(String);

impl KeyId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for KeyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The key id of a 32-byte Ed25519 public key.
pub fn key_id(public: &[u8; 32]) -> KeyId {
    let digest = Sha256::digest(public);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    KeyId(hex[..KEY_ID_LEN].to_string())
}

/// Signs the exact bytes of `index.json` with a 32-byte Ed25519 secret key; returns the `.sig` file text.
pub fn sign(index_bytes: &[u8], secret: &[u8; 32]) -> String {
    let key = SigningKey::from_bytes(secret);
    let signature = key.sign(index_bytes);
    let id = key_id(&key.verifying_key().to_bytes());
    format!("{}\nkey: {id}\n", STANDARD.encode(signature.to_bytes()))
}

/// Splits a `.sig` text into the signature and the key id; refuses any other shape.
fn parse_sig(sig_text: &str) -> Result<(Signature, &str)> {
    let shape = || "The index signature file is not in the expected format";
    let body = sig_text.strip_suffix('\n').with_context(shape)?;
    let (b64, key_line) = body.split_once('\n').with_context(shape)?;
    let id = key_line.strip_prefix("key: ").with_context(shape)?;
    let hex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
    if id.len() != KEY_ID_LEN || !id.bytes().all(hex) {
        bail!(shape());
    }
    let raw = STANDARD.decode(b64).ok().with_context(shape)?;
    let bytes: [u8; 64] = raw.as_slice().try_into().ok().with_context(shape)?;
    // Only the one canonical encoding of these bytes is accepted.
    if STANDARD.encode(bytes) != b64 {
        bail!(shape());
    }
    Ok((Signature::from_bytes(&bytes), id))
}

/// Verifies `index_bytes` against a `.sig` text with one of the `trusted` keys, given as
/// `(key id, standard base64 of the 32-byte public key)`. The key named by the `.sig` is used; its id is
/// recomputed from its bytes and must match, so a mistyped trust list never verifies. Returns the key id used.
pub fn verify(index_bytes: &[u8], sig_text: &str, trusted: &[(&str, &str)]) -> Result<KeyId> {
    let (signature, id) = parse_sig(sig_text)?;
    let Some((_, public_b64)) = trusted.iter().find(|(trusted_id, _)| *trusted_id == id) else {
        bail!("The plugin index is signed with an unknown key: {id}");
    };
    let public = STANDARD
        .decode(public_b64)
        .ok()
        .and_then(|raw| <[u8; 32]>::try_from(raw.as_slice()).ok())
        .with_context(|| format!("The trusted key {id} is not a valid public key"))?;
    let actual = key_id(&public);
    if actual.as_str() != id {
        bail!("The trusted key {id} does not match its id");
    }
    let key = VerifyingKey::from_bytes(&public).ok().with_context(|| format!("The trusted key {id} is not a valid public key"))?;
    key.verify_strict(index_bytes, &signature).ok().context("The plugin index signature is not valid")?;
    Ok(actual)
}

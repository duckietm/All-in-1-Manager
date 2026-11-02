//! SSH keys: generating a key pair (OpenSSH "openssh-key-v1" exactly as `ssh-keygen` writes it, encrypted with
//! aes256-ctr and a bcrypt_pbkdf key, plus the same key as a PuTTY .ppk v2), and reading public keys in the OpenSSH
//! one-line format, authorized_keys lines and the RFC 4716 format PuTTYgen saves.

use aes::cipher::{BlockEncryptMut, KeyIvInit, StreamCipher};
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use base64::Engine;
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha1::Sha1;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshKeyType {
    Ed25519,
    Rsa4096,
}

/// A public key as "type base64 comment", the format of authorized_keys and .pub files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedKey {
    pub key_type: String,
    pub base64: String,
    pub comment: String,
}

impl AuthorizedKey {
    pub fn new(key_type: &str, base64: &str, comment: &str) -> Self {
        Self { key_type: key_type.into(), base64: base64.into(), comment: comment.into() }
    }

    pub fn line(&self) -> String {
        if self.comment.is_empty() {
            format!("{} {}", self.key_type, self.base64)
        } else {
            format!("{} {} {}", self.key_type, self.base64, self.comment)
        }
    }

    /// As printed by `ssh-keygen -l`.
    pub fn fingerprint(&self) -> String {
        let blob = STANDARD.decode(&self.base64).unwrap_or_default();
        format!("SHA256:{}", STANDARD_NO_PAD.encode(Sha256::digest(&blob)))
    }

    /// Ignores options and comments.
    pub fn same_key(&self, other: &AuthorizedKey) -> bool {
        self.key_type == other.key_type && self.base64 == other.base64
    }
}

/// A generated key pair, with the private key in both OpenSSH and PuTTY .ppk format.
#[derive(Debug, Clone)]
pub struct SshKeyPair {
    pub key_type: SshKeyType,
    pub private_key_pem: String,
    pub putty_key: String,
    pub public_key: AuthorizedKey,
}

impl SshKeyPair {
    pub fn file_name(&self) -> &'static str {
        match self.key_type {
            SshKeyType::Ed25519 => "id_ed25519",
            SshKeyType::Rsa4096 => "id_rsa",
        }
    }
}

const SUPPORTED_TYPES: [&str; 7] = [
    "ssh-ed25519",
    "ssh-rsa",
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp521",
    "sk-ssh-ed25519@openssh.com",
    "sk-ecdsa-sha2-nistp256@openssh.com",
];

/// ssh-keygen's default.
pub const KDF_ROUNDS: u32 = 16;

/// An empty `passphrase` leaves the private key unencrypted.
pub fn generate(key_type: SshKeyType, comment: &str, passphrase: &str) -> SshKeyPair {
    let comment = clean_comment(comment);
    match key_type {
        SshKeyType::Ed25519 => generate_ed25519(&comment, passphrase),
        SshKeyType::Rsa4096 => generate_rsa(&comment, passphrase),
    }
}

fn generate_ed25519(comment: &str, passphrase: &str) -> SshKeyPair {
    let signing = ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng);
    let seed = signing.to_bytes();
    let public_key = signing.verifying_key().to_bytes();

    let public_blob = blob(|w| {
        w.string("ssh-ed25519");
        w.bytes(&public_key);
    });
    let private_fields = blob(|w| {
        w.string("ssh-ed25519");
        w.bytes(&public_key);
        // OpenSSH stores seed || public key.
        w.bytes(&[seed.as_slice(), public_key.as_slice()].concat());
    });
    // PuTTY stores only the seed.
    let putty = putty_private_key("ssh-ed25519", &public_blob, &blob(|w| w.bytes(&seed)), comment, passphrase);
    SshKeyPair {
        key_type: SshKeyType::Ed25519,
        private_key_pem: openssh_private_key(&public_blob, &private_fields, comment, passphrase),
        putty_key: putty,
        public_key: AuthorizedKey::new("ssh-ed25519", &STANDARD.encode(&public_blob), comment),
    }
}

fn generate_rsa(comment: &str, passphrase: &str) -> SshKeyPair {
    use rsa::traits::{PrivateKeyParts, PublicKeyParts};
    let key = rsa::RsaPrivateKey::new(&mut rand::rngs::OsRng, 4096).expect("RSA key generation");
    let n = key.n().to_bytes_be();
    let e = key.e().to_bytes_be();
    let d = key.d().to_bytes_be();
    let primes = key.primes();
    let p = primes[0].to_bytes_be();
    let q = primes[1].to_bytes_be();
    // q^-1 mod p (RSAParameters.InverseQ).
    let inverse_q = key.crt_coefficient().expect("CRT coefficient").to_bytes_be();

    let public_blob = blob(|w| {
        w.string("ssh-rsa");
        w.mpint(&e);
        w.mpint(&n);
    });
    let private_fields = blob(|w| {
        w.string("ssh-rsa");
        w.mpint(&n);
        w.mpint(&e);
        w.mpint(&d);
        w.mpint(&inverse_q);
        w.mpint(&p);
        w.mpint(&q);
    });
    // PuTTY stores d, p, q and q^-1 mod p. n and e are in the public part.
    let putty_private = blob(|w| {
        w.mpint(&d);
        w.mpint(&p);
        w.mpint(&q);
        w.mpint(&inverse_q);
    });
    let putty = putty_private_key("ssh-rsa", &public_blob, &putty_private, comment, passphrase);
    SshKeyPair {
        key_type: SshKeyType::Rsa4096,
        private_key_pem: openssh_private_key(&public_blob, &private_fields, comment, passphrase),
        putty_key: putty,
        public_key: AuthorizedKey::new("ssh-rsa", &STANDARD.encode(&public_blob), comment),
    }
}

/// "openssh-key-v1" as ssh-keygen writes it, with aes256-ctr and a bcrypt_pbkdf key when there is a passphrase.
fn openssh_private_key(public_blob: &[u8], private_fields: &[u8], comment: &str, passphrase: &str) -> String {
    let encrypted = !passphrase.is_empty();
    let block_size = if encrypted { 16 } else { 8 };
    let check = rand::rngs::OsRng.next_u32();
    let mut private_section = blob(|w| {
        w.u32(check);
        w.u32(check);
        w.raw(private_fields);
        w.string(comment);
        // Padded with 1, 2, 3, ... to the cipher block size.
        let mut pad = 1u8;
        while w.len() % block_size != 0 {
            w.raw(&[pad]);
            pad += 1;
        }
    });

    let mut kdf_options = Vec::new();
    if encrypted {
        let mut salt = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut salt);
        kdf_options = blob(|w| {
            w.bytes(&salt);
            w.u32(KDF_ROUNDS);
        });
        let mut key_and_iv = [0u8; 48];
        bcrypt_pbkdf::bcrypt_pbkdf(passphrase.as_bytes(), &salt, KDF_ROUNDS, &mut key_and_iv).expect("bcrypt_pbkdf");
        // The IV is a 128-bit big-endian counter.
        let mut cipher = ctr::Ctr128BE::<aes::Aes256>::new(key_and_iv[..32].into(), key_and_iv[32..].into());
        cipher.apply_keystream(&mut private_section);
        key_and_iv.fill(0);
    }
    let body = blob(|w| {
        w.raw(b"openssh-key-v1\0");
        w.string(if encrypted { "aes256-ctr" } else { "none" });
        w.string(if encrypted { "bcrypt" } else { "none" });
        w.bytes(&kdf_options);
        w.u32(1);
        w.bytes(public_blob);
        w.bytes(&private_section);
    });

    let mut pem = String::from("-----BEGIN OPENSSH PRIVATE KEY-----\n");
    let base64 = STANDARD.encode(&body);
    for chunk in base64.as_bytes().chunks(70) {
        pem.push_str(std::str::from_utf8(chunk).unwrap());
        pem.push('\n');
    }
    pem.push_str("-----END OPENSSH PRIVATE KEY-----\n");
    pem
}

/// PPK v2, which every PuTTY since 0.52 reads, HeidiSQL's plink included. v3 would need Argon2.
/// Encryption is AES-256-CBC with a zero IV and the key SHA1(0000 || pass) || SHA1(0001 || pass) cut to 32 bytes.
/// The MAC is HMAC-SHA1 keyed with SHA1("putty-private-key-file-mac-key" || pass) over the algorithm, encryption,
/// comment, public blob and padded private plaintext.
fn putty_private_key(algorithm: &str, public_blob: &[u8], private_blob: &[u8], comment: &str, passphrase: &str) -> String {
    let encrypted = !passphrase.is_empty();
    let encryption = if encrypted { "aes256-cbc" } else { "none" };
    let secret = passphrase.as_bytes();

    // Padded to the AES block size. Readers ignore what follows the key, and the MAC covers it.
    let mut plain = private_blob.to_vec();
    if encrypted && !plain.len().is_multiple_of(16) {
        let mut padding = vec![0u8; 16 - plain.len() % 16];
        rand::rngs::OsRng.fill_bytes(&mut padding);
        plain.extend_from_slice(&padding);
    }

    let mac = putty_mac(algorithm, encryption, comment, public_blob, &plain, secret);

    let mut stored = plain.clone();
    if encrypted {
        let key = putty_cipher_key(secret);
        let encryptor = cbc::Encryptor::<aes::Aes256>::new(key[..32].into(), &[0u8; 16].into());
        let length = stored.len();
        encryptor.encrypt_padded_mut::<aes::cipher::block_padding::NoPadding>(&mut stored, length).expect("block aligned");
    }

    fn lines(text: &mut String, label: &str, data: &[u8]) {
        let base64 = STANDARD.encode(data);
        let count = base64.len().div_ceil(64);
        text.push_str(&format!("{label}: {count}\n"));
        for chunk in base64.as_bytes().chunks(64) {
            text.push_str(std::str::from_utf8(chunk).unwrap());
            text.push('\n');
        }
    }

    let mut ppk = String::new();
    ppk.push_str(&format!("PuTTY-User-Key-File-2: {algorithm}\n"));
    ppk.push_str(&format!("Encryption: {encryption}\n"));
    ppk.push_str(&format!("Comment: {comment}\n"));
    lines(&mut ppk, "Public-Lines", public_blob);
    lines(&mut ppk, "Private-Lines", &stored);
    ppk.push_str(&format!("Private-MAC: {mac}\n"));
    ppk
}

fn putty_mac(algorithm: &str, encryption: &str, comment: &str, public_blob: &[u8], plain: &[u8], secret: &[u8]) -> String {
    let mac_key = Sha1::digest([b"putty-private-key-file-mac-key".as_slice(), secret].concat());
    let mac_data = blob(|w| {
        w.string(algorithm);
        w.string(encryption);
        w.string(comment);
        w.bytes(public_blob);
        w.bytes(plain);
    });
    let mut hmac = <Hmac<Sha1> as Mac>::new_from_slice(&mac_key).expect("any key length");
    hmac.update(&mac_data);
    hex::encode(hmac.finalize().into_bytes())
}

fn putty_cipher_key(secret: &[u8]) -> Vec<u8> {
    let first = Sha1::digest([&[0u8, 0, 0, 0][..], secret].concat());
    let second = Sha1::digest([&[0u8, 0, 0, 1][..], secret].concat());
    [first.as_slice(), second.as_slice()].concat()
}

/// Why a PuTTY key could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PpkError {
    #[error("This key has a password: enter it.")]
    NeedsPassword,
    #[error("The key could not be opened: wrong key password?")]
    WrongPassword,
    #[error("This is not a PuTTY key the installer can use: {0}")]
    Invalid(String),
}

/// Reads a PuTTY .ppk (version 2 or 3, unencrypted or aes256-cbc) and returns the same key as an unencrypted
/// OpenSSH private key, which the SSH library can log in with. Ed25519, RSA and ECDSA keys.
pub fn ppk_to_openssh(text: &str, passphrase: &str) -> Result<String, PpkError> {
    let invalid = |why: &str| PpkError::Invalid(why.to_string());
    let text = text.replace("\r\n", "\n");
    let mut lines = text.lines().map(str::trim_end).peekable();
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut public = Vec::new();
    let mut private = Vec::new();
    while let Some(line) = lines.next() {
        let Some((name, value)) = line.split_once(": ") else { continue };
        let mut read_lines = |count: &str| -> Result<Vec<u8>, PpkError> {
            let count: usize = count.trim().parse().map_err(|_| invalid("bad line count"))?;
            let mut base64 = String::new();
            for _ in 0..count {
                base64.push_str(lines.next().ok_or_else(|| invalid("the file ends too early"))?.trim());
            }
            STANDARD.decode(base64).map_err(|_| invalid("bad base64"))
        };
        match name {
            "Public-Lines" => public = read_lines(value)?,
            "Private-Lines" => private = read_lines(value)?,
            _ => headers.push((name.to_string(), value.to_string())),
        }
    }
    let header = |name: &str| headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str());
    let (version, algorithm) = if let Some(alg) = header("PuTTY-User-Key-File-3") {
        (3, alg)
    } else if let Some(alg) = header("PuTTY-User-Key-File-2") {
        (2, alg)
    } else {
        return Err(invalid("not a PuTTY-User-Key-File version 2 or 3"));
    };
    let encryption = header("Encryption").unwrap_or("none");
    let comment = header("Comment").unwrap_or("");
    let mac_hex = header("Private-MAC").ok_or_else(|| invalid("no Private-MAC"))?;
    let encrypted = match encryption {
        "none" => false,
        "aes256-cbc" => true,
        other => return Err(invalid(&format!("encryption {other} is not supported"))),
    };
    if encrypted && passphrase.is_empty() {
        return Err(PpkError::NeedsPassword);
    }
    let secret = passphrase.as_bytes();

    // Key material: v2 from SHA-1, v3 from Argon2 (cipher key, IV and MAC key).
    let (cipher_key, iv, mac_key): (Vec<u8>, Vec<u8>, Vec<u8>) = if version == 3 {
        if encrypted {
            let flavour = match header("Key-Derivation") {
                Some("Argon2id") => argon2::Algorithm::Argon2id,
                Some("Argon2i") => argon2::Algorithm::Argon2i,
                Some("Argon2d") => argon2::Algorithm::Argon2d,
                _ => return Err(invalid("unknown Key-Derivation")),
            };
            let number = |name: &str| header(name).and_then(|v| v.parse::<u32>().ok()).ok_or_else(|| invalid(&format!("no {name}")));
            let salt = hex::decode(header("Argon2-Salt").unwrap_or("")).map_err(|_| invalid("bad Argon2-Salt"))?;
            let params = argon2::Params::new(number("Argon2-Memory")?, number("Argon2-Passes")?, number("Argon2-Parallelism")?, Some(80))
                .map_err(|e| invalid(&e.to_string()))?;
            let mut out = [0u8; 80];
            argon2::Argon2::new(flavour, argon2::Version::V0x13, params)
                .hash_password_into(secret, &salt, &mut out)
                .map_err(|e| invalid(&e.to_string()))?;
            (out[..32].to_vec(), out[32..48].to_vec(), out[48..].to_vec())
        } else {
            (Vec::new(), Vec::new(), Vec::new())
        }
    } else {
        let mac_key = Sha1::digest([b"putty-private-key-file-mac-key".as_slice(), secret].concat()).to_vec();
        (putty_cipher_key(secret)[..32].to_vec(), vec![0u8; 16], mac_key)
    };

    let mut plain = private.clone();
    if encrypted {
        use aes::cipher::BlockDecryptMut;
        if plain.len() % 16 != 0 {
            return Err(invalid("the private part is not a whole number of blocks"));
        }
        let length = plain.len();
        cbc::Decryptor::<aes::Aes256>::new(cipher_key.as_slice().into(), iv.as_slice().into())
            .decrypt_padded_mut::<aes::cipher::block_padding::NoPadding>(&mut plain[..length])
            .map_err(|_| invalid("decryption failed"))?;
    }
    let mac_data = blob(|w| {
        w.string(algorithm);
        w.string(encryption);
        w.string(comment);
        w.bytes(&public);
        w.bytes(&plain);
    });
    let expected = hex::decode(mac_hex.trim()).map_err(|_| invalid("bad Private-MAC"))?;
    let valid = if version == 3 {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&mac_key).expect("any key length");
        mac.update(&mac_data);
        mac.verify_slice(&expected).is_ok()
    } else {
        let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(&mac_key).expect("any key length");
        mac.update(&mac_data);
        mac.verify_slice(&expected).is_ok()
    };
    if !valid {
        return Err(if encrypted { PpkError::WrongPassword } else { invalid("the MAC does not match: the file is damaged") });
    }

    // PuTTY's private fields -> OpenSSH's.
    let mut pub_reader = SshReader(&public);
    let key_type = pub_reader.string().ok_or_else(|| invalid("bad public key"))?;
    let mut priv_reader = SshReader(&plain);
    let fields = match key_type.as_str() {
        "ssh-ed25519" => {
            let public_key = pub_reader.bytes().ok_or_else(|| invalid("bad public key"))?;
            let seed = priv_reader.bytes().ok_or_else(|| invalid("bad private key"))?;
            if seed.len() != 32 || public_key.len() != 32 {
                return Err(invalid("bad Ed25519 key"));
            }
            blob(|w| {
                w.string("ssh-ed25519");
                w.bytes(public_key);
                w.bytes(&[seed, public_key].concat());
            })
        }
        "ssh-rsa" => {
            let e = pub_reader.bytes().ok_or_else(|| invalid("bad public key"))?;
            let n = pub_reader.bytes().ok_or_else(|| invalid("bad public key"))?;
            let mut next = || priv_reader.bytes().ok_or_else(|| invalid("bad private key"));
            let (d, p, q, iqmp) = (next()?, next()?, next()?, next()?);
            blob(|w| {
                w.string("ssh-rsa");
                for value in [n, e, d, iqmp, p, q] {
                    w.bytes(value); // already mpint encoded
                }
            })
        }
        t if t.starts_with("ecdsa-sha2-") => {
            let curve = pub_reader.bytes().ok_or_else(|| invalid("bad public key"))?;
            let point = pub_reader.bytes().ok_or_else(|| invalid("bad public key"))?;
            let d = priv_reader.bytes().ok_or_else(|| invalid("bad private key"))?;
            blob(|w| {
                w.string(t);
                w.bytes(curve);
                w.bytes(point);
                w.bytes(d);
            })
        }
        other => return Err(invalid(&format!("key type {other} is not supported"))),
    };
    let _ = algorithm;
    Ok(openssh_private_key(&public, &fields, comment, ""))
}

struct SshReader<'a>(&'a [u8]);

impl<'a> SshReader<'a> {
    fn bytes(&mut self) -> Option<&'a [u8]> {
        if self.0.len() < 4 {
            return None;
        }
        let length = u32::from_be_bytes(self.0[..4].try_into().ok()?) as usize;
        if self.0.len() < 4 + length {
            return None;
        }
        let value = &self.0[4..4 + length];
        self.0 = &self.0[4 + length..];
        Some(value)
    }

    fn string(&mut self) -> Option<String> {
        self.bytes().map(|b| String::from_utf8_lossy(b).into_owned())
    }
}

pub fn is_private_key(text: &str) -> bool {
    text.contains("PRIVATE KEY") || text.trim_start().starts_with("PuTTY-User-Key-File")
}

/// Accepts the OpenSSH one-line format or the RFC 4716 format PuTTYgen saves.
pub fn try_parse_public_key(text: &str) -> Result<AuthorizedKey, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Paste a public key or load a .pub file.".into());
    }
    if text.contains("PRIVATE KEY") || text.starts_with("PuTTY-User-Key-File") {
        return Err("This is a private key. Never share it: use the public key (the .pub file) instead.".into());
    }
    let parsed = if text.starts_with("---- BEGIN SSH2 PUBLIC KEY ----") {
        parse_rfc4716(text)
    } else {
        let lines: Vec<&str> = text.split('\n').map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
        if lines.len() == 1 {
            parse_line(lines[0])
        } else {
            None
        }
    };
    let Some(parsed) = parsed else {
        return Err("That is not a valid public key. It should look like: ssh-ed25519 AAAAC3Nza... you@computer".into());
    };
    if !SUPPORTED_TYPES.contains(&parsed.key_type.as_str()) {
        return Err(if parsed.key_type == "ssh-dss" {
            "DSA keys are no longer accepted by OpenSSH. Use an Ed25519 or RSA key.".into()
        } else {
            format!("Key type '{}' is not supported.", parsed.key_type)
        });
    }
    Ok(parsed)
}

/// Parses an authorized_keys line, skipping leading options. Returns None if it holds no valid key.
pub fn parse_line(line: &str) -> Option<AuthorizedKey> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    for i in 0..parts.len().saturating_sub(1) {
        if !looks_like_key_type(parts[i]) {
            continue;
        }
        if blob_type(parts[i + 1]).as_deref() != Some(parts[i]) {
            continue;
        }
        return Some(AuthorizedKey::new(parts[i], parts[i + 1], &parts[i + 2..].join(" ")));
    }
    None
}

pub fn parse_authorized_keys(content: &str) -> Vec<AuthorizedKey> {
    content.split('\n').map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).filter_map(parse_line).collect()
}

fn parse_rfc4716(text: &str) -> Option<AuthorizedKey> {
    let text = text.replace('\r', "");
    let lines: Vec<&str> = text.split('\n').map(str::trim).collect();
    let end = lines.iter().position(|l| l.starts_with("---- END SSH2 PUBLIC KEY ----"))?;
    let mut comment = String::new();
    let mut base64 = String::new();
    let mut continued = false;
    for line in lines.iter().skip(1).take(end.saturating_sub(1)) {
        if continued || line.contains(':') {
            // Header line. A trailing backslash continues it on the next line.
            if !continued && line.len() >= 8 && line[..8].eq_ignore_ascii_case("Comment:") {
                comment = line[8..].trim().trim_matches('"').to_string();
            }
            continued = line.ends_with('\\');
            continue;
        }
        base64.push_str(line);
    }
    let key_type = blob_type(&base64)?;
    Some(AuthorizedKey::new(&key_type, &base64, &clean_comment(&comment)))
}

fn looks_like_key_type(token: &str) -> bool {
    token.starts_with("ssh-") || token.starts_with("ecdsa-") || token.starts_with("sk-")
}

/// The type string at the start of the blob, or None if it is not a key blob.
fn blob_type(base64: &str) -> Option<String> {
    let blob = STANDARD.decode(base64).ok()?;
    if blob.len() < 4 {
        return None;
    }
    let length = u32::from_be_bytes([blob[0], blob[1], blob[2], blob[3]]) as usize;
    if length == 0 || length > 64 || 4 + length > blob.len() {
        return None;
    }
    Some(blob[4..4 + length].iter().map(|&b| b as char).collect())
}

/// authorized_keys comments must be a single line.
pub fn clean_comment(comment: &str) -> String {
    comment.split_whitespace().map(|word| word.chars().filter(|c| !c.is_control()).collect::<String>()).collect::<Vec<_>>().join(" ")
}

fn blob(write: impl FnOnce(&mut SshWriter)) -> Vec<u8> {
    let mut writer = SshWriter(Vec::new());
    write(&mut writer);
    writer.0
}

struct SshWriter(Vec<u8>);

impl SshWriter {
    fn len(&self) -> usize {
        self.0.len()
    }

    fn raw(&mut self, bytes: &[u8]) {
        self.0.extend_from_slice(bytes);
    }

    fn u32(&mut self, value: u32) {
        self.0.extend_from_slice(&value.to_be_bytes());
    }

    fn bytes(&mut self, bytes: &[u8]) {
        self.u32(bytes.len() as u32);
        self.raw(bytes);
    }

    fn string(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    /// Big-endian two's complement without leading zeros, plus a zero byte when the top bit is set.
    fn mpint(&mut self, unsigned_big_endian: &[u8]) {
        let start = unsigned_big_endian.iter().position(|&b| b != 0).unwrap_or(unsigned_big_endian.len());
        let trimmed = &unsigned_big_endian[start..];
        if trimmed.first().is_some_and(|b| b & 0x80 != 0) {
            self.bytes(&[&[0u8][..], trimmed].concat());
        } else {
            self.bytes(trimmed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockDecryptMut;

    fn read_u32(data: &[u8], at: &mut usize) -> u32 {
        let v = u32::from_be_bytes(data[*at..*at + 4].try_into().unwrap());
        *at += 4;
        v
    }

    fn read_bytes<'a>(data: &'a [u8], at: &mut usize) -> &'a [u8] {
        let len = read_u32(data, at) as usize;
        let v = &data[*at..*at + len];
        *at += len;
        v
    }

    /// Decrypts our own OpenSSH file the way OpenSSH does and checks the structure.
    fn open_openssh(pem: &str, passphrase: &str) -> Vec<u8> {
        let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
        let data = STANDARD.decode(body).unwrap();
        assert!(data.starts_with(b"openssh-key-v1\0"));
        let mut at = 15;
        let cipher = read_bytes(&data, &mut at).to_vec();
        let kdf = read_bytes(&data, &mut at).to_vec();
        let options = read_bytes(&data, &mut at).to_vec();
        assert_eq!(read_u32(&data, &mut at), 1);
        let _public = read_bytes(&data, &mut at);
        let mut private = read_bytes(&data, &mut at).to_vec();
        if cipher == b"aes256-ctr" {
            assert_eq!(kdf, b"bcrypt");
            let mut o = 0;
            let salt = read_bytes(&options, &mut o).to_vec();
            let rounds = read_u32(&options, &mut o);
            assert_eq!(rounds, 16);
            let mut key_iv = [0u8; 48];
            bcrypt_pbkdf::bcrypt_pbkdf(passphrase.as_bytes(), &salt, rounds, &mut key_iv).unwrap();
            ctr::Ctr128BE::<aes::Aes256>::new(key_iv[..32].into(), key_iv[32..].into()).apply_keystream(&mut private);
        }
        private
    }

    #[test]
    fn ed25519_openssh_and_putty() {
        let pair = generate(SshKeyType::Ed25519, "me@pc\nsecond line", "correct horse");
        assert_eq!(pair.public_key.comment, "me@pc second line");
        assert!(pair.private_key_pem.lines().all(|l| l.len() <= 70 || l.starts_with("-----")));
        let private = open_openssh(&pair.private_key_pem, "correct horse");
        // check ints equal, then the key type.
        assert_eq!(&private[0..4], &private[4..8]);
        let mut at = 8;
        assert_eq!(read_bytes(&private, &mut at), b"ssh-ed25519");
        assert_eq!(private.len() % 16, 0);

        // The .ppk: decrypt and verify the MAC like PuTTY does.
        let ppk = &pair.putty_key;
        assert!(ppk.starts_with("PuTTY-User-Key-File-2: ssh-ed25519\nEncryption: aes256-cbc\nComment: me@pc second line\nPublic-Lines: 2\n"));
        let mut lines = ppk.lines();
        let mut section = |label: &str| -> Vec<u8> {
            let header = lines.find(|l| l.starts_with(label)).unwrap();
            let count: usize = header.split(": ").nth(1).unwrap().parse().unwrap();
            let text: String = (0..count).map(|_| lines.next().unwrap()).collect();
            STANDARD.decode(text).unwrap()
        };
        let public = section("Public-Lines");
        let mut stored = section("Private-Lines");
        let mac = ppk.lines().last().unwrap().strip_prefix("Private-MAC: ").unwrap().to_string();
        let key = putty_cipher_key(b"correct horse");
        let length = stored.len();
        cbc::Decryptor::<aes::Aes256>::new(key[..32].into(), &[0u8; 16].into())
            .decrypt_padded_mut::<aes::cipher::block_padding::NoPadding>(&mut stored[..length])
            .unwrap();
        assert_eq!(putty_mac("ssh-ed25519", "aes256-cbc", "me@pc second line", &public, &stored, b"correct horse"), mac);
        assert_eq!(STANDARD.encode(&public), pair.public_key.base64);
    }

    /// The OpenSSH key fields after the two check ints, up to the comment.
    fn openssh_fields(pem: &str, passphrase: &str) -> Vec<u8> {
        let private = open_openssh(pem, passphrase);
        private[8..].to_vec()
    }

    #[test]
    fn ppk_reads_back_as_the_same_openssh_key() {
        for key_type in [SshKeyType::Ed25519, SshKeyType::Rsa4096] {
            let pair = generate(key_type, "me@pc", "secret pass");
            let converted = ppk_to_openssh(&pair.putty_key, "secret pass").unwrap();
            let original = openssh_fields(&pair.private_key_pem, "secret pass");
            let from_ppk = openssh_fields(&converted, "");
            // Same fields and comment; only the block padding differs (16 encrypted, 8 plain).
            let unpad = |v: &[u8]| {
                let mut v = v.to_vec();
                while v.last().is_some_and(|&b| b != 0 && (b as usize) < 16 && v.ends_with(&(1..=b).collect::<Vec<u8>>())) {
                    let b = *v.last().unwrap();
                    v.truncate(v.len() - b as usize);
                }
                v
            };
            assert_eq!(unpad(&original), unpad(&from_ppk), "{key_type:?}");
            assert_eq!(ppk_to_openssh(&pair.putty_key, "wrong").unwrap_err(), PpkError::WrongPassword);
            assert_eq!(ppk_to_openssh(&pair.putty_key, "").unwrap_err(), PpkError::NeedsPassword);
        }
        let plain = generate(SshKeyType::Ed25519, "x", "");
        assert!(ppk_to_openssh(&plain.putty_key, "").is_ok());
    }

    #[test]
    fn unencrypted_key() {
        let pair = generate(SshKeyType::Ed25519, "x", "");
        let private = open_openssh(&pair.private_key_pem, "");
        assert_eq!(private.len() % 8, 0);
        assert!(pair.putty_key.contains("Encryption: none\n"));
    }

    #[test]
    fn public_key_formats() {
        let pair = generate(SshKeyType::Ed25519, "me@pc", "");
        let parsed = try_parse_public_key(&pair.public_key.line()).unwrap();
        assert_eq!(parsed, pair.public_key);
        let with_options = format!("command=\"echo hi\",no-pty {}", pair.public_key.line());
        assert!(parse_line(&with_options).unwrap().same_key(&pair.public_key));
        let rfc = format!(
            "---- BEGIN SSH2 PUBLIC KEY ----\nComment: \"rsa-key-20261024\"\n{}\n---- END SSH2 PUBLIC KEY ----\n",
            pair.public_key.base64
        );
        let parsed = try_parse_public_key(&rfc).unwrap();
        assert_eq!(parsed.comment, "rsa-key-20261024");
        assert!(try_parse_public_key(&pair.private_key_pem).unwrap_err().contains("private key"));
        assert!(try_parse_public_key("ssh-ed25519 AAAA").is_err());
        assert!(pair.public_key.fingerprint().starts_with("SHA256:"));
        assert!(!pair.public_key.fingerprint().ends_with('='));
    }

    #[test]
    fn mpint_encoding() {
        let mut w = SshWriter(Vec::new());
        w.mpint(&[0, 0, 0x80, 1]);
        assert_eq!(w.0, vec![0, 0, 0, 3, 0, 0x80, 1]);
        let mut w = SshWriter(Vec::new());
        w.mpint(&[0x7f]);
        assert_eq!(w.0, vec![0, 0, 0, 1, 0x7f]);
    }
}

//! `aio-index`: builds, signs and verifies the official plugin index (run by the index repository's CI).
//!
//! Every error goes to stderr with a non-zero exit. Secrets are never printed, not even in errors.

use aio_plugin_host::package::inspect;
use aio_plugin_index::{key_id, sign, verify_and_parse, Entry, Index, Revoked, Source};
use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use chrono::{Duration, SecondsFormat, Utc};
use ed25519_dalek::SigningKey;
use rand::RngCore as _;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// `--flag value` pairs and bare positional arguments.
struct Args {
    flags: BTreeMap<String, String>,
    positional: Vec<String>,
}

impl Args {
    fn parse(raw: &[String]) -> Result<Args> {
        let (mut flags, mut positional) = (BTreeMap::new(), Vec::new());
        let mut it = raw.iter();
        while let Some(a) = it.next() {
            if let Some(name) = a.strip_prefix("--") {
                let value = it.next().with_context(|| format!("--{name} needs a value"))?;
                if flags.insert(name.to_string(), value.clone()).is_some() {
                    bail!("--{name} given twice");
                }
            } else {
                positional.push(a.clone());
            }
        }
        Ok(Args { flags, positional })
    }

    fn get(&self, name: &str) -> Result<&str> {
        self.flags.get(name).map(String::as_str).with_context(|| format!("missing --{name}"))
    }

    fn only(&self, allowed: &[&str], positional: usize) -> Result<()> {
        if let Some(f) = self.flags.keys().find(|k| !allowed.contains(&k.as_str())) {
            bail!("unknown option --{f}");
        }
        if self.positional.len() != positional {
            bail!("expected {positional} file argument(s), got {}", self.positional.len());
        }
        Ok(())
    }
}

fn decode_32(b64: &str, what: &str) -> Result<[u8; 32]> {
    let raw = STANDARD.decode(b64.trim()).ok().with_context(|| format!("{what} is not valid base64"))?;
    <[u8; 32]>::try_from(raw.as_slice()).ok().with_context(|| format!("{what} is not 32 bytes"))
}

fn read(path: &str) -> Result<Vec<u8>> {
    std::fs::read(path).with_context(|| format!("cannot read {path}"))
}

fn keygen(a: &Args) -> Result<()> {
    a.only(&["out"], 0)?;
    let dir = PathBuf::from(a.get("out")?);
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let path = dir.join("secret.key");
    let mut seed = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut seed);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let mut file = opts.open(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            anyhow::anyhow!("{} already exists; refusing to overwrite it", path.display())
        } else {
            anyhow::anyhow!("cannot create {}: {e}", path.display())
        }
    })?;
    std::io::Write::write_all(&mut file, format!("{}\n", STANDARD.encode(seed)).as_bytes())
        .with_context(|| format!("cannot write {}", path.display()))?;
    let public = SigningKey::from_bytes(&seed).verifying_key().to_bytes();
    println!("public: {}", STANDARD.encode(public));
    println!("key-id: {}", key_id(&public));
    Ok(())
}

fn entry(a: &Args) -> Result<()> {
    a.only(&["aiop", "prefix", "repo", "commit"], 0)?;
    let bytes = read(a.get("aiop")?)?;
    let prefix = a.get("prefix")?;
    let package = inspect(&bytes, &[]).context("the package is not valid")?;
    let m = package.manifest;
    let entry = Entry {
        url: format!("{prefix}{}-{}/{}.aiop", m.id, m.version, m.id),
        id: m.id,
        name: m.name,
        version: m.version,
        author: m.author,
        description: m.description,
        api: m.api,
        permissions: m.permissions.iter().map(ToString::to_string).collect(),
        size: bytes.len() as u64,
        sha256: Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect(),
        source: Source { repo: a.get("repo")?.to_string(), commit: a.get("commit")?.to_string() },
    };
    println!("{}", serde_json::to_string_pretty(&entry)?);
    Ok(())
}

fn build(a: &Args) -> Result<()> {
    a.only(&["entries", "revoked", "previous", "days", "prefix", "out"], 0)?;
    let prefix = a.get("prefix")?;
    let days: i64 = a.get("days")?.parse().ok().filter(|d| (1..=365).contains(d)).context("--days must be 1 to 365")?;
    let previous = match a.get("previous")? {
        "none" => 0,
        path => {
            let v: serde_json::Value = serde_json::from_slice(&read(path)?).context("the previous index is not valid json")?;
            v["sequence"].as_u64().context("the previous index has no sequence")?
        }
    };
    let dir = a.get("entries")?;
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("cannot read {dir}"))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    let mut plugins = Vec::new();
    for f in &files {
        let e: Entry = serde_json::from_slice(&read(&f.to_string_lossy())?)
            .with_context(|| format!("{} is not a valid entry", f.display()))?;
        plugins.push(e);
    }
    plugins.sort_by(|x, y| x.id.cmp(&y.id));
    let revoked: Vec<Revoked> = serde_json::from_slice(&read(a.get("revoked")?)?).context("the revocation list is not valid")?;
    let now = Utc::now();
    let index = Index {
        schema: aio_plugin_index::SCHEMA,
        sequence: previous.checked_add(1).context("sequence overflow")?,
        generated: now,
        expires: now + Duration::days(days),
        plugins,
        revoked,
    };
    let mut json = serde_json::to_string_pretty(&index)?;
    json.push('\n');
    // Never write what the Manager would refuse.
    Index::parse(json.as_bytes(), prefix).context("the built index is not valid")?;
    let out = a.get("out")?;
    std::fs::write(out, json).with_context(|| format!("cannot write {out}"))?;
    println!(
        "built sequence={} plugins={} expires={}",
        index.sequence,
        index.plugins.len(),
        index.expires.to_rfc3339_opts(SecondsFormat::Secs, true)
    );
    Ok(())
}

fn sign_cmd(a: &Args) -> Result<()> {
    a.only(&["key", "prefix"], 1)?;
    let secret_b64 = match a.flags.get("key") {
        Some(path) => String::from_utf8(read(path)?).ok().context("the key file is not text")?,
        None => std::env::var("AIO_INDEX_KEY").ok().context("give --key <file> or set AIO_INDEX_KEY")?,
    };
    let secret = decode_32(&secret_b64, "the secret key")?;
    let path = &a.positional[0];
    let bytes = read(path)?;
    Index::parse(&bytes, a.get("prefix")?).context("refusing to sign an index the Manager would refuse")?;
    let sig = sign(&bytes, &secret);
    let sig_path = format!("{path}.sig");
    std::fs::write(&sig_path, sig).with_context(|| format!("cannot write {sig_path}"))?;
    println!("signed {sig_path}");
    Ok(())
}

fn verify_cmd(a: &Args) -> Result<()> {
    a.only(&["pubkey", "prefix"], 2)?;
    let pubkey = a.get("pubkey")?.trim();
    let id = key_id(&decode_32(pubkey, "the public key")?).to_string();
    let bytes = read(&a.positional[0])?;
    let sig = String::from_utf8(read(&a.positional[1])?).ok().context("the signature file is not text")?;
    let (index, _) = verify_and_parse(&bytes, &sig, &[(&id, pubkey)], a.get("prefix")?)?;
    println!("ok sequence={} plugins={}", index.sequence, index.plugins.len());
    Ok(())
}

fn run() -> Result<()> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let Some((cmd, rest)) = raw.split_first() else {
        bail!("usage: aio-index <keygen|entry|build|sign|verify> [options]");
    };
    let a = Args::parse(rest)?;
    match cmd.as_str() {
        "keygen" => keygen(&a),
        "entry" => entry(&a),
        "build" => build(&a),
        "sign" => sign_cmd(&a),
        "verify" => verify_cmd(&a),
        other => bail!("unknown command {other}"),
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

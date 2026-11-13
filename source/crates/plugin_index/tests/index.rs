use aio_plugin_index::{
    check_fresh, key_id, sign, verify, verify_and_parse, Freshness, Index, MAX_INDEX_BYTES, MAX_PACKAGE_BYTES, MAX_PLUGINS,
};
use base64::Engine as _;
use chrono::{DateTime, Utc};
use ed25519_dalek::SigningKey;
use serde_json::{json, Value};

const PREFIX: &str = "https://example.test/download/";
const SECRET_A: [u8; 32] = [7; 32];
const SECRET_B: [u8; 32] = [42; 32];

fn entry(id: &str) -> Value {
    json!({
        "id": id,
        "name": "Hello",
        "version": "1.2.3",
        "author": "Someone",
        "description": "Says hello.",
        "api": "1.0",
        "permissions": ["ssh", "sql:read:users,rooms", "http:api.example.com"],
        "size": 12345,
        "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "url": format!("{PREFIX}{id}-1.2.3/{id}.aiop"),
        "source": { "repo": "https://github.com/someone/hello", "commit": "0123456789abcdef0123456789abcdef01234567" }
    })
}

fn index_with(plugins: Vec<Value>, revoked: Value) -> Value {
    json!({
        "schema": 1,
        "sequence": 10,
        "generated": "2026-11-13T10:00:00Z",
        "expires": "2026-12-13T10:00:00Z",
        "plugins": plugins,
        "revoked": revoked,
    })
}

fn sample() -> Value {
    index_with(vec![entry("hello"), entry("other-plugin")], json!([]))
}

fn bytes(v: &Value) -> Vec<u8> {
    serde_json::to_vec_pretty(v).unwrap()
}

fn public_b64(secret: &[u8; 32]) -> (String, String) {
    let public = SigningKey::from_bytes(secret).verifying_key().to_bytes();
    let id = key_id(&public).to_string();
    (id, base64::engine::general_purpose::STANDARD.encode(public))
}

fn at(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

#[test]
fn round_trip_sign_verify() {
    let data = bytes(&sample());
    let sig = sign(&data, &SECRET_A);
    let (id, public) = public_b64(&SECRET_A);
    assert_eq!(id.len(), 16);
    assert!(id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
    let lines: Vec<&str> = sig.split('\n').collect();
    assert_eq!(lines.len(), 3, "two lines, each ending in a newline: {sig:?}");
    assert_eq!(lines[1], format!("key: {id}"));
    assert_eq!(lines[2], "");
    let got = verify(&data, &sig, &[(id.as_str(), public.as_str())]).unwrap();
    assert_eq!(got.to_string(), id);

    let index = Index::parse(&data, PREFIX).unwrap();
    assert_eq!(index.schema, 1);
    assert_eq!(index.sequence, 10);
    assert_eq!(index.plugins.len(), 2);
    let hello = index.entry("hello").unwrap();
    assert_eq!(hello.version, "1.2.3");
    assert_eq!(hello.size, 12345);
    assert_eq!(hello.source.commit, "0123456789abcdef0123456789abcdef01234567");
    assert!(index.entry("missing").is_none());

    // Other shapes of the .sig file are refused.
    let trusted = [(id.as_str(), public.as_str())];
    for bad in [
        sig.trim_end().to_string(),
        sig.replace('\n', "\r\n"),
        format!("{sig}\n"),
        format!(" {sig}"),
        sig.replace("key: ", "key:"),
        sig.replace("key: ", "Key: "),
        format!("{}\nkey: {}\n", lines[0].trim_end_matches('='), id),
        format!("{}\nkey: {}\n", lines[0], id.to_uppercase()),
        String::new(),
    ] {
        assert!(verify(&data, &bad, &trusted).is_err(), "accepted {bad:?}");
    }
}

#[test]
fn tampered_byte_fails() {
    let data = bytes(&sample());
    let sig = sign(&data, &SECRET_A);
    let (id, public) = public_b64(&SECRET_A);
    let trusted = [(id.as_str(), public.as_str())];
    for i in [0, data.len() / 2, data.len() - 1] {
        let mut tampered = data.clone();
        tampered[i] ^= 0x01;
        assert!(verify(&tampered, &sig, &trusted).is_err(), "byte {i}");
    }
    let mut longer = data.clone();
    longer.push(b'\n');
    assert!(verify(&longer, &sig, &trusted).is_err());

    // A flipped bit in the signature itself.
    let (line, rest) = sig.split_once('\n').unwrap();
    let mut raw = base64::engine::general_purpose::STANDARD.decode(line).unwrap();
    raw[10] ^= 0x80;
    let bad = format!("{}\n{rest}", base64::engine::general_purpose::STANDARD.encode(raw));
    assert!(verify(&data, &bad, &trusted).is_err());
}

#[test]
fn unknown_key_fails() {
    let data = bytes(&sample());
    let sig = sign(&data, &SECRET_A);
    let (id_a, public_a) = public_b64(&SECRET_A);
    let (id_b, public_b) = public_b64(&SECRET_B);

    // Only key B is trusted.
    assert!(verify(&data, &sig, &[(id_b.as_str(), public_b.as_str())]).is_err());
    // Nothing is trusted.
    assert!(verify(&data, &sig, &[]).is_err());
    // A mistyped trust list: A's id paired with B's public key must not verify.
    assert!(verify(&data, &sig, &[(id_a.as_str(), public_b.as_str())]).is_err());
    // A's id paired with garbage.
    assert!(verify(&data, &sig, &[(id_a.as_str(), "not base64!")]).is_err());
    // Signed by B but the .sig claims key A, with both trusted.
    let sig_b = sign(&data, &SECRET_B).replace(&id_b, &id_a);
    let both = [(id_a.as_str(), public_a.as_str()), (id_b.as_str(), public_b.as_str())];
    assert!(verify(&data, &sig_b, &both).is_err());
}

#[test]
fn two_keys_rotation() {
    let data = bytes(&sample());
    let (id_a, public_a) = public_b64(&SECRET_A);
    let (id_b, public_b) = public_b64(&SECRET_B);
    assert_ne!(id_a, id_b);
    let both = [(id_a.as_str(), public_a.as_str()), (id_b.as_str(), public_b.as_str())];
    assert_eq!(verify(&data, &sign(&data, &SECRET_A), &both).unwrap().to_string(), id_a);
    assert_eq!(verify(&data, &sign(&data, &SECRET_B), &both).unwrap().to_string(), id_b);
    let reversed = [both[1], both[0]];
    assert_eq!(verify(&data, &sign(&data, &SECRET_A), &reversed).unwrap().to_string(), id_a);
}

#[test]
fn unknown_field_refused() {
    let mut top = sample();
    top["extra"] = json!(true);
    assert!(Index::parse(&bytes(&top), PREFIX).is_err());

    let mut in_entry = sample();
    in_entry["plugins"][0]["homepage"] = json!("https://x.test");
    assert!(Index::parse(&bytes(&in_entry), PREFIX).is_err());

    let mut in_source = sample();
    in_source["plugins"][0]["source"]["branch"] = json!("main");
    assert!(Index::parse(&bytes(&in_source), PREFIX).is_err());

    let mut in_revoked = index_with(vec![entry("hello")], json!([{ "id": "hello", "versions": ["*"], "reason": "r" }]));
    assert!(Index::parse(&bytes(&in_revoked), PREFIX).is_ok());
    in_revoked["revoked"][0]["note"] = json!("x");
    assert!(Index::parse(&bytes(&in_revoked), PREFIX).is_err());

    // Missing fields, wrong schema, trailing data, duplicate keys, duplicate ids, bad values.
    let mut missing = sample();
    missing["plugins"][0].as_object_mut().unwrap().remove("sha256");
    assert!(Index::parse(&bytes(&missing), PREFIX).is_err());
    let mut schema2 = sample();
    schema2["schema"] = json!(2);
    assert!(Index::parse(&bytes(&schema2), PREFIX).is_err());
    let mut trailing = bytes(&sample());
    trailing.extend_from_slice(b" {}");
    assert!(Index::parse(&trailing, PREFIX).is_err());
    let good = String::from_utf8(bytes(&sample())).unwrap();
    let dup_key = good.replacen("\"sequence\": 10,", "\"sequence\": 10, \"sequence\": 11,", 1);
    assert_ne!(dup_key, good);
    assert!(Index::parse(dup_key.as_bytes(), PREFIX).is_err());
    let dup_id = index_with(vec![entry("hello"), entry("hello")], json!([]));
    assert!(Index::parse(&bytes(&dup_id), PREFIX).is_err());

    let cases: Vec<(&str, Value)> = vec![
        ("id", json!("Bad_Id")),
        ("version", json!("1.2")),
        ("version", json!("01.2.3")),
        ("api", json!("x")),
        ("sha256", json!("0123")),
        ("sha256", json!("0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef")),
        ("url", json!("https://elsewhere.test/hello.aiop")),
        ("url", json!(format!("{PREFIX}hello 1.aiop"))),
        ("size", json!(0)),
        ("size", json!(-1)),
        ("name", json!("")),
        ("name", json!("Evil\u{202E}name")),
        ("description", json!("x".repeat(5000))),
    ];
    for (field, value) in cases {
        let mut v = sample();
        v["plugins"][0][field] = value.clone();
        assert!(Index::parse(&bytes(&v), PREFIX).is_err(), "{field} = {value}");
    }

    let mut backwards = sample();
    backwards["expires"] = json!("2026-11-12T10:00:00Z");
    assert!(Index::parse(&bytes(&backwards), PREFIX).is_err());
    // RFC 3339 needs a time-zone offset; chrono (like RFC 3339 section 5.6) also accepts a space as separator.
    for not_rfc3339 in ["2026-11-13T10:00:00", "2026-11-13", "13/11/2026 10:00:00Z", "2026-11-13T10:00:00+25:00"] {
        let mut v = sample();
        v["generated"] = json!(not_rfc3339);
        assert!(Index::parse(&bytes(&v), PREFIX).is_err(), "{not_rfc3339}");
    }

    for revoked in [
        json!([{ "id": "Bad_Id", "versions": ["*"], "reason": "r" }]),
        json!([{ "id": "hello", "versions": [], "reason": "r" }]),
        json!([{ "id": "hello", "versions": ["1.2"], "reason": "r" }]),
        json!([{ "id": "hello", "versions": ["1.2.3"], "reason": "a\u{0007}b" }]),
    ] {
        let v = index_with(vec![entry("hello")], revoked.clone());
        assert!(Index::parse(&bytes(&v), PREFIX).is_err(), "{revoked}");
    }
}

#[test]
fn bad_permission_refused() {
    for bad in [json!(["teleport"]), json!(["sql:read:"]), json!(["tcp:localhost:0"]), json!(["ssh", "ssh"]), json!([1])] {
        let mut v = sample();
        v["plugins"][0]["permissions"] = bad.clone();
        assert!(Index::parse(&bytes(&v), PREFIX).is_err(), "{bad}");
    }
    let mut none = sample();
    none["plugins"][0]["permissions"] = json!([]);
    assert!(Index::parse(&bytes(&none), PREFIX).is_ok());
}

#[test]
fn older_sequence_is_refused() {
    let index = Index::parse(&bytes(&sample()), PREFIX).unwrap();
    let now = at("2026-11-20T00:00:00Z");
    assert_eq!(check_fresh(&index, Some(11), now), Freshness::Rollback);
    assert_eq!(check_fresh(&index, Some(u64::MAX), now), Freshness::Rollback);
    assert_eq!(check_fresh(&index, Some(10), now), Freshness::Current);
    assert_eq!(check_fresh(&index, Some(9), now), Freshness::Current);
    assert_eq!(check_fresh(&index, None, now), Freshness::Current);
    // A rollback is reported even when the old index has also expired.
    assert_eq!(check_fresh(&index, Some(11), at("2027-01-01T00:00:00Z")), Freshness::Rollback);
}

#[test]
fn expired_is_reported() {
    let index = Index::parse(&bytes(&sample()), PREFIX).unwrap();
    assert_eq!(check_fresh(&index, None, at("2026-12-13T10:00:00Z")), Freshness::Current);
    assert_eq!(check_fresh(&index, None, at("2026-12-13T10:00:01Z")), Freshness::Expired);
    assert_eq!(check_fresh(&index, Some(10), at("2027-06-01T00:00:00Z")), Freshness::Expired);
}

#[test]
fn revocation_star_and_version() {
    let v = index_with(
        vec![entry("hello"), entry("other-plugin")],
        json!([
            { "id": "hello", "versions": ["1.0.0", "1.2.3"], "reason": "Leaks passwords" },
            { "id": "gone-plugin", "versions": ["*"], "reason": "Malware" },
        ]),
    );
    let index = Index::parse(&bytes(&v), PREFIX).unwrap();
    assert_eq!(index.revocation_for("hello", "1.2.3").unwrap().reason, "Leaks passwords");
    assert!(index.revocation_for("hello", "1.0.0").is_some());
    assert!(index.revocation_for("hello", "1.2.4").is_none());
    assert_eq!(index.revocation_for("gone-plugin", "9.9.9").unwrap().reason, "Malware");
    assert!(index.revocation_for("gone-plugin", "0.0.1").is_some());
    assert!(index.revocation_for("other-plugin", "1.2.3").is_none());
    assert!(index.revocation_for("gone", "1.2.3").is_none());
}

#[test]
fn limits_enforced() {
    assert_eq!(MAX_PLUGINS, 2000);
    assert_eq!(MAX_INDEX_BYTES, 4 * 1024 * 1024);
    let ids: Vec<String> = (0..=MAX_PLUGINS).map(|i| format!("plugin-{i}")).collect();

    let at_limit = index_with(ids[..MAX_PLUGINS].iter().map(|id| entry(id)).collect(), json!([]));
    assert_eq!(Index::parse(&bytes(&at_limit), PREFIX).unwrap().plugins.len(), MAX_PLUGINS);
    let over = index_with(ids.iter().map(|id| entry(id)).collect(), json!([]));
    let err = Index::parse(&bytes(&over), PREFIX).unwrap_err().to_string();
    assert!(err.contains("2000"), "{err}");

    // Exactly 4 MiB (valid JSON padded with whitespace) passes; one byte more is refused before parsing.
    let mut padded = bytes(&sample());
    padded.resize(MAX_INDEX_BYTES, b' ');
    assert!(Index::parse(&padded, PREFIX).is_ok());
    padded.push(b' ');
    assert_eq!(padded.len(), 4 * 1024 * 1024 + 1);
    let err = Index::parse(&padded, PREFIX).unwrap_err().to_string();
    assert!(err.contains("4 MiB"), "{err}");
}

fn with_url(url: &str) -> Value {
    let mut v = sample();
    v["plugins"][0]["url"] = json!(url);
    v
}

#[test]
fn url_traversal_refused() {
    const OFFICIAL: &str = "https://github.com/all-in-1-plugins/index/releases/download/";
    let official = |url: String| {
        let mut v = with_url(&url);
        v["plugins"][1]["url"] = json!(format!("{OFFICIAL}other-plugin-1.2.3/other-plugin.aiop"));
        Index::parse(&bytes(&v), OFFICIAL)
    };
    assert!(official(format!("{OFFICIAL}hello-1.2.3/hello.aiop")).is_ok());
    for bad in [
        format!("{OFFICIAL}../../../../attacker/repo/releases/download/hello-1.2.3/hello.aiop"),
        format!("{OFFICIAL}%2e%2e/%2e%2e/attacker/hello-1.2.3/hello.aiop"),
        format!("{OFFICIAL}hello-1.2.3/../hello-1.2.3/hello.aiop"),
        format!("{OFFICIAL}hello-1.2.3\\hello.aiop"),
        format!("{OFFICIAL}hello-1.2.3/hello.aiop?x=1"),
        format!("{OFFICIAL}hello-1.2.3/hello.aiop#x"),
        format!("{OFFICIAL}hello-1.2.3/hello.aiop "),
        format!("{OFFICIAL}hello-1.2.3//hello.aiop"),
        format!("{OFFICIAL}hello-1.2.3/hello%2eaiop"),
    ] {
        assert!(official(bad.clone()).is_err(), "{bad}");
    }
    // The prefix itself may not hide such characters either.
    for bad_prefix in ["https://example.test/a/../b/", "https://example.test/%2e%2e/", "https://example.test/a?b/", "https://example.test/a b/"] {
        let mut v = sample();
        for (i, id) in ["hello", "other-plugin"].iter().enumerate() {
            v["plugins"][i]["url"] = json!(format!("{bad_prefix}{id}-1.2.3/{id}.aiop"));
        }
        assert!(Index::parse(&bytes(&v), bad_prefix).is_err(), "{bad_prefix}");
    }
}

#[test]
fn url_must_match_id_and_version() {
    assert!(Index::parse(&bytes(&with_url(&format!("{PREFIX}hello-1.2.3/hello.aiop"))), PREFIX).is_ok());
    for bad in [
        format!("{PREFIX}other-plugin-1.2.3/other-plugin.aiop"),
        format!("{PREFIX}hello-1.2.4/hello.aiop"),
        format!("{PREFIX}hello-1.2.3/other.aiop"),
        format!("{PREFIX}hello-1.2.3/hello.zip"),
        format!("{PREFIX}hello-1.2.3/hello.aiop/"),
        format!("{PREFIX}x/hello-1.2.3/hello.aiop"),
        format!("{PREFIX}HELLO-1.2.3/hello.aiop"),
        PREFIX.to_string(),
        "https://elsewhere.test/download/hello-1.2.3/hello.aiop".to_string(),
    ] {
        assert!(Index::parse(&bytes(&with_url(&bad)), PREFIX).is_err(), "{bad}");
    }
}

#[test]
fn prefix_without_slash_refused() {
    let base = "http://127.0.0.1:8080";
    let mut v = sample();
    for (i, id) in ["hello", "other-plugin"].iter().enumerate() {
        v["plugins"][i]["url"] = json!(format!("{base}/{id}-1.2.3/{id}.aiop"));
    }
    assert!(Index::parse(&bytes(&v), &format!("{base}/")).is_ok());
    assert!(Index::parse(&bytes(&v), base).is_err());
    // The attack the trailing slash prevents: a host that merely starts with the prefix.
    let mut evil = sample();
    evil["plugins"][0]["url"] = json!(format!("{base}.evil.test/hello-1.2.3/hello.aiop"));
    assert!(Index::parse(&bytes(&evil), base).is_err());
    // Refused even when the index lists no plugins.
    let empty = index_with(vec![], json!([]));
    assert!(Index::parse(&bytes(&empty), &format!("{base}/")).is_ok());
    assert!(Index::parse(&bytes(&empty), base).is_err());
    assert!(Index::parse(&bytes(&empty), "").is_err());
}

#[test]
fn verify_and_parse_uses_the_same_bytes() {
    let data = bytes(&sample());
    let sig = sign(&data, &SECRET_A);
    let (id, public) = public_b64(&SECRET_A);
    let trusted = [(id.as_str(), public.as_str())];
    let (index, key) = verify_and_parse(&data, &sig, &trusted, PREFIX).unwrap();
    assert_eq!(key.to_string(), id);
    assert_eq!(index, Index::parse(&data, PREFIX).unwrap());
    // A bad signature refuses before parsing; a signed but invalid index refuses after verifying.
    let mut tampered = data.clone();
    tampered[5] ^= 1;
    assert!(verify_and_parse(&tampered, &sig, &trusted, PREFIX).is_err());
    let invalid = bytes(&with_url("https://elsewhere.test/hello-1.2.3/hello.aiop"));
    assert!(verify_and_parse(&invalid, &sign(&invalid, &SECRET_A), &trusted, PREFIX).is_err());
}

#[test]
fn size_cap_is_the_package_cap() {
    assert_eq!(MAX_PACKAGE_BYTES, aio_plugin_host::package::MAX_PACKAGE as u64);
    let mut at_cap = sample();
    at_cap["plugins"][0]["size"] = json!(MAX_PACKAGE_BYTES);
    assert!(Index::parse(&bytes(&at_cap), PREFIX).is_ok());
    at_cap["plugins"][0]["size"] = json!(MAX_PACKAGE_BYTES + 1);
    assert!(Index::parse(&bytes(&at_cap), PREFIX).is_err());
}

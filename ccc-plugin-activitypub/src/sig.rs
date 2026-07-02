//! HTTP Signatures (draft-cavage v10) — both producer and consumer.
//!
//! `sign_post` builds outbound `Signature:` headers; `verify_rsa_sha256`
//! checks an incoming one against a public key fetched from the sender's
//! actor URL. `body_digest` and `http_date` are utilities used by both
//! sides. Signing/verification are RSA-SHA256 only — that's all the
//! Fediverse uses in practice.

use base64::Engine as _;
use rsa::pkcs1v15::{Signature, SigningKey, VerifyingKey};
use rsa::sha2::Sha256;
use rsa::signature::{RandomizedSigner, SignatureEncoding, Verifier};
use sha2::Digest;

/// Compute the SHA-256 digest header value for a request body.
/// Returns a string suitable for `Digest:` — e.g. `SHA-256=base64`.
pub fn body_digest(body: &[u8]) -> String {
    let hash = sha2::Sha256::digest(body);
    let b64 = base64::engine::general_purpose::STANDARD.encode(hash);
    format!("SHA-256={}", b64)
}

/// Check a received `Digest:` header value against the actual body.
///
/// RFC 3230 allows a comma-separated list of `algorithm=value` pairs
/// and peers are free to send algorithms we don't implement. Returns:
/// - `Some(true)`  — at least one supported entry, and it matches
/// - `Some(false)` — a supported entry is present but wrong (reject!)
/// - `None`        — no supported algorithm in the header; the caller
///   decides (we accept rather than 401 legitimate peers we simply
///   can't check — the signature still covers the raw header line).
///
/// SHA-256 and SHA-512 are supported; tokens match case-insensitively
/// because both `SHA-256=` and `sha-256=` show up in the wild.
pub fn digest_check(provided: &str, body: &[u8]) -> Option<bool> {
    let mut verdict: Option<bool> = None;
    for entry in provided.split(',') {
        // split_once takes the FIRST '=', so base64 padding at the end
        // of the value survives intact.
        let Some((algo, b64)) = entry.trim().split_once('=') else {
            continue;
        };
        let algo = algo.trim();
        let expected = if algo.eq_ignore_ascii_case("sha-256") {
            base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(body))
        } else if algo.eq_ignore_ascii_case("sha-512") {
            base64::engine::general_purpose::STANDARD.encode(sha2::Sha512::digest(body))
        } else {
            continue;
        };
        if b64.trim() == expected {
            return Some(true);
        }
        verdict = Some(false);
    }
    verdict
}

/// Strict form of [`digest_check`]: true only when a supported digest
/// entry is present AND matches the body.
pub fn digest_matches(provided: &str, body: &[u8]) -> bool {
    digest_check(provided, body) == Some(true)
}

/// Sign an outbound POST. Returns the value to put in the `Signature:`
/// header. `key_id` is the URL of the public key (e.g.
/// `https://example.com/users/station#main-key`).
///
/// Headers signed: `(request-target) host date digest`.
pub fn sign_post(
    private_key_pem: &str,
    key_id: &str,
    target_path: &str,
    host: &str,
    date: &str,
    digest_header: &str,
) -> Result<String, String> {
    use rsa::pkcs8::DecodePrivateKey;
    let private_key = rsa::RsaPrivateKey::from_pkcs8_pem(private_key_pem)
        .map_err(|e| format!("private key parse: {}", e))?;
    let signing_key = SigningKey::<Sha256>::new(private_key);

    let signing_string = format!(
        "(request-target): post {}\nhost: {}\ndate: {}\ndigest: {}",
        target_path.to_lowercase(),
        host.to_lowercase(),
        date,
        digest_header
    );

    let mut rng = rand::thread_rng();
    let signature = signing_key.sign_with_rng(&mut rng, signing_string.as_bytes());
    let b64 = base64::engine::general_purpose::STANDARD.encode(signature.to_bytes());

    Ok(format!(
        "keyId=\"{}\",algorithm=\"rsa-sha256\",headers=\"(request-target) host date digest\",signature=\"{}\"",
        key_id, b64
    ))
}

/// Parsed pieces of an HTTP-Signature header.
#[derive(Debug, Clone)]
pub struct ParsedSignature {
    pub key_id: String,
    /// Optional: senders may declare `algorithm="rsa-sha256"` etc. We
    /// hard-code RSA-SHA256 verification regardless, so we only parse
    /// this for diagnostic logging if needed later.
    #[allow(dead_code)]
    pub algorithm: Option<String>,
    pub headers: Vec<String>,
    pub signature_b64: String,
}

/// Parse the comma-separated `key="value"` syntax of the `Signature:` header.
pub fn parse_signature_header(header: &str) -> Option<ParsedSignature> {
    let mut key_id = None;
    let mut algorithm = None;
    let mut headers = None;
    let mut signature_b64 = None;
    for kv in split_signature_params(header) {
        let (k, v) = kv;
        match k {
            "keyId" => key_id = Some(v.to_string()),
            "algorithm" => algorithm = Some(v.to_string()),
            "headers" => headers = Some(v.split_whitespace().map(|s| s.to_string()).collect()),
            "signature" => signature_b64 = Some(v.to_string()),
            _ => {}
        }
    }
    Some(ParsedSignature {
        key_id: key_id?,
        algorithm,
        headers: headers.unwrap_or_else(|| vec!["date".into()]),
        signature_b64: signature_b64?,
    })
}

fn split_signature_params(header: &str) -> Vec<(&str, &str)> {
    // Sample input: keyId="X",algorithm="rsa-sha256",headers="(request-target) host date digest",signature="..."
    let mut out = Vec::new();
    let mut rest = header.trim();
    while !rest.is_empty() {
        // key=
        let eq = match rest.find('=') {
            Some(i) => i,
            None => break,
        };
        let key = rest[..eq].trim();
        rest = &rest[eq + 1..];
        if !rest.starts_with('"') {
            break;
        }
        let val_start = 1;
        let val_end = match rest[val_start..].find('"') {
            Some(i) => i + val_start,
            None => break,
        };
        let value = &rest[val_start..val_end];
        out.push((key, value));
        rest = &rest[val_end + 1..];
        rest = rest.trim_start_matches(|c: char| c == ',' || c.is_whitespace());
    }
    out
}

/// Reconstruct the signing string from a request's headers.
///
/// `header_names` should be lowercased (as supplied by the parsed
/// `headers="..."` field). `(request-target)` is special — substitute
/// `<method> <path>`. `host`, `date`, `digest` come from `headers`.
pub fn build_signing_string(
    header_names: &[String],
    method: &str,
    target_path: &str,
    headers: &axum::http::HeaderMap,
) -> Result<String, String> {
    let mut lines = Vec::with_capacity(header_names.len());
    for name in header_names {
        let n = name.to_ascii_lowercase();
        let line = if n == "(request-target)" {
            format!(
                "(request-target): {} {}",
                method.to_lowercase(),
                target_path
            )
        } else {
            let value = headers
                .get(n.as_str())
                .ok_or_else(|| format!("missing required header: {}", n))?
                .to_str()
                .map_err(|_| format!("header {} not ascii", n))?;
            format!("{}: {}", n, value)
        };
        lines.push(line);
    }
    Ok(lines.join("\n"))
}

/// Verify the supplied base64 RSA-SHA256 signature against the
/// reconstructed signing string, using a PEM-encoded SPKI public key.
pub fn verify_rsa_sha256(
    public_key_pem: &str,
    signing_string: &str,
    signature_b64: &str,
) -> Result<(), String> {
    use rsa::pkcs8::DecodePublicKey;
    let public_key = rsa::RsaPublicKey::from_public_key_pem(public_key_pem)
        .map_err(|e| format!("public key parse: {}", e))?;
    let verifying_key: VerifyingKey<Sha256> = VerifyingKey::new(public_key);
    let sig_bytes = base64::engine::general_purpose::STANDARD
        .decode(signature_b64.trim())
        .map_err(|e| format!("signature base64: {}", e))?;
    let sig =
        Signature::try_from(sig_bytes.as_slice()).map_err(|e| format!("signature parse: {}", e))?;
    verifying_key
        .verify(signing_string.as_bytes(), &sig)
        .map_err(|e| format!("signature verify: {}", e))
}

/// Format a Unix timestamp as the `Date:` header per RFC 7231 / RFC 5322.
pub fn http_date(secs: u64) -> String {
    let (y, mo, d, h, mi, s, weekday) = unix_to_ymdhms_weekday(secs);
    let weekday_str = match weekday {
        0 => "Sun",
        1 => "Mon",
        2 => "Tue",
        3 => "Wed",
        4 => "Thu",
        5 => "Fri",
        6 => "Sat",
        _ => "???",
    };
    let month_str = match mo {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        12 => "Dec",
        _ => "???",
    };
    format!(
        "{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
        weekday_str, d, month_str, y, h, mi, s
    )
}

fn unix_to_ymdhms_weekday(secs: u64) -> (u32, u32, u32, u32, u32, u32, u32) {
    let days_since_epoch = (secs / 86_400) as u32;
    let s = (secs % 60) as u32;
    let mi = ((secs / 60) % 60) as u32;
    let h = ((secs / 3600) % 24) as u32;
    // Jan 1 1970 was a Thursday (4).
    let weekday = (4 + days_since_epoch) % 7;

    let mut year = 1970u32;
    let mut days_remaining = days_since_epoch;
    loop {
        let in_year = if is_leap(year) { 366 } else { 365 };
        if days_remaining < in_year {
            break;
        }
        days_remaining -= in_year;
        year += 1;
    }
    let months_normal = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let months_leap = [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let months = if is_leap(year) {
        months_leap
    } else {
        months_normal
    };
    let mut month = 1u32;
    for &len in &months {
        if days_remaining < len {
            break;
        }
        days_remaining -= len;
        month += 1;
    }
    let day = days_remaining + 1;
    (year, month, day, h, mi, s, weekday)
}

fn is_leap(y: u32) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2048-bit RSA fixture keypair, generated once with openssl and
    // frozen here so the signature tests are deterministic and don't
    // pay keygen cost on every run. Test-only — never use these keys
    // for anything real.
    const TEST_PRIVATE_PEM: &str = "-----BEGIN PRIVATE KEY-----
MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQCxGFYt7wEoDcaI
Z2FsFepf+EElsnEfY20xSFzE3nfu/sjzzPgY6bf6sU5wDfVCD9soSiRYT4TiUMAE
ugoO6AIPC3XztbCxQRT4v0w8F3Y++teWVVQlEShbgwBA3r53KTRoyemc4bi+SibV
/knIxUJv8vBc/KjR4jPby/Tkk/skVLQC9dEBKHpWJpCjs0GNAU3E6TFa41++dWAE
cwU1wpoV59NJ0JqbbAc1oLtLtkVSl9N+WbjSypdqqvfR1FGZd+Ub/ig+T/2FsCYU
7yeroj1dlxXxCUtiIHQHVr4a4ErQPJp1bDWWr7DGMwx2b9KWB5/rsYil54SX69hx
9qalEkzZAgMBAAECggEAHfX3koLWZ7/Y4DfZQA9/HOhjoSyMvdo03XsYEshucvUQ
60o4iQJ1eUio9AUwgpIrKihM+NHPMfimW8ZbsETpIEF4nZgWAg74LNM0YxA3FZWb
hpf+Cd1avgyuLMtiXseOUdIjH0MC3dKuulJwijJxOH5Z0ul96DBERMG/NzKf8PfI
5dre56p7sCYuGkqkYoAta0Enzld2z/dZU5z7xRcXZUhcxWe1OtWKw+XTfmU57+ba
XFD0QTE6CtnyG6GSsPECbR4HKirjbpFAVAZ1a2PxwG3rwV3AczntoJR5TEI7+Q14
quDG0zrUNrrn08ltPScav5UT47RswY9tCHv8SJRP5wKBgQDy/OozaaQ3ATqwjNYK
Ip4K0IePtiRf49olwaIPpgrjpcGvOZGTSYLkgET+guDLx4Cali9/SIsNdaTYkJjE
bX/QSXmi0E//KXdZYw6+pwzQIL+r28aeHaP3VNEZv4oglraMQ3bPndy0JPefXTU+
/3PYKHRvPPwWkUDC6Qx71FbiBwKBgQC6lBsu4/eVr/bw1Fyp/eQK60Vha98Z6VtG
kX8iYCVUDARt+he9S6K/C751DZ9ncEQc6FG9dL29AVeatdQGMVgcn55uouJeNyvW
ZbvZl5VXtcQ2KSDg8PzL4ZBM6cK/WbAhU/JtO7uTaV2J94wsNH+gN+N9wotMo70v
DOE8tTgiHwKBgQChyHla3+ShZuUftbWxK9lUVAFeUySm37tYVjND/gbUCYIK2+as
TU6kOy800glARMXAp2xab/99Z63pL2U3q7xk/vbIy6/nsUEwt76SfAEQ9i0IDR2S
GyQXwlYysZATIKXMgAK0O2Q+YTqp1dJk7orACjdz9QPXNsEKxUY1kGyEswKBgQCP
h8Ahol2kWWmueq+OVjJH9BW7jDBkzo/ZCbSzzSLqNRRzRiFdDgJ01aP+iCxIdqTv
WAkJpfUQoSezbI1e9jbCTkii88sQj4C5vil0ipfokXwP7jPM15ahgTRw9qNKcMR9
3wZv4bO7AyNiL301xG0p49StuitPGec/erkK6p30IQKBgHdUKGgEyq7Nu56EkSjA
/C2+KnHD5X5NsdiBEgam7lp3cfKJUnoXCruDrklBoksnK7HpfXvDlTgs6IbuC83j
gWamBrmm3c1jr6uv6/eh9471k+62BbgUChIHcGUn9NEmgs4xQkPmewYW/vYdpYCu
aOW+dslKjuIucaoXrhmMT784
-----END PRIVATE KEY-----
";

    const TEST_PUBLIC_PEM: &str = "-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAsRhWLe8BKA3GiGdhbBXq
X/hBJbJxH2NtMUhcxN537v7I88z4GOm3+rFOcA31Qg/bKEokWE+E4lDABLoKDugC
Dwt187WwsUEU+L9MPBd2PvrXllVUJREoW4MAQN6+dyk0aMnpnOG4vkom1f5JyMVC
b/LwXPyo0eIz28v05JP7JFS0AvXRASh6ViaQo7NBjQFNxOkxWuNfvnVgBHMFNcKa
FefTSdCam2wHNaC7S7ZFUpfTflm40sqXaqr30dRRmXflG/4oPk/9hbAmFO8nq6I9
XZcV8QlLYiB0B1a+GuBK0DyadWw1lq+wxjMMdm/Slgef67GIpeeEl+vYcfampRJM
2QIDAQAB
-----END PUBLIC KEY-----
";

    // A *different* keypair's public key — verification against this
    // must fail even for a perfectly well-formed signature.
    const OTHER_PUBLIC_PEM: &str = "-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAsSoksXns1XeEanqaUJrr
dhW6J7SZDAq4EV8DseSBfXkKhbxG95Hmzgmf+wEtJG5cFLhvGSrKGiO/llss3BZL
/xss4FP7OA05htoId4zrWYAVxJRJGVIsXwOHkDDpnQI/O6XrYaQ3rprlYrEpC283
PtDU2DoqXneZTxFYTH1V70fC+727IY5Qrzj0Eo9AT3POtbRco7rJiTPIxZuDsLCm
aa+iBUybjvtWNsn5Z5Lp/De6+3rGVVUQNp5HKNQkEw4CbUKtWdypmgZDMMTSPY7q
jpYMpWRypoEAYb8X7NrFa7KdhBoA732wVa6O2FHMhBLsjWigAEPAmWpOD7T788cQ
xwIDAQAB
-----END PUBLIC KEY-----
";

    const TEST_KEY_ID: &str = "https://station.example/users/station#main-key";

    #[test]
    fn parse_signature_header_full() {
        let header = r#"keyId="https://a.example/u/x#main-key",algorithm="rsa-sha256",headers="(request-target) host date digest",signature="c2ln""#;
        let parsed = parse_signature_header(header).expect("parses");
        assert_eq!(parsed.key_id, "https://a.example/u/x#main-key");
        assert_eq!(parsed.algorithm.as_deref(), Some("rsa-sha256"));
        assert_eq!(
            parsed.headers,
            vec!["(request-target)", "host", "date", "digest"]
        );
        assert_eq!(parsed.signature_b64, "c2ln");
    }

    #[test]
    fn parse_signature_header_defaults_headers_to_date() {
        // Per draft-cavage, an absent `headers` field means just "date".
        let header = r#"keyId="k",signature="s""#;
        let parsed = parse_signature_header(header).expect("parses");
        assert_eq!(parsed.headers, vec!["date"]);
    }

    #[test]
    fn parse_signature_header_rejects_missing_required_fields() {
        // No keyId.
        assert!(parse_signature_header(r#"signature="s""#).is_none());
        // No signature.
        assert!(parse_signature_header(r#"keyId="k""#).is_none());
        // Garbage.
        assert!(parse_signature_header("not a signature header").is_none());
    }

    /// Full producer→consumer round trip: sign_post builds the header,
    /// parse_signature_header + build_signing_string reconstruct the
    /// signing string on the receiving side, and verify_rsa_sha256
    /// accepts it with the matching public key.
    #[test]
    fn sign_then_verify_round_trip() {
        let body: &[u8] = br#"{"type":"Follow","actor":"https://b.example/u/y"}"#;
        let digest = body_digest(body);
        let date = http_date(1_705_322_096);
        let host = "station.example";
        let path = "/users/station/inbox";

        let header =
            sign_post(TEST_PRIVATE_PEM, TEST_KEY_ID, path, host, &date, &digest).expect("signs");
        let parsed = parse_signature_header(&header).expect("parses own header");
        assert_eq!(parsed.key_id, TEST_KEY_ID);

        // Reconstruct the signing string the way the inbox handler does,
        // from an axum HeaderMap.
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("host", host.parse().unwrap());
        headers.insert("date", date.parse().unwrap());
        headers.insert("digest", digest.parse().unwrap());
        let signing_string =
            build_signing_string(&parsed.headers, "post", path, &headers).expect("builds");

        verify_rsa_sha256(TEST_PUBLIC_PEM, &signing_string, &parsed.signature_b64)
            .expect("valid signature verifies");
    }

    #[test]
    fn verify_rejects_tampered_signing_string() {
        let body: &[u8] = b"{}";
        let digest = body_digest(body);
        let date = http_date(1_705_322_096);
        let header = sign_post(
            TEST_PRIVATE_PEM,
            TEST_KEY_ID,
            "/users/station/inbox",
            "station.example",
            &date,
            &digest,
        )
        .expect("signs");
        let parsed = parse_signature_header(&header).expect("parses");

        // Signature was made over the inbox path; verify against a
        // different path — an attacker replaying the request elsewhere.
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("host", "station.example".parse().unwrap());
        headers.insert("date", date.parse().unwrap());
        headers.insert("digest", digest.parse().unwrap());
        let signing_string =
            build_signing_string(&parsed.headers, "post", "/users/other/inbox", &headers)
                .expect("builds");
        assert!(verify_rsa_sha256(TEST_PUBLIC_PEM, &signing_string, &parsed.signature_b64).is_err());
    }

    #[test]
    fn verify_rejects_wrong_public_key() {
        // Sign, then reconstruct the exact signing string sign_post
        // used, so the ONLY difference between the two assertions
        // below is which key we verify with.
        let header = sign_post(TEST_PRIVATE_PEM, TEST_KEY_ID, "/x", "h", "d", "SHA-256=x")
            .expect("signs");
        let parsed = parse_signature_header(&header).expect("parses");
        let signing_string = "(request-target): post /x\nhost: h\ndate: d\ndigest: SHA-256=x";
        // Sanity: the matching key accepts it...
        verify_rsa_sha256(TEST_PUBLIC_PEM, signing_string, &parsed.signature_b64)
            .expect("own key verifies");
        // ...and a different actor's key does not.
        assert!(verify_rsa_sha256(OTHER_PUBLIC_PEM, signing_string, &parsed.signature_b64).is_err());
    }

    #[test]
    fn verify_rejects_bad_base64_and_bad_pem() {
        assert!(verify_rsa_sha256(TEST_PUBLIC_PEM, "x", "!!!not-base64!!!").is_err());
        assert!(verify_rsa_sha256("not a pem", "x", "c2ln").is_err());
    }

    #[test]
    fn body_digest_known_vector() {
        // SHA-256 of the empty string, base64-encoded.
        assert_eq!(
            body_digest(b""),
            "SHA-256=47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU="
        );
    }

    #[test]
    fn digest_matches_accepts_correct_and_case_insensitive_algo() {
        let body = b"hello world";
        let d = body_digest(body);
        assert!(digest_matches(&d, body));
        // Lowercase algorithm token (some servers send this).
        let lower = d.replacen("SHA-256", "sha-256", 1);
        assert!(digest_matches(&lower, body));
    }

    #[test]
    fn digest_matches_rejects_wrong_body_algo_and_garbage() {
        let d = body_digest(b"hello world");
        assert!(!digest_matches(&d, b"tampered body"));
        assert!(!digest_matches("MD5=abcd", b"hello world"));
        assert!(!digest_matches("garbage-no-equals", b"hello world"));
        assert!(!digest_matches("", b"hello world"));
    }

    #[test]
    fn digest_check_tristate() {
        let body = b"hello world";
        let good = body_digest(body);
        // Supported + correct.
        assert_eq!(digest_check(&good, body), Some(true));
        // Supported + wrong body: hard mismatch.
        assert_eq!(digest_check(&good, b"tampered"), Some(false));
        // Only unsupported algorithms: undecidable, caller's choice.
        assert_eq!(digest_check("MD5=abcd", body), None);
        assert_eq!(digest_check("unicorn-1=xyz", body), None);
        assert_eq!(digest_check("", body), None);
        // RFC 3230 list: unsupported entry + supported correct entry.
        let list = format!("MD5=abcd,{}", good);
        assert_eq!(digest_check(&list, body), Some(true));
        // List where the only supported entry is wrong.
        let wrong = body_digest(b"other").replacen("SHA-256=", "sha-256=", 1);
        let list = format!("MD5=abcd,{}", wrong);
        assert_eq!(digest_check(&list, body), Some(false));
        // SHA-512 support.
        let sha512 = format!(
            "SHA-512={}",
            base64::engine::general_purpose::STANDARD.encode(sha2::Sha512::digest(body))
        );
        assert_eq!(digest_check(&sha512, body), Some(true));
    }

    #[test]
    fn http_date_known_timestamps() {
        // 2024-01-15 12:34:56 UTC == 1705322096. Expected: "Mon, 15 Jan 2024 12:34:56 GMT"
        assert_eq!(http_date(1_705_322_096), "Mon, 15 Jan 2024 12:34:56 GMT");
        // Unix epoch
        assert_eq!(http_date(0), "Thu, 01 Jan 1970 00:00:00 GMT");
    }

    #[test]
    fn http_date_leap_day() {
        // 2024-02-29 00:00:00 UTC == 1709164800. Expected: "Thu, 29 Feb 2024 00:00:00 GMT"
        assert_eq!(http_date(1_709_164_800), "Thu, 29 Feb 2024 00:00:00 GMT");
    }

    #[test]
    fn http_date_century_boundary() {
        // 2100-03-01 00:00:00 UTC == 4_107_542_400. Monday.
        // (2100 is NOT a leap year, even though it's divisible by 4.
        //  This is the exact bug the audit warned about — if the
        //  date math regressed, the output month would be "Mar"
        //  without the 28-day February, vs the buggy "Feb 29".)
        assert_eq!(http_date(4_107_542_400), "Mon, 01 Mar 2100 00:00:00 GMT");
        // 2100-01-01 00:00:00 UTC == 4_102_444_800. Friday.
        assert_eq!(http_date(4_102_444_800), "Fri, 01 Jan 2100 00:00:00 GMT");
    }
}

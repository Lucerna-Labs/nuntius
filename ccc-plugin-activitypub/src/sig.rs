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
        _ => "Sat",
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
        _ => "Dec",
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

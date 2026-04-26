//! HTTP Signatures (draft-cavage v10) signer for outbound ActivityPub
//! deliveries. We implement the producer side only — verifying incoming
//! signatures is a follow-up. v1 trusts what comes in for the sake of
//! shipping; in practice most Fediverse servers refuse to deliver
//! activities to actors that don't present a signed `Accept`, so the
//! producer side is what actually unblocks federation.

use base64::Engine as _;
use rsa::pkcs1v15::SigningKey;
use rsa::sha2::Sha256;
use rsa::signature::{RandomizedSigner, SignatureEncoding};
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

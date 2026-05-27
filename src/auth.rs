//! CryptoJS-compatible AES-256-CBC password "encryption" used by the NanoKVM
//! login API.
//!
//! The frontend wraps the password with `CryptoJS.AES.encrypt(passphrase,
//! password)`, which uses OpenSSL's `EVP_BytesToKey` (MD5-based) and emits
//! `Salted__ || salt || ciphertext`, base64-encoded then URL-percent-encoded.
//! That's the exact wire format we reproduce here.
//!
//! This is weird, bad, and useless, but we have to do what the API expects.

use aes::Aes256;
use aes::cipher::BlockModeEncrypt;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use cbc::cipher::{KeyIvInit, block_padding::Pkcs7};
use md5::{Digest, Md5};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use rand::RngExt;

type Aes256CbcEnc = cbc::Encryptor<Aes256>;

const NANOKVM_PASSPHRASE: &[u8] = b"nanokvm-sipeed-2024";

/// Encrypt a password for the NanoKVM `/api/auth/login` endpoint.
pub fn encrypt_password(password: &str) -> String {
    let salt: [u8; 8] = rand::rng().random();
    encrypt_password_with_salt(password, &salt)
}

/// Same as [`encrypt_password`] but with a caller-supplied salt — used by
/// tests against known vectors.
fn encrypt_password_with_salt(password: &str, salt: &[u8; 8]) -> String {
    let (key, iv) = evp_bytes_to_key(NANOKVM_PASSPHRASE, salt);
    let ciphertext = Aes256CbcEnc::new(&key.into(), &iv.into())
        .encrypt_padded_vec::<Pkcs7>(password.as_bytes());

    let mut openssl_data = Vec::with_capacity(8 + 8 + ciphertext.len());
    openssl_data.extend_from_slice(b"Salted__");
    openssl_data.extend_from_slice(salt);
    openssl_data.extend_from_slice(&ciphertext);

    let b64 = B64.encode(&openssl_data);
    utf8_percent_encode(&b64, NON_ALPHANUMERIC).to_string()
}

/// OpenSSL's `EVP_BytesToKey` with MD5 — matches CryptoJS's default KDF.
/// Hardcoded to AES-256 + 16-byte IV (48 bytes total).
fn evp_bytes_to_key(password: &[u8], salt: &[u8]) -> ([u8; 32], [u8; 16]) {
    let mut out = Vec::with_capacity(48);
    let mut prev: Vec<u8> = Vec::new();
    while out.len() < 48 {
        let mut h = Md5::new();
        h.update(&prev);
        h.update(password);
        h.update(salt);
        prev = h.finalize().to_vec();
        out.extend_from_slice(&prev);
    }
    let mut key = [0u8; 32];
    let mut iv = [0u8; 16];
    key.copy_from_slice(&out[..32]);
    iv.copy_from_slice(&out[32..48]);
    (key, iv)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Vector generated from the Python reference implementation:
    //   salt     = 01 02 03 04 05 06 07 08
    //   password = "admin"
    const FIXED_SALT: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
    const EXPECTED_KEY_HEX: &str =
        "42c3fe5fe864ee325657646ae8c9d363ba0b3496e5a88c44ba6ce6eea6c952fe";
    const EXPECTED_IV_HEX: &str = "116c9a4f3b5668e5f13c47b174267d36";
    const EXPECTED_URL: &str = "U2FsdGVkX18BAgMEBQYHCDqqQ2txUJEymiuPNgygcGQ%3D";

    #[test]
    fn evp_bytes_to_key_matches_reference() {
        let (key, iv) = evp_bytes_to_key(NANOKVM_PASSPHRASE, &FIXED_SALT);
        assert_eq!(hex::encode(key), EXPECTED_KEY_HEX);
        assert_eq!(hex::encode(iv), EXPECTED_IV_HEX);
    }

    #[test]
    fn encrypt_password_matches_reference_fixed_salt() {
        let got = encrypt_password_with_salt("admin", &FIXED_SALT);
        assert_eq!(got, EXPECTED_URL);
    }

    #[test]
    fn encrypt_password_random_salt_produces_url_safe_output() {
        let out = encrypt_password("hunter2");
        // No raw '+', '/', or '=' should leak through the percent-encoding.
        assert!(!out.contains('+'));
        assert!(!out.contains('/'));
        assert!(!out.contains('='));
        // Decoding the percent-encoding should give well-formed OpenSSL-format base64.
        let decoded = percent_encoding::percent_decode_str(&out)
            .decode_utf8()
            .unwrap();
        let bytes = B64.decode(decoded.as_ref()).unwrap();
        assert!(bytes.starts_with(b"Salted__"));
        // 8-byte magic + 8-byte salt + ciphertext padded to AES block size.
        assert!(bytes.len() >= 16 + 16);
        assert_eq!((bytes.len() - 16) % 16, 0);
    }

    #[test]
    fn encrypt_password_two_calls_differ() {
        // Random salt makes outputs distinct.
        assert_ne!(encrypt_password("admin"), encrypt_password("admin"));
    }
}

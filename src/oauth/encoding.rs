//! The crate's base64: standard encoding, and a lenient decoder that a mail
//! body's wrapped lines and truncated fetches need.

/// Standard base64, for inline `data:` URIs.
pub fn base64_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

/// Decode standard base64, skipping whitespace and `=` wherever they fall (a
/// MIME body wraps its lines) and a lone last character that cannot make a
/// byte. `None` if the input isn't valid (which for a truncated fetch is a
/// real possibility, not a bug).
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
    use base64::Engine;
    const LENIENT: GeneralPurpose = GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        GeneralPurposeConfig::new()
            .with_decode_padding_mode(DecodePaddingMode::RequireNone)
            .with_decode_allow_trailing_bits(true),
    );
    let mut clean: Vec<u8> = text
        .bytes()
        .filter(|b| !b.is_ascii_whitespace() && *b != b'=')
        .collect();
    if clean.len() % 4 == 1 {
        clean.pop();
    }
    LENIENT.decode(clean).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_is_as_forgiving_as_a_mail_body_needs() {
        let bytes: Vec<u8> = (0..=255).collect();
        assert_eq!(base64_decode(&base64_encode(&bytes)).unwrap(), bytes);
        assert_eq!(base64_encode(b"hi!?"), "aGkhPw==");
        assert_eq!(base64_decode("aGkh\r\nPw==").unwrap(), b"hi!?");
        assert_eq!(
            base64_decode("aGkhPw").unwrap(),
            b"hi!?",
            "padding is optional"
        );
        assert_eq!(
            base64_decode("aGkhP").unwrap(),
            b"hi!",
            "a lone last character is dropped"
        );
        assert_eq!(base64_decode("aG*h"), None);
    }
}

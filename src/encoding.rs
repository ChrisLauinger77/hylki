//! Text encodings the app writes and reads: base64 (with the lenient
//! decoder mail needs) and percent-encoding. One home for each, so every
//! module encodes the same way.

mod base64;
pub mod percent;

pub use self::base64::{base64_decode, base64_encode};

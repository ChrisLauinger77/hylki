//! NTLMv2 for HTTP (#391): the three messages an Exchange server's Windows
//! authentication asks for, after MS-NLMP.
//!
//! Only what HTTP needs: no signing or sealing, so no key exchange, and the
//! session key serves only the message integrity code (MIC). The channel
//! binding (MS-NLMP 3.1.5.2.2, RFC 5929 `tls-server-end-point`) ties the
//! sign-in to the TLS connection, which Exchange 2019 CU14 and later demand
//! by default (Extended Protection).

use hmac::{Hmac, Mac};
use md4::Md4;
use md5::{Digest, Md5};

type HmacMd5 = Hmac<Md5>;

const SIGNATURE: &[u8; 8] = b"NTLMSSP\0";

const NEGOTIATE_UNICODE: u32 = 0x0000_0001;
const REQUEST_TARGET: u32 = 0x0000_0004;
const NEGOTIATE_NTLM: u32 = 0x0000_0200;
const NEGOTIATE_ALWAYS_SIGN: u32 = 0x0000_8000;
const NEGOTIATE_EXTENDED_SESSIONSECURITY: u32 = 0x0008_0000;
const NEGOTIATE_TARGET_INFO: u32 = 0x0080_0000;
const NEGOTIATE_VERSION: u32 = 0x0200_0000;
const NEGOTIATE_128: u32 = 0x2000_0000;
const NEGOTIATE_56: u32 = 0x8000_0000;

/// What the client asks for: Unicode, NTLMv2-style session security, the
/// server's target information (which NTLMv2 needs), and no key exchange.
const CLIENT_FLAGS: u32 = NEGOTIATE_UNICODE
    | REQUEST_TARGET
    | NEGOTIATE_NTLM
    | NEGOTIATE_ALWAYS_SIGN
    | NEGOTIATE_EXTENDED_SESSIONSECURITY
    | NEGOTIATE_TARGET_INFO
    | NEGOTIATE_VERSION
    | NEGOTIATE_128
    | NEGOTIATE_56;

/// Windows 10, NTLM revision 15: what the version field conventionally says.
const VERSION: [u8; 8] = [10, 0, 0x61, 0x4a, 0, 0, 0, 15];

const AV_EOL: u16 = 0;
const AV_FLAGS: u16 = 6;
const AV_TIMESTAMP: u16 = 7;
const AV_TARGET_NAME: u16 = 9;
const AV_CHANNEL_BINDINGS: u16 = 10;
/// MsvAvFlags: the AUTHENTICATE message carries a MIC.
const AV_FLAG_MIC: u32 = 0x2;

/// One NTLM sign-in in progress, from the NEGOTIATE message to the
/// AUTHENTICATE one.
pub struct Handshake {
    negotiate: Vec<u8>,
}

/// A CHALLENGE message, read.
#[derive(Debug, PartialEq)]
pub struct Challenge {
    pub flags: u32,
    pub server_challenge: [u8; 8],
    pub target_info: Vec<u8>,
    raw: Vec<u8>,
}

/// Who signs in: `DOMAIN\user` splits into its parts; `user@domain` (a
/// UPN) goes whole, with no domain, as Windows clients send it.
pub fn split_user(login: &str) -> (String, String) {
    match login.split_once('\\') {
        Some((domain, user)) => (domain.to_string(), user.to_string()),
        None => (String::new(), login.to_string()),
    }
}

impl Handshake {
    /// The NEGOTIATE message, to send first.
    pub fn new() -> (Handshake, Vec<u8>) {
        let mut m = Vec::with_capacity(40);
        m.extend_from_slice(SIGNATURE);
        m.extend_from_slice(&1u32.to_le_bytes());
        m.extend_from_slice(&CLIENT_FLAGS.to_le_bytes());
        // Empty domain and workstation fields, pointing past the header.
        for _ in 0..2 {
            m.extend_from_slice(&0u16.to_le_bytes());
            m.extend_from_slice(&0u16.to_le_bytes());
            m.extend_from_slice(&40u32.to_le_bytes());
        }
        m.extend_from_slice(&VERSION);
        (Handshake { negotiate: m.clone() }, m)
    }

    /// The AUTHENTICATE message answering `challenge`. `channel_binding` is
    /// the `tls-server-end-point:` application data of the connection, when
    /// it is TLS; `spn` names the service, `HTTP/<host>`.
    pub fn authenticate(
        self,
        challenge: &Challenge,
        login: &str,
        password: &str,
        channel_binding: Option<&[u8]>,
        spn: &str,
    ) -> Vec<u8> {
        let mut client_challenge = [0u8; 8];
        let _ = crate::rng::fill(&mut client_challenge);
        let now = filetime_now();
        self.authenticate_with(challenge, login, password, channel_binding, spn, client_challenge, now)
    }

    #[allow(clippy::too_many_arguments)]
    fn authenticate_with(
        self,
        challenge: &Challenge,
        login: &str,
        password: &str,
        channel_binding: Option<&[u8]>,
        spn: &str,
        client_challenge: [u8; 8],
        now: u64,
    ) -> Vec<u8> {
        let (domain, user) = split_user(login);
        let server_time = av_pairs(&challenge.target_info)
            .find(|(id, _)| *id == AV_TIMESTAMP)
            .and_then(|(_, v)| v.try_into().ok().map(u64::from_le_bytes));
        let target_info = client_target_info(&challenge.target_info, server_time.is_some(), channel_binding, spn);
        let key = ntowfv2(password, &user, &domain);
        let (nt_response, session_base_key) = ntlmv2_response(
            &key,
            &challenge.server_challenge,
            &client_challenge,
            server_time.unwrap_or(now),
            &target_info,
        );
        // With the server's timestamp in hand the LM response is zeros
        // (MS-NLMP 3.1.5.1.2); otherwise LMv2.
        let lm_response = if server_time.is_some() {
            vec![0u8; 24]
        } else {
            let mut mac = <HmacMd5 as Mac>::new_from_slice(&key).expect("any key length");
            mac.update(&challenge.server_challenge);
            mac.update(&client_challenge);
            let mut lm = mac.finalize().into_bytes().to_vec();
            lm.extend_from_slice(&client_challenge);
            lm
        };
        let flags = challenge.flags & CLIENT_FLAGS | NEGOTIATE_UNICODE;
        let domain = utf16le(&domain);
        let user = utf16le(&user);
        let workstation: Vec<u8> = Vec::new();
        let session_key: Vec<u8> = Vec::new();

        const HEADER: usize = 88;
        let mut m = Vec::with_capacity(HEADER + nt_response.len() + 128);
        m.extend_from_slice(SIGNATURE);
        m.extend_from_slice(&3u32.to_le_bytes());
        let mut offset = HEADER as u32;
        let mut payload = Vec::new();
        for field in [&lm_response, &nt_response, &domain, &user, &workstation, &session_key] {
            m.extend_from_slice(&(field.len() as u16).to_le_bytes());
            m.extend_from_slice(&(field.len() as u16).to_le_bytes());
            m.extend_from_slice(&offset.to_le_bytes());
            offset += field.len() as u32;
            payload.extend_from_slice(field);
        }
        m.extend_from_slice(&flags.to_le_bytes());
        m.extend_from_slice(&VERSION);
        let mic_at = m.len();
        m.extend_from_slice(&[0u8; 16]);
        m.extend_from_slice(&payload);
        // The MIC covers all three messages, this one with the MIC zeroed.
        // Without key exchange the exported session key is the session
        // base key itself.
        let mut mac = <HmacMd5 as Mac>::new_from_slice(&session_base_key).expect("any key length");
        mac.update(&self.negotiate);
        mac.update(&challenge.raw);
        mac.update(&m);
        let mic = mac.finalize().into_bytes();
        m[mic_at..mic_at + 16].copy_from_slice(&mic);
        m
    }
}

impl Challenge {
    pub fn parse(raw: &[u8]) -> Option<Challenge> {
        if raw.len() < 48 || &raw[..8] != SIGNATURE || u32::from_le_bytes(raw[8..12].try_into().ok()?) != 2 {
            return None;
        }
        let flags = u32::from_le_bytes(raw[20..24].try_into().ok()?);
        let server_challenge: [u8; 8] = raw[24..32].try_into().ok()?;
        let len = u16::from_le_bytes(raw[40..42].try_into().ok()?) as usize;
        let at = u32::from_le_bytes(raw[44..48].try_into().ok()?) as usize;
        let target_info = raw.get(at..at.checked_add(len)?)?.to_vec();
        Some(Challenge { flags, server_challenge, target_info, raw: raw.to_vec() })
    }
}

/// The application data of a TLS channel binding (RFC 5929) from the
/// certificate hash the TLS library worked out for the connection.
pub fn tls_server_end_point(hash: &[u8]) -> Vec<u8> {
    let mut out = b"tls-server-end-point:".to_vec();
    out.extend_from_slice(hash);
    out
}

/// The target information the client sends back: the server's, with the
/// MIC flag (when there is a timestamp), the channel binding and the
/// service's name added.
fn client_target_info(server: &[u8], mic: bool, channel_binding: Option<&[u8]>, spn: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(server.len() + 64);
    let mut flags = 0u32;
    for (id, value) in av_pairs(server) {
        match id {
            AV_EOL => break,
            AV_FLAGS => flags = value.try_into().map(u32::from_le_bytes).unwrap_or(0),
            AV_CHANNEL_BINDINGS | AV_TARGET_NAME => {}
            _ => push_av(&mut out, id, value),
        }
    }
    if mic {
        flags |= AV_FLAG_MIC;
    }
    if flags != 0 {
        push_av(&mut out, AV_FLAGS, &flags.to_le_bytes());
    }
    push_av(&mut out, AV_CHANNEL_BINDINGS, &channel_binding_hash(channel_binding));
    if !spn.is_empty() {
        push_av(&mut out, AV_TARGET_NAME, &utf16le(spn));
    }
    push_av(&mut out, AV_EOL, &[]);
    out
}

/// MD5 of the `gss_channel_bindings_struct` with no addresses and the given
/// application data; sixteen zeros when there is none.
fn channel_binding_hash(application_data: Option<&[u8]>) -> [u8; 16] {
    let Some(data) = application_data else { return [0; 16] };
    let mut h = Md5::new();
    h.update([0u8; 16]);
    h.update((data.len() as u32).to_le_bytes());
    h.update(data);
    h.finalize().into()
}

fn push_av(out: &mut Vec<u8>, id: u16, value: &[u8]) {
    out.extend_from_slice(&id.to_le_bytes());
    out.extend_from_slice(&(value.len() as u16).to_le_bytes());
    out.extend_from_slice(value);
}

fn av_pairs(info: &[u8]) -> impl Iterator<Item = (u16, &[u8])> {
    let mut rest = info;
    std::iter::from_fn(move || {
        if rest.len() < 4 {
            return None;
        }
        let id = u16::from_le_bytes([rest[0], rest[1]]);
        let len = u16::from_le_bytes([rest[2], rest[3]]) as usize;
        let value = rest.get(4..4 + len)?;
        rest = &rest[4 + len..];
        Some((id, value))
    })
}

/// NTOWFv2: HMAC-MD5 under the password's MD4 of the upper-cased user name
/// and the domain, both UTF-16.
fn ntowfv2(password: &str, user: &str, domain: &str) -> [u8; 16] {
    let nt_hash = Md4::digest(utf16le(password));
    let mut mac = <HmacMd5 as Mac>::new_from_slice(&nt_hash).expect("any key length");
    mac.update(&utf16le(&user.to_uppercase()));
    mac.update(&utf16le(domain));
    mac.finalize().into_bytes().into()
}

/// The NTLMv2 response (NTProofStr followed by the client blob) and the
/// session base key.
fn ntlmv2_response(
    key: &[u8; 16],
    server_challenge: &[u8; 8],
    client_challenge: &[u8; 8],
    time: u64,
    target_info: &[u8],
) -> (Vec<u8>, [u8; 16]) {
    let mut blob = Vec::with_capacity(32 + target_info.len());
    blob.extend_from_slice(&[1, 1, 0, 0, 0, 0, 0, 0]);
    blob.extend_from_slice(&time.to_le_bytes());
    blob.extend_from_slice(client_challenge);
    blob.extend_from_slice(&[0; 4]);
    blob.extend_from_slice(target_info);
    blob.extend_from_slice(&[0; 4]);
    let mut mac = <HmacMd5 as Mac>::new_from_slice(key).expect("any key length");
    mac.update(server_challenge);
    mac.update(&blob);
    let proof = mac.finalize().into_bytes();
    let mut mac = <HmacMd5 as Mac>::new_from_slice(key).expect("any key length");
    mac.update(&proof);
    let session_base_key: [u8; 16] = mac.finalize().into_bytes().into();
    let mut response = proof.to_vec();
    response.extend_from_slice(&blob);
    (response, session_base_key)
}

fn utf16le(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// Now as a Windows FILETIME: tenths of a microsecond since 1601.
fn filetime_now() -> u64 {
    const EPOCH_GAP_SECS: u64 = 11_644_473_600;
    let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    (since.as_secs() + EPOCH_GAP_SECS) * 10_000_000 + u64::from(since.subsec_nanos() / 100)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    /// MS-NLMP 4.2.4: the NTLMv2 test vector.
    #[test]
    fn ntlmv2_matches_the_specification() {
        let key = ntowfv2("Password", "User", "Domain");
        assert_eq!(key.to_vec(), hex("0c868a403bfd7a93a3001ef22ef02e3f"));
        let target_info = hex(
            "02000c0044006f006d00610069006e00 01000c0053006500720076006500720000000000",
        );
        let server: [u8; 8] = hex("0123456789abcdef").try_into().unwrap();
        let client: [u8; 8] = hex("aaaaaaaaaaaaaaaa").try_into().unwrap();
        let (response, session) = ntlmv2_response(&key, &server, &client, 0, &target_info);
        assert_eq!(response[..16].to_vec(), hex("68cd0ab851e51c96aabc927bebef6a1c"));
        assert_eq!(session.to_vec(), hex("8de40ccadbc14a82f15cb0ad0de95ca3"));
    }

    fn challenge_with(target_info: &[u8]) -> Vec<u8> {
        let mut m = SIGNATURE.to_vec();
        m.extend_from_slice(&2u32.to_le_bytes());
        m.extend_from_slice(&[0, 0, 0, 0, 56, 0, 0, 0]);
        m.extend_from_slice(&(CLIENT_FLAGS | 0x4000_0000).to_le_bytes());
        m.extend_from_slice(&hex("0123456789abcdef"));
        m.extend_from_slice(&[0; 8]);
        m.extend_from_slice(&(target_info.len() as u16).to_le_bytes());
        m.extend_from_slice(&(target_info.len() as u16).to_le_bytes());
        m.extend_from_slice(&56u32.to_le_bytes());
        m.extend_from_slice(&VERSION);
        m.extend_from_slice(target_info);
        m
    }

    #[test]
    fn the_authenticate_message_carries_the_binding_and_the_mic() {
        let mut info = Vec::new();
        push_av(&mut info, 2, &utf16le("CORP"));
        push_av(&mut info, AV_TIMESTAMP, &1u64.to_le_bytes());
        push_av(&mut info, AV_EOL, &[]);
        let raw = challenge_with(&info);
        let c = Challenge::parse(&raw).unwrap();
        assert_eq!(c.server_challenge.to_vec(), hex("0123456789abcdef"));
        assert_eq!(c.target_info, info);
        let (h, neg) = Handshake::new();
        assert_eq!(neg.len(), 40);
        let binding = tls_server_end_point(b"not a certificate");
        let m = h.authenticate_with(&c, "CORP\\alice", "pw", Some(&binding), "HTTP/mail.corp", [7; 8], 2);
        assert_eq!(&m[..8], SIGNATURE);
        // No key exchange asked for, whatever the server offered.
        let flags = u32::from_le_bytes(m[60..64].try_into().unwrap());
        assert_eq!(flags & 0x4000_0000, 0);
        // The LM response is zeros with a server timestamp.
        let lm_at = u32::from_le_bytes(m[16..20].try_into().unwrap()) as usize;
        assert_eq!(&m[lm_at..lm_at + 24], &[0u8; 24]);
        // The client's target info holds the MIC flag and the binding hash.
        let nt_len = u16::from_le_bytes(m[20..22].try_into().unwrap()) as usize;
        let nt_at = u32::from_le_bytes(m[24..28].try_into().unwrap()) as usize;
        let blob = &m[nt_at + 16..nt_at + nt_len];
        let pairs: Vec<(u16, Vec<u8>)> = av_pairs(&blob[28..]).map(|(i, v)| (i, v.to_vec())).collect();
        assert!(pairs.contains(&(AV_FLAGS, AV_FLAG_MIC.to_le_bytes().to_vec())));
        assert!(pairs.contains(&(AV_CHANNEL_BINDINGS, channel_binding_hash(Some(&binding)).to_vec())));
        assert!(pairs.contains(&(AV_TARGET_NAME, utf16le("HTTP/mail.corp"))));
        // The user and domain went separately, in UTF-16.
        let dom_at = u32::from_le_bytes(m[32..36].try_into().unwrap()) as usize;
        assert_eq!(&m[dom_at..dom_at + 8], utf16le("CORP").as_slice());
        assert_ne!(&m[72..88], &[0u8; 16], "MIC filled in");
    }

    #[test]
    fn logins_split_on_a_backslash_only() {
        assert_eq!(split_user("CORP\\alice"), ("CORP".into(), "alice".into()));
        assert_eq!(split_user("alice@corp.example"), (String::new(), "alice@corp.example".into()));
    }
}

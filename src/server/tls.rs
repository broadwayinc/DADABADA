//! Self-signed certificate on init, and the rustls server config.

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Arc;

pub fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

pub fn fingerprint(der: &[u8]) -> String {
    Sha256::digest(der).iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
}

/// Makes `tls/cert.pem` and `tls/key.pem`; returns the fingerprint.
pub fn make_self_signed(data: &Path) -> Result<String, String> {
    let dir = data.join("tls");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut names = vec!["localhost".to_string(), "127.0.0.1".to_string()];
    if let Some(h) = host_name() {
        names.push(h);
    }
    let ck = rcgen::generate_simple_self_signed(names).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("cert.pem"), ck.cert.pem()).map_err(|e| e.to_string())?;
    let kp = dir.join("key.pem");
    std::fs::write(&kp, ck.key_pair.serialize_pem()).map_err(|e| e.to_string())?;
    set_mode(&kp, 0o600);
    Ok(fingerprint(ck.cert.der()))
}

/// This machine's name, for the certificate. Read from the system rather than /etc/hostname, which
/// macOS does not have; a name a certificate cannot hold (not ASCII) is left out.
fn host_name() -> Option<String> {
    let mut buf = [0u8; 256];
    if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } != 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let h = String::from_utf8_lossy(&buf[..end]).trim().to_string();
    (!h.is_empty() && h.is_ascii()).then_some(h)
}

pub fn set_mode(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode));
}

pub fn load(cert: &Path, key: &Path) -> Result<(Arc<rustls::ServerConfig>, String), String> {
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut std::io::BufReader::new(
        std::fs::File::open(cert).map_err(|e| format!("Could not open {}: {e}", cert.display()))?,
    ))
    .collect::<Result<_, _>>()
    .map_err(|e| format!("Could not read {}: {e}", cert.display()))?;
    if certs.is_empty() {
        return Err(format!("{} holds no certificate.", cert.display()));
    }
    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut std::io::BufReader::new(
        std::fs::File::open(key).map_err(|e| format!("Could not open {}: {e}", key.display()))?,
    ))
    .map_err(|e| format!("Could not read {}: {e}", key.display()))?
    .ok_or_else(|| format!("{} holds no private key.", key.display()))?;
    let fp = fingerprint(&certs[0]);
    let mut cfg = rustls::ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("The certificate and key do not work together: {e}"))?;
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok((Arc::new(cfg), fp))
}

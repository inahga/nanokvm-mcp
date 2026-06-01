//! WebSocket transport for the NanoKVM HID protocol.
//!
//! Just the connect helper. The wire format itself (binary frames carrying
//! USB HID reports prefixed by a 1-byte message tag) lives in `client.rs`,
//! next to the code that builds and sends those frames.
//!
//! TLS verification mirrors the reqwest client: with `verify_ssl=false` we
//! build a rustls `ClientConfig` whose certificate verifier accepts any chain,
//! so an HTTPS-with-self-signed NanoKVM works over the WS HID path too (UART
//! reset, char-by-char typing, etc.). With `verify_ssl=true` we pass no
//! connector and tungstenite uses its default webpki-roots verification.

use std::sync::Arc;

use http::HeaderValue;
use rustls::ClientConfig;
use rustls::DigitallySignedStruct;
use rustls::SignatureScheme;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, aws_lc_rs, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream, tungstenite::client::IntoClientRequest,
};

use crate::error::{Error, Result};

pub type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A `ServerCertVerifier` that accepts any certificate. Used only when the
/// operator passes `verify_ssl=false` for a self-signed NanoKVM. Handshake
/// signatures are still checked against the crypto provider's algorithms; only
/// the chain/name trust decision is skipped — the same trade-off reqwest's
/// `danger_accept_invalid_certs(true)` makes.
#[derive(Debug)]
struct NoVerify(Arc<CryptoProvider>);

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Build a tungstenite `Connector` that skips certificate verification. Returns
/// an error if the rustls config can't be assembled (e.g. no protocol versions).
fn insecure_connector() -> Result<Connector> {
    let provider = Arc::new(aws_lc_rs::default_provider());
    let config = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| Error::Ws(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerify(provider)))
        .with_no_client_auth();
    Ok(Connector::Rustls(Arc::new(config)))
}

/// Open a WebSocket connection to `ws_url`, attaching the auth cookie if any.
/// When `verify_ssl` is false, certificate verification is disabled to match
/// the reqwest client's behavior for self-signed devices.
pub async fn connect(ws_url: &str, cookie: Option<&str>, verify_ssl: bool) -> Result<WsStream> {
    let mut req = ws_url
        .into_client_request()
        .map_err(|e| Error::Ws(e.to_string()))?;
    if let Some(c) = cookie {
        let v = HeaderValue::from_str(c).map_err(|e| Error::Ws(e.to_string()))?;
        req.headers_mut().insert(http::header::COOKIE, v);
    }

    let connector = if verify_ssl {
        None
    } else {
        Some(insecure_connector()?)
    };

    let (ws, _) = tokio_tungstenite::connect_async_tls_with_config(req, None, false, connector)
        .await
        .map_err(|e| Error::Ws(e.to_string()))?;
    Ok(ws)
}

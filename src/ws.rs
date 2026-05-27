//! WebSocket transport for the NanoKVM HID protocol.
//!
//! Just the connect helper. The wire format itself (binary frames carrying
//! USB HID reports prefixed by a 1-byte message tag) lives in `client.rs`,
//! next to the code that builds and sends those frames.
//!
//! KNOWN LIMITATION: TLS verification here is whatever rustls does by default
//! (webpki roots). Unlike the reqwest client, this connect path does *not*
//! honor `verify_ssl=false`. An HTTPS-with-self-signed setup will succeed
//! over REST and silently fail on the WS HID path. If we ever need that, the
//! fix is `connect_async_tls_with_config` with a custom rustls `ClientConfig`.

use http::HeaderValue;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::client::IntoClientRequest,
};

use crate::error::{Error, Result};

pub type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Open a WebSocket connection to `ws_url`, attaching the auth cookie if any.
pub async fn connect(ws_url: &str, cookie: Option<&str>) -> Result<WsStream> {
    let mut req = ws_url
        .into_client_request()
        .map_err(|e| Error::Ws(e.to_string()))?;
    if let Some(c) = cookie {
        let v = HeaderValue::from_str(c).map_err(|e| Error::Ws(e.to_string()))?;
        req.headers_mut().insert(http::header::COOKIE, v);
    }
    let (ws, _) = tokio_tungstenite::connect_async(req)
        .await
        .map_err(|e| Error::Ws(e.to_string()))?;
    Ok(ws)
}

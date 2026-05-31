//! Async client for the NanoKVM REST API, WebSocket HID, and MJPEG screenshot.
//!
//! REST endpoints share a consistent envelope
//! `{ "code": 0, "msg": "...", "data": ... }`; non-zero `code` is an error.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use reqwest::{Method, header};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;
use tracing::debug;

use crate::auth::encrypt_password;
use crate::error::{Error, Result};
use crate::hid;

#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub username: String,
    pub password: String,
    pub screen_width: u32,
    pub screen_height: u32,
    pub use_https: bool,
    pub verify_ssl: bool,
    /// Allowlisted directory for ISO uploads. `None` disables `upload_iso`
    /// entirely. Any requested file must canonicalize to a path inside this
    /// directory (after `..` and symlink resolution).
    pub iso_dir: Option<std::path::PathBuf>,
    /// TTY device on the NanoKVM whose TX line is wired to an external
    /// power-relay trigger. `Some("/dev/ttyS2")` enables
    /// `external_power_reset`; `None` keeps the tool inert.
    pub uart_reset_device: Option<String>,
}

pub struct NanoKvmClient {
    config: Config,
    http: reqwest::Client,
    base_url: String,
    ws_url: String,
    /// The session JWT. Refreshable (was a `OnceCell`): a 401 drops it so the
    /// next request re-logs-in, since the NanoKVM token expires over time.
    token: Mutex<Option<String>>,
    /// HID WebSocket, opened lazily on first WS HID call and reopened on send
    /// failure.
    ws: Mutex<Option<crate::ws::WsStream>>,
}

impl NanoKvmClient {
    pub fn new(config: Config) -> Result<Arc<Self>> {
        let scheme_http = if config.use_https { "https" } else { "http" };
        let scheme_ws = if config.use_https { "wss" } else { "ws" };
        let base_url = format!("{scheme_http}://{}", config.host);
        let ws_url = format!("{scheme_ws}://{}/api/ws", config.host);

        let http = reqwest::Client::builder()
            .danger_accept_invalid_certs(!config.verify_ssl)
            .timeout(std::time::Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::limited(10))
            // Don't keep idle keep-alive connections around. The NanoKVM
            // closes the socket after responding, so a pooled connection
            // reused for a later request (notably a multi-GB upload POST
            // following the login) streams into a half-closed socket and is
            // reset mid-body. A fresh connection per request avoids it.
            .pool_max_idle_per_host(0)
            .build()?;

        Ok(Arc::new(Self {
            config,
            http,
            base_url,
            ws_url,
            token: Mutex::new(None),
            ws: Mutex::new(None),
        }))
    }

    // -------------------------------------------------------------------------
    // Authentication
    // -------------------------------------------------------------------------

    async fn ensure_authenticated(&self) -> Result<String> {
        let mut guard = self.token.lock().await;
        if let Some(t) = guard.as_ref() {
            return Ok(t.clone());
        }
        let t = self.login().await?;
        *guard = Some(t.clone());
        Ok(t)
    }

    /// Drop the cached token after a 401 so the next request re-authenticates.
    /// The NanoKVM JWT expires; without this it is cached for the life of the
    /// process and every call after expiry fails Unauthorized.
    async fn invalidate_token(&self) {
        *self.token.lock().await = None;
    }

    /// Attach the auth cookie and send a REST request. On 401 (expired JWT)
    /// drop the token, re-login, and retry once — but only if the body is
    /// cloneable (streaming/multipart bodies like the ISO upload can't be
    /// retried; they surface the 401 and the next fresh call re-auths).
    async fn send_authed(&self, req: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        let retry = req.try_clone();
        let cookie = self.auth_cookie_header().await?;
        let resp = req.header(header::COOKIE, cookie).send().await?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            if let Some(retry) = retry {
                self.invalidate_token().await;
                let cookie = self.auth_cookie_header().await?;
                return Ok(retry.header(header::COOKIE, cookie).send().await?);
            }
        }
        Ok(resp)
    }

    /// Open a WS to `ws_url` with the auth cookie. On a 401 handshake (expired
    /// JWT) drop the token, re-login, and retry once.
    async fn connect_ws(&self, ws_url: &str) -> Result<crate::ws::WsStream> {
        let cookie = self.auth_cookie_header().await?;
        match crate::ws::connect(ws_url, Some(&cookie)).await {
            Err(Error::Ws(msg)) if msg.contains("401") => {
                self.invalidate_token().await;
                let cookie = self.auth_cookie_header().await?;
                crate::ws::connect(ws_url, Some(&cookie)).await
            }
            other => other,
        }
    }

    /// Return a `Cookie: nano-kvm-token=<jwt>` header value, authenticating
    /// first if needed. The single helper for both HTTP and WS code paths.
    async fn auth_cookie_header(&self) -> Result<String> {
        let token = self.ensure_authenticated().await?;
        Ok(format!("nano-kvm-token={token}"))
    }

    async fn login(&self) -> Result<String> {
        let encrypted = encrypt_password(&self.config.password);
        let body = json!({
            "username": self.config.username,
            "password": encrypted,
        });
        let url = format!("{}/api/auth/login", self.base_url);
        let resp = self.http.post(url).json(&body).send().await?;

        // Cookie may be set via Set-Cookie...
        let cookie_token = resp
            .cookies()
            .find(|c| c.name() == "nano-kvm-token")
            .map(|c| c.value().to_owned());

        let resp = resp.error_for_status()?;
        let env: ApiEnvelope<Value> = resp.json().await?;
        if env.code != 0 {
            return Err(Error::Auth(env.msg.unwrap_or_else(|| "unknown".into())));
        }

        // ...or returned in the body's `data.token` field. Either is accepted.
        let token = cookie_token
            .or_else(|| {
                env.data
                    .as_ref()
                    .and_then(|d| d.get("token"))
                    .and_then(|t| t.as_str())
                    .map(str::to_owned)
            })
            .ok_or_else(|| Error::Auth("login succeeded but no token returned".into()))?;

        debug!("authenticated; token acquired");
        Ok(token)
    }

    /// Make an authenticated REST call, returning the decoded `data` field.
    /// `T = serde_json::Value` returns the raw `data` for endpoints whose
    /// shape we don't care about strictly.
    async fn request<T>(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<T>
    where
        T: for<'de> Deserialize<'de> + Default,
    {
        let url = format!("{}{path}", self.base_url);
        let mut req = self.http.request(method, url);
        if let Some(b) = body {
            req = req.json(&b);
        }
        self.send_envelope(req).await
    }

    /// Attach auth, send the request, parse the standard envelope.
    /// Use this when you need to send a non-JSON body (form, multipart).
    async fn send_envelope<T>(&self, req: reqwest::RequestBuilder) -> Result<T>
    where
        T: for<'de> Deserialize<'de> + Default,
    {
        let resp = self.send_authed(req).await?.error_for_status()?;
        let env: ApiEnvelope<T> = resp.json().await?;
        if env.code != 0 {
            return Err(Error::Api {
                code: env.code,
                msg: env.msg.unwrap_or_default(),
            });
        }
        Ok(env.data.unwrap_or_default())
    }

    /// Authenticated REST call that ignores the response body apart from the
    /// success code.
    async fn request_unit(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<()> {
        let _: Value = self.request(method, path, body).await?;
        Ok(())
    }

    // -------------------------------------------------------------------------
    // Power Control
    // -------------------------------------------------------------------------

    /// Press a virtual ATX button.
    /// `action`: "power" for the power button, "reset" for the reset button.
    /// `duration_ms`: hold duration; 800 ≈ short press, 5000 ≈ force-off hold.
    pub async fn power(&self, action: &str, duration_ms: u32) -> Result<()> {
        self.request_unit(
            Method::POST,
            "/api/vm/gpio",
            Some(json!({ "type": action, "duration": duration_ms })),
        )
        .await
    }

    pub async fn power_short(&self) -> Result<()> {
        self.power("power", 800).await
    }

    pub async fn power_long(&self) -> Result<()> {
        self.power("power", 5000).await
    }

    pub async fn reset(&self) -> Result<()> {
        self.power("reset", 800).await
    }

    /// Force-off, wait, then power on. Useful for boards (e.g. Raspberry Pi 5)
    /// that have no hardware reset.
    pub async fn power_cycle(&self, off_duration_ms: u64) -> Result<()> {
        self.power_long().await?;
        tokio::time::sleep(std::time::Duration::from_millis(off_duration_ms)).await;
        self.power_short().await
    }

    pub async fn led_status(&self) -> Result<Value> {
        // `/api/vm/gpio` is overloaded: GET returns `{pwr, hdd}` LED states;
        // POST presses a virtual button.
        self.request(Method::GET, "/api/vm/gpio", None).await
    }

    /// Drive the NanoKVM's UART TX line low for `duration_ms` via a UART
    /// break, then release. For setups where the TX line drives an external
    /// power relay (instead of the ATX header) and the host is wired into
    /// that relay's NO outlet, this performs a hard power cycle.
    ///
    /// Runs over the device's web-terminal WebSocket, using the same JWT auth
    /// as the rest of the API. Requires `config.uart_reset_device` to be set
    /// — otherwise returns [`Error::InvalidArgument`].
    pub async fn external_power_reset(&self, duration_ms: u64) -> Result<()> {
        use futures_util::{SinkExt, StreamExt};

        let device = self.config.uart_reset_device.as_deref().ok_or_else(|| {
            Error::InvalidArgument(
                "external power reset is disabled; set --uart-reset-device to enable".into(),
            )
        })?;
        if !device.starts_with("/dev/") {
            return Err(Error::InvalidArgument(format!(
                "refusing non-/dev device path: {device}"
            )));
        }

        let scheme = if self.config.use_https { "wss" } else { "ws" };
        let term_url = format!("{scheme}://{}/api/vm/terminal", self.config.host);
        let mut ws = self.connect_ws(&term_url).await?;

        // TIOCSBRK = 0x5427, TIOCCBRK = 0x5428. Python prints the sentinel
        // only after a successful TIOCCBRK; a thrown exception or kill stops
        // us from seeing it and we time out instead of reporting fake success.
        //
        // The sentinel is concatenated inside Python (`"NKVMOK" + "_8f3c"`)
        // so the source text — which the PTY echoes back as input — does not
        // contain the assembled string. Only Python's runtime output does.
        // That keeps us from being fooled by PTY input echo / line-wrap.
        let secs = duration_ms as f64 / 1000.0;
        const SENTINEL_LEFT: &str = "NKVMOK";
        const SENTINEL_RIGHT: &str = "_8f3c";
        let sentinel = format!("{SENTINEL_LEFT}{SENTINEL_RIGHT}");
        let cmd = format!(
            "python3 -c 'import fcntl,os,time; fd=os.open(\"{device}\", os.O_RDWR|os.O_NOCTTY); fcntl.ioctl(fd, 0x5427); time.sleep({secs}); fcntl.ioctl(fd, 0x5428); os.close(fd); print(\"{SENTINEL_LEFT}\" + \"{SENTINEL_RIGHT}\")'; exit\n"
        );
        ws.send(Message::Text(cmd.into()))
            .await
            .map_err(|e| Error::Ws(e.to_string()))?;

        // Wait for the sentinel echo. Allow generous slack: shell echo,
        // python startup on a slow SG2002, the configured break duration,
        // and the cleanup.
        let deadline = Duration::from_millis(duration_ms + 8000);
        let result = tokio::time::timeout(deadline, async {
            let mut buf = Vec::new();
            while let Some(msg) = ws.next().await {
                let m = msg.map_err(|e| Error::Ws(e.to_string()))?;
                match m {
                    Message::Binary(b) => buf.extend_from_slice(&b),
                    Message::Text(t) => buf.extend_from_slice(t.as_bytes()),
                    Message::Close(_) => break,
                    _ => {}
                }
                // The assembled sentinel only appears in Python's runtime
                // output, never in the source. A single match means success.
                if buf
                    .windows(sentinel.len())
                    .any(|w| w == sentinel.as_bytes())
                {
                    return Ok(());
                }
            }
            Err(Error::Ws(
                "terminal closed before UART break completed".into(),
            ))
        })
        .await;

        let _ = ws.close(None).await;

        match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(Error::Ws("external power reset timed out".into())),
        }
    }

    // -------------------------------------------------------------------------
    // HDMI
    // -------------------------------------------------------------------------

    pub async fn hdmi_status(&self) -> Result<Value> {
        self.request(Method::GET, "/api/vm/hdmi", None).await
    }

    pub async fn reset_hdmi(&self) -> Result<()> {
        self.request_unit(Method::POST, "/api/vm/hdmi/reset", None).await
    }

    pub async fn enable_hdmi(&self) -> Result<()> {
        self.request_unit(Method::POST, "/api/vm/hdmi/enable", None).await
    }

    pub async fn disable_hdmi(&self) -> Result<()> {
        self.request_unit(Method::POST, "/api/vm/hdmi/disable", None).await
    }

    // -------------------------------------------------------------------------
    // HID (REST)
    // -------------------------------------------------------------------------

    /// Paste text via the NanoKVM paste API. Max 1024 chars per call.
    pub async fn paste_text(&self, text: &str, language: &str) -> Result<()> {
        if text.chars().count() > 1024 {
            return Err(Error::InvalidArgument(
                "text must be 1024 characters or fewer".into(),
            ));
        }
        self.request_unit(
            Method::POST,
            "/api/hid/paste",
            // Note: the NanoKVM API misspells "language" as "langue".
            Some(json!({ "content": text, "langue": language })),
        )
        .await
    }

    pub async fn reset_hid(&self) -> Result<()> {
        self.request_unit(Method::POST, "/api/hid/reset", None).await
    }

    pub async fn hid_mode(&self) -> Result<String> {
        let v: Value = self.request(Method::GET, "/api/hid/mode", None).await?;
        Ok(v.get("mode")
            .and_then(|m| m.as_str())
            .unwrap_or("normal")
            .to_string())
    }

    // -------------------------------------------------------------------------
    // Storage
    // -------------------------------------------------------------------------

    pub async fn list_images(&self) -> Result<Value> {
        self.request(Method::GET, "/api/storage/image", None).await
    }

    pub async fn mounted_image(&self) -> Result<Value> {
        self.request(Method::GET, "/api/storage/image/mounted", None)
            .await
    }

    pub async fn mount_image(&self, file: &str, cdrom: bool) -> Result<()> {
        self.request_unit(
            Method::POST,
            "/api/storage/image/mount",
            Some(json!({ "file": file, "cdrom": cdrom })),
        )
        .await
    }

    pub async fn unmount_image(&self) -> Result<()> {
        self.request_unit(
            Method::POST,
            "/api/storage/image/mount",
            Some(json!({})),
        )
        .await
    }

    pub async fn delete_image(&self, file: &str) -> Result<()> {
        self.request_unit(
            Method::POST,
            "/api/storage/image/delete",
            Some(json!({ "file": file })),
        )
        .await
    }

    pub async fn cdrom(&self) -> Result<Value> {
        self.request(Method::GET, "/api/storage/cdrom", None).await
    }

    // -------------------------------------------------------------------------
    // ISO upload (/api/download/*)
    //
    // The upstream NanoKVM doesn't put these under /api/storage — they live
    // under /api/download/* and use a multipart body. We deliberately do not
    // expose the URL-fetch endpoint (POST /api/download/image): that turns
    // into device-side SSRF for the caller, and tool-description warnings
    // aren't a real defense.
    // -------------------------------------------------------------------------

    /// Whether `/data` is writable; gates the upload endpoint.
    pub async fn iso_upload_enabled(&self) -> Result<bool> {
        let v: Value = self
            .request(Method::GET, "/api/download/image/enabled", None)
            .await?;
        Ok(v.get("Enabled")
            .or_else(|| v.get("enabled"))
            .and_then(Value::as_bool)
            .unwrap_or(false))
    }

    /// Multipart-upload a local ISO file to `/data` on the NanoKVM. The
    /// filename used on the device is the basename of `local_path`.
    ///
    /// `local_path` must canonicalize to a path inside `iso_dir`; otherwise
    /// the request is refused. lstat → open → canonicalize is deliberate: the
    /// fd anchors the inode so a swap between open and canonicalize can only
    /// trip the containment check, never make us read the wrong file. A
    /// residual race lets a writer in `iso_dir` swap to a FIFO between lstat
    /// and open to hang the request; operators should keep `iso_dir` writable
    /// only by trusted accounts.
    pub async fn upload_iso(&self, local_path: &str) -> Result<Value> {
        use tokio::io::AsyncReadExt;

        let iso_dir = self.config.iso_dir.as_ref().ok_or_else(|| {
            Error::InvalidArgument(
                "uploads are disabled; set --iso-dir / NANOKVM_ISO_DIR to enable".into(),
            )
        })?;

        // lstat upfront — refuses non-regular files (FIFO/socket/device) and
        // symlinks before `File::open` can block on them.
        let pre_meta = tokio::fs::symlink_metadata(local_path).await.map_err(|e| {
            Error::InvalidArgument(format!("cannot stat {local_path}: {e}"))
        })?;
        if !pre_meta.file_type().is_file() {
            return Err(Error::InvalidArgument(format!(
                "{local_path} is not a regular file (or is a symlink)"
            )));
        }

        let mut file = tokio::fs::File::open(local_path).await.map_err(|e| {
            Error::InvalidArgument(format!("cannot open {local_path}: {e}"))
        })?;

        // fstat via the fd — catches a swap between lstat and open.
        let meta = file.metadata().await.map_err(|e| {
            Error::InvalidArgument(format!("cannot stat {local_path}: {e}"))
        })?;
        if !meta.file_type().is_file() {
            return Err(Error::InvalidArgument(format!(
                "{local_path} is not a regular file"
            )));
        }

        let canonical_dir = tokio::fs::canonicalize(iso_dir).await.map_err(|e| {
            Error::InvalidArgument(format!(
                "iso-dir {} unreadable: {e}",
                iso_dir.display()
            ))
        })?;
        let canonical_path = tokio::fs::canonicalize(local_path).await.map_err(|e| {
            Error::InvalidArgument(format!("cannot resolve {local_path}: {e}"))
        })?;
        if !canonical_path.starts_with(&canonical_dir) {
            return Err(Error::InvalidArgument(format!(
                "{local_path} is outside the configured iso-dir ({})",
                iso_dir.display()
            )));
        }

        let filename = canonical_path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| Error::InvalidArgument(format!("bad local path: {local_path}")))?
            .to_owned();
        if !filename.to_ascii_lowercase().ends_with(".iso") {
            return Err(Error::InvalidArgument(format!(
                "only .iso files may be uploaded (got {filename})"
            )));
        }

        // Read from the fd, not the (possibly-swapped) path.
        let mut bytes = Vec::with_capacity(meta.len() as usize);
        file.read_to_end(&mut bytes).await.map_err(|e| {
            Error::InvalidArgument(format!("failed to read {local_path}: {e}"))
        })?;

        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(filename)
            .mime_str("application/octet-stream")
            .map_err(|e| Error::InvalidArgument(e.to_string()))?;
        let form = reqwest::multipart::Form::new().part("file", part);

        let req = self
            .http
            .post(format!("{}/api/download/file", self.base_url))
            // The client-wide 30s timeout (see `new`) suits the small
            // JSON/HID calls, but writing a multi-GB ISO to the NanoKVM's
            // SD card takes minutes — the global deadline aborts the POST
            // mid-stream and leaves a truncated file on `/data`. A
            // per-request timeout supersedes the client default, so give
            // the upload a generous ceiling instead.
            .timeout(std::time::Duration::from_secs(3600))
            .multipart(form);
        self.send_envelope(req).await
    }

    // -------------------------------------------------------------------------
    // System info
    // -------------------------------------------------------------------------

    pub async fn info(&self) -> Result<Value> {
        self.request(Method::GET, "/api/vm/info", None).await
    }

    pub async fn hardware(&self) -> Result<Value> {
        self.request(Method::GET, "/api/vm/hardware", None).await
    }

    // -------------------------------------------------------------------------
    // HID (WebSocket)
    //
    // Wire format (firmware ≥ 2.3.0):
    //   binary frame, first byte = message kind
    //     0 = heartbeat
    //     1 = keyboard event   — followed by 8-byte USB HID boot keyboard report
    //     2 = mouse event      — followed by 4-byte (relative) or 6-byte
    //                            (absolute) HID mouse report
    //
    // The 2.2.x firmware used a JSON-text format with a different array shape.
    // That older protocol is gone — we don't try to support it.
    // -------------------------------------------------------------------------

    /// Send a binary WS frame, opening the connection lazily and reopening it
    /// once on send failure.
    async fn ws_send_binary(&self, payload: Vec<u8>) -> Result<()> {
        let mut guard = self.ws.lock().await;

        for attempt in 0..2 {
            if guard.is_none() {
                *guard = Some(self.connect_ws(&self.ws_url).await?);
            }
            let ws = guard.as_mut().expect("connected on this iteration");
            match ws.send(Message::Binary(payload.clone().into())).await {
                Ok(()) => return Ok(()),
                Err(e) if attempt == 0 => {
                    debug!(error = %e, "ws send failed; reconnecting");
                    *guard = None;
                    continue;
                }
                Err(e) => return Err(Error::Ws(e.to_string())),
            }
        }
        unreachable!("loop runs at most twice and either returns or continues")
    }

    /// Press and release a single key, optionally with modifiers.
    pub async fn send_key(
        &self,
        key: &str,
        ctrl: bool,
        shift: bool,
        alt: bool,
        meta: bool,
    ) -> Result<()> {
        let info = hid::get_key_info(key)
            .ok_or_else(|| Error::InvalidArgument(format!("unknown key: {key}")))?;

        let mut mods = 0u8;
        if ctrl { mods |= hid::modifier::CTRL_LEFT; }
        if shift || info.shift { mods |= hid::modifier::SHIFT_LEFT; }
        if alt { mods |= hid::modifier::ALT_LEFT; }
        if meta { mods |= hid::modifier::META_LEFT; }

        self.ws_send_binary(keyboard_frame(mods, info.code)).await?;
        tokio::time::sleep(Duration::from_millis(50)).await;
        self.ws_send_binary(keyboard_release_frame()).await
    }

    /// Type `text` character-by-character via the HID WebSocket. For long
    /// strings use [`paste_text`] instead.
    pub async fn send_text_ws(&self, text: &str) -> Result<()> {
        for c in text.chars() {
            let Some((code, modifier)) = hid::char_to_keycode(c) else {
                tracing::warn!(?c, "skipping unmappable character");
                continue;
            };
            self.ws_send_binary(keyboard_frame(modifier, code)).await?;
            tokio::time::sleep(Duration::from_millis(30)).await;
            self.ws_send_binary(keyboard_release_frame()).await?;
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        Ok(())
    }

    /// Move the absolute (touchpad) mouse cursor to screen coordinates.
    pub async fn mouse_move(&self, x: i32, y: i32) -> Result<()> {
        let kvm_x = scale_coord(x, self.config.screen_width);
        let kvm_y = scale_coord(y, self.config.screen_height);
        self.ws_send_binary(mouse_abs_frame(0, kvm_x, kvm_y, 0)).await
    }

    /// Click a mouse button, optionally moving to a position first. Uses the
    /// absolute (touchpad) device so screen coordinates land correctly.
    pub async fn mouse_click(
        &self,
        button: &str,
        position: Option<(i32, i32)>,
    ) -> Result<()> {
        let btn_mask = parse_mouse_button(button)?;

        // If the caller asked for a position, move there first and reuse those
        // coordinates for the press/release so the touchpad doesn't relocate.
        let (kx, ky) = if let Some((px, py)) = position {
            let kx = scale_coord(px, self.config.screen_width);
            let ky = scale_coord(py, self.config.screen_height);
            self.ws_send_binary(mouse_abs_frame(0, kx, ky, 0)).await?;
            tokio::time::sleep(Duration::from_millis(50)).await;
            (kx, ky)
        } else {
            (0, 0)
        };

        self.ws_send_binary(mouse_abs_frame(btn_mask, kx, ky, 0)).await?;
        tokio::time::sleep(Duration::from_millis(50)).await;
        self.ws_send_binary(mouse_abs_frame(0, kx, ky, 0)).await
    }

    pub async fn tap(&self, x: i32, y: i32) -> Result<()> {
        self.mouse_click("left", Some((x, y))).await
    }

    /// Scroll the mouse wheel. Positive = scroll *down* (kept for parity with
    /// the prior tool description); the HID wheel convention is the opposite,
    /// so we negate when packing.
    pub async fn mouse_scroll(&self, delta: i32) -> Result<()> {
        let wheel = (-delta).clamp(-127, 127) as i8;
        self.ws_send_binary(mouse_abs_frame(0, 0, 0, wheel)).await
    }

    /// Send a single relative-mouse HID report. Required for environments that
    /// don't speak the HID touchpad / absolute descriptor — most notably PC
    /// BIOSes and other pre-OS firmware.
    ///
    /// `dx` / `dy` are signed pixel deltas (clamped to [-127, 127]); large
    /// jumps are decomposed into a sequence of clamped reports.
    pub async fn mouse_move_relative(&self, dx: i32, dy: i32) -> Result<()> {
        let (mut rx, mut ry) = (dx, dy);
        while rx != 0 || ry != 0 {
            let step_x = rx.clamp(-127, 127);
            let step_y = ry.clamp(-127, 127);
            self.ws_send_binary(mouse_rel_frame(0, step_x as i8, step_y as i8, 0))
                .await?;
            rx -= step_x;
            ry -= step_y;
            if rx != 0 || ry != 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
        Ok(())
    }

    /// Press+release a mouse button via the *relative* device. For BIOS use:
    /// caller is responsible for moving the cursor first via
    /// `mouse_move_relative` (since there's no absolute position to click).
    pub async fn mouse_click_relative(&self, button: &str) -> Result<()> {
        let btn_mask = parse_mouse_button(button)?;
        self.ws_send_binary(mouse_rel_frame(btn_mask, 0, 0, 0)).await?;
        tokio::time::sleep(Duration::from_millis(50)).await;
        self.ws_send_binary(mouse_rel_frame(0, 0, 0, 0)).await
    }

    // -------------------------------------------------------------------------
    // Screenshot (MJPEG single-frame capture)
    // -------------------------------------------------------------------------

    /// Capture a single JPEG frame from the device's MJPEG stream.
    ///
    /// Uses the `?n=1` parameter so the NanoKVM emits exactly one frame and
    /// closes the stream — far less disruptive than tapping into the continuous
    /// feed used by the web UI.
    pub async fn screenshot(&self, timeout: Duration) -> Result<Vec<u8>> {
        tokio::time::timeout(timeout, self.screenshot_inner())
            .await
            .map_err(|_| Error::ScreenshotTimeout)?
    }

    async fn screenshot_inner(&self) -> Result<Vec<u8>> {
        let url = format!("{}/api/stream/mjpeg?n=1", self.base_url);
        let resp = self
            .send_authed(self.http.get(&url))
            .await?
            .error_for_status()?;

        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);

        while let Some(chunk) = stream.next().await {
            buf.extend_from_slice(&chunk?);
            if let Some(frame) = extract_jpeg_frame(&buf) {
                debug!(bytes = frame.len(), "captured screenshot frame");
                return Ok(frame);
            }
        }

        Err(Error::ScreenshotTimeout)
    }
}

/// Find the first complete JPEG frame in `buf` (delimited by SOI…EOI markers)
/// and return a copy of its bytes. Returns `None` if a complete frame isn't
/// present yet — caller should buffer more input and retry.
fn extract_jpeg_frame(buf: &[u8]) -> Option<Vec<u8>> {
    const JPEG_SOI: &[u8; 2] = &[0xFF, 0xD8]; // Start Of Image
    const JPEG_EOI: &[u8; 2] = &[0xFF, 0xD9]; // End Of Image
    let start = find_subsequence(buf, JPEG_SOI)?;
    let end = find_subsequence(&buf[start..], JPEG_EOI)? + start;
    Some(buf[start..end + JPEG_EOI.len()].to_vec())
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Map a screen coordinate in pixels to NanoKVM's `[1, 0x7FFF]` range.
fn scale_coord(pixel: i32, screen: u32) -> u16 {
    let s = (pixel as f64 / screen as f64 * 0x7FFEu32 as f64) as i64 + 1;
    s.clamp(1, 0x7FFF) as u16
}

/// USB HID 8-byte boot-keyboard report wrapped with the WS keyboard-event tag.
fn keyboard_frame(mods: u8, keycode: u8) -> Vec<u8> {
    vec![WS_KEYBOARD, mods, 0, keycode, 0, 0, 0, 0, 0]
}

fn keyboard_release_frame() -> Vec<u8> {
    vec![WS_KEYBOARD, 0, 0, 0, 0, 0, 0, 0, 0]
}

/// Absolute-mouse HID report wrapped with the WS mouse-event tag.
/// Layout: [tag, buttons, x_lo, x_hi, y_lo, y_hi, wheel]
fn mouse_abs_frame(buttons: u8, x: u16, y: u16, wheel: i8) -> Vec<u8> {
    let [x_lo, x_hi] = x.to_le_bytes();
    let [y_lo, y_hi] = y.to_le_bytes();
    vec![WS_MOUSE, buttons, x_lo, x_hi, y_lo, y_hi, wheel as u8]
}

/// Relative-mouse HID report wrapped with the WS mouse-event tag.
/// Layout: [tag, buttons, dx, dy, wheel]
fn mouse_rel_frame(buttons: u8, dx: i8, dy: i8, wheel: i8) -> Vec<u8> {
    vec![WS_MOUSE, buttons, dx as u8, dy as u8, wheel as u8]
}

/// Decode an HID button name into the USB HID mouse-button bitmask.
fn parse_mouse_button(name: &str) -> Result<u8> {
    match name {
        "left" => Ok(0x01),
        "right" => Ok(0x02),
        "middle" => Ok(0x04),
        other => Err(Error::InvalidArgument(format!(
            "unknown mouse button: {other}"
        ))),
    }
}

const WS_KEYBOARD: u8 = 1;
const WS_MOUSE: u8 = 2;

#[derive(Debug, Deserialize, Serialize)]
struct ApiEnvelope<T> {
    code: i64,
    msg: Option<String>,
    data: Option<T>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coord_scaling_endpoints() {
        // Left/top edge maps to 1.
        assert_eq!(scale_coord(0, 1920), 1);
        // Right edge maps to the max (0x7FFE + 1 = 0x7FFF).
        assert_eq!(scale_coord(1920, 1920), 0x7FFF);
        // Off-screen pixels are clamped, never sent as 0 or > 32767.
        assert_eq!(scale_coord(-100, 1920), 1);
        assert_eq!(scale_coord(9_999_999, 1920), 0x7FFF);
    }

    #[test]
    fn coord_scaling_midpoint() {
        let half = scale_coord(960, 1920);
        // Should land within 1 of 0x7FFE/2 + 1 = 16384.
        assert!(half.abs_diff(16384) <= 1, "got {half}");
    }

    #[test]
    fn jpeg_frame_extracted_when_complete() {
        let payload = b"prefix\xff\xd8inner\xff\xd9garbage_after";
        let frame = extract_jpeg_frame(payload).unwrap();
        assert_eq!(&frame[..2], &[0xFF, 0xD8]);
        assert_eq!(&frame[frame.len() - 2..], &[0xFF, 0xD9]);
        assert_eq!(&frame[2..frame.len() - 2], b"inner");
    }

    #[test]
    fn jpeg_frame_none_when_incomplete() {
        assert!(extract_jpeg_frame(b"prefix\xff\xd8inner_no_end_marker").is_none());
        assert!(extract_jpeg_frame(b"no markers at all").is_none());
    }
}

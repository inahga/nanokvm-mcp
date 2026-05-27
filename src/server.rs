//! MCP tool surface — every tool delegates to a method on
//! [`crate::client::NanoKvmClient`].

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    schemars, tool, tool_handler, tool_router,
};
use serde::{Deserialize, Serialize};

use crate::client::NanoKvmClient;
use crate::error::Error;
use crate::image::process_image;

const SCREENSHOT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct NanoKvmServer {
    client: Arc<NanoKvmClient>,
    // Read by the `#[tool_handler]` macro expansion; clippy doesn't see it.
    #[allow(dead_code)]
    tool_router: ToolRouter<NanoKvmServer>,
}

#[tool_router]
impl NanoKvmServer {
    pub fn new(client: Arc<NanoKvmClient>) -> Self {
        Self {
            client,
            tool_router: Self::tool_router(),
        }
    }

    // -------- power --------

    #[tool(
        description = "Press the target's power or reset button. action = \"power\" (short press), \"power_long\" (force off, 5s hold), or \"reset\". Note: \"reset\" does nothing on boards without a hardware reset pin (e.g. Raspberry Pi 5) — use nanokvm_power_cycle there."
    )]
    async fn nanokvm_power(
        &self,
        Parameters(args): Parameters<PowerArgs>,
    ) -> Result<CallToolResult, McpError> {
        let result = match args.action.as_str() {
            "power" => self.client.power_short().await,
            "power_long" => self.client.power_long().await,
            "reset" => self.client.reset().await,
            other => {
                return Err(McpError::invalid_params(
                    format!("unknown action: {other}"),
                    None,
                ));
            }
        };
        result.map_err(to_mcp)?;
        Ok(text_ok(format!("power action: {}", args.action)))
    }

    #[tool(
        description = "Pulse the NanoKVM's UART TX line low for `duration_ms` (default 5000), then release. For hosts wired to an external power relay (not the ATX header): the break drops the relay, killing host power; releasing the break restores it. Requires --uart-reset-device to be configured by the operator; returns an error otherwise."
    )]
    async fn nanokvm_external_power_reset(
        &self,
        Parameters(args): Parameters<ExternalPowerResetArgs>,
    ) -> Result<CallToolResult, McpError> {
        let ms = args.duration_ms.unwrap_or(5000);
        self.client.external_power_reset(ms).await.map_err(to_mcp)?;
        Ok(text_ok(format!("external power reset complete ({ms}ms break)")))
    }

    #[tool(
        description = "Force off, wait, then power on. Use this to \"reset\" boards with no hardware reset line. off_duration_ms defaults to 3000."
    )]
    async fn nanokvm_power_cycle(
        &self,
        Parameters(args): Parameters<PowerCycleArgs>,
    ) -> Result<CallToolResult, McpError> {
        let ms = args.off_duration_ms.unwrap_or(3000);
        self.client.power_cycle(ms).await.map_err(to_mcp)?;
        Ok(text_ok(format!("power cycled (off for {ms}ms)")))
    }

    #[tool(description = "Get power and HDD LED status of the target.")]
    async fn nanokvm_led_status(&self) -> Result<CallToolResult, McpError> {
        let v = self.client.led_status().await.map_err(to_mcp)?;
        json_content(&v)
    }

    // -------- hdmi --------

    #[tool(description = "Get HDMI connection status and resolution.")]
    async fn nanokvm_hdmi_status(&self) -> Result<CallToolResult, McpError> {
        let v = self.client.hdmi_status().await.map_err(to_mcp)?;
        json_content(&v)
    }

    #[tool(description = "Reset the HDMI capture (useful if video stops updating).")]
    async fn nanokvm_hdmi_reset(&self) -> Result<CallToolResult, McpError> {
        self.client.reset_hdmi().await.map_err(to_mcp)?;
        Ok(text_ok("hdmi reset"))
    }

    #[tool(description = "Enable the HDMI capture pipeline.")]
    async fn nanokvm_hdmi_enable(&self) -> Result<CallToolResult, McpError> {
        self.client.enable_hdmi().await.map_err(to_mcp)?;
        Ok(text_ok("hdmi enabled"))
    }

    #[tool(
        description = "Disable the HDMI capture pipeline. Screenshots will fail until you re-enable."
    )]
    async fn nanokvm_hdmi_disable(&self) -> Result<CallToolResult, McpError> {
        self.client.disable_hdmi().await.map_err(to_mcp)?;
        Ok(text_ok("hdmi disabled"))
    }

    // -------- text/key input --------

    #[tool(
        description = "Type a string via the NanoKVM paste API (≤1024 chars). language: \"\" for US, \"de\" for German layout."
    )]
    async fn nanokvm_send_text(
        &self,
        Parameters(args): Parameters<SendTextArgs>,
    ) -> Result<CallToolResult, McpError> {
        let lang = args.language.as_deref().unwrap_or("");
        let n = args.text.chars().count();
        self.client
            .paste_text(&args.text, lang)
            .await
            .map_err(to_mcp)?;
        Ok(text_ok(format!("typed {n} chars")))
    }

    #[tool(
        description = "Press a single key, optionally with modifiers. Key names: enter, escape, tab, backspace, delete, space, f1..f12, up/down/left/right, home/end/pageup/pagedown/insert, or a single character."
    )]
    async fn nanokvm_send_key(
        &self,
        Parameters(args): Parameters<SendKeyArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.client
            .send_key(
                &args.key,
                args.ctrl.unwrap_or(false),
                args.shift.unwrap_or(false),
                args.alt.unwrap_or(false),
                args.meta.unwrap_or(false),
            )
            .await
            .map_err(to_mcp)?;
        Ok(text_ok(format!("sent key: {}", args.key)))
    }

    // -------- mouse --------

    #[tool(description = "Tap (left-click) at an absolute screen position in pixels.")]
    async fn nanokvm_tap(
        &self,
        Parameters(args): Parameters<XyArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.client.tap(args.x, args.y).await.map_err(to_mcp)?;
        Ok(text_ok(format!("tapped at ({}, {})", args.x, args.y)))
    }

    #[tool(
        description = "Click a mouse button. button = \"left\" | \"right\" | \"middle\". If x and y are provided, move there first."
    )]
    async fn nanokvm_click(
        &self,
        Parameters(args): Parameters<ClickArgs>,
    ) -> Result<CallToolResult, McpError> {
        let button = args.button.as_deref().unwrap_or("left");
        let pos = match (args.x, args.y) {
            (Some(x), Some(y)) => Some((x, y)),
            _ => None,
        };
        self.client.mouse_click(button, pos).await.map_err(to_mcp)?;
        Ok(text_ok(format!("clicked {button}")))
    }

    #[tool(description = "Move the mouse cursor to an absolute screen position in pixels.")]
    async fn nanokvm_move(
        &self,
        Parameters(args): Parameters<XyArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.client.mouse_move(args.x, args.y).await.map_err(to_mcp)?;
        Ok(text_ok(format!("moved to ({}, {})", args.x, args.y)))
    }

    #[tool(description = "Scroll the mouse wheel. Positive = down, negative = up.")]
    async fn nanokvm_scroll(
        &self,
        Parameters(args): Parameters<ScrollArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.client.mouse_scroll(args.amount).await.map_err(to_mcp)?;
        Ok(text_ok(format!("scrolled {}", args.amount)))
    }

    #[tool(
        description = "Move the mouse by a relative pixel offset via the HID boot mouse. Use this in pre-OS environments (BIOS / UEFI setup / bootloader) where the absolute HID touchpad isn't supported — nanokvm_move / nanokvm_click won't work there. Large deltas are decomposed into smaller HID reports automatically."
    )]
    async fn nanokvm_move_relative(
        &self,
        Parameters(args): Parameters<RelativeXyArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.client
            .mouse_move_relative(args.dx, args.dy)
            .await
            .map_err(to_mcp)?;
        Ok(text_ok(format!("moved by ({}, {})", args.dx, args.dy)))
    }

    #[tool(
        description = "Click a mouse button via the HID boot mouse (relative path). For pre-OS environments; move first with nanokvm_move_relative."
    )]
    async fn nanokvm_click_relative(
        &self,
        Parameters(args): Parameters<ClickRelativeArgs>,
    ) -> Result<CallToolResult, McpError> {
        let button = args.button.as_deref().unwrap_or("left");
        self.client
            .mouse_click_relative(button)
            .await
            .map_err(to_mcp)?;
        Ok(text_ok(format!("clicked {button} (relative)")))
    }

    // -------- screenshot --------

    #[tool(
        description = "Capture a screenshot. Returns a JPEG. By default downscales to 1920x1080 to keep the response small; pass 0 for max_width/max_height to disable that limit."
    )]
    async fn nanokvm_screenshot(
        &self,
        Parameters(args): Parameters<ScreenshotArgs>,
    ) -> Result<CallToolResult, McpError> {
        let raw = self
            .client
            .screenshot(SCREENSHOT_TIMEOUT)
            .await
            .map_err(to_mcp)?;
        let max_w = args.max_width.unwrap_or(1920);
        let max_h = args.max_height.unwrap_or(1080);
        let quality = args.quality.unwrap_or(80);
        let jpeg = process_image(&raw, max_w, max_h, quality).map_err(to_mcp)?;
        let b64 = B64.encode(&jpeg);
        Ok(CallToolResult::success(vec![Content::image(b64, "image/jpeg")]))
    }

    // -------- storage --------

    #[tool(description = "List available ISO images on the NanoKVM.")]
    async fn nanokvm_list_images(&self) -> Result<CallToolResult, McpError> {
        let v = self.client.list_images().await.map_err(to_mcp)?;
        json_content(&v)
    }

    #[tool(
        description = "Mount an ISO image on the target. as_cdrom=true mounts as CD-ROM, false as USB disk. Call nanokvm_unmount_iso when done — the mount persists until you do."
    )]
    async fn nanokvm_mount_iso(
        &self,
        Parameters(args): Parameters<MountIsoArgs>,
    ) -> Result<CallToolResult, McpError> {
        let cdrom = args.as_cdrom.unwrap_or(true);
        self.client
            .mount_image(&args.file, cdrom)
            .await
            .map_err(to_mcp)?;
        Ok(text_ok(format!(
            "mounted {} as {}",
            args.file,
            if cdrom { "CD-ROM" } else { "USB disk" }
        )))
    }

    #[tool(description = "Unmount the currently mounted ISO.")]
    async fn nanokvm_unmount_iso(&self) -> Result<CallToolResult, McpError> {
        self.client.unmount_image().await.map_err(to_mcp)?;
        Ok(text_ok("unmounted"))
    }

    #[tool(description = "Get information about the currently mounted ISO (if any).")]
    async fn nanokvm_mounted_image(&self) -> Result<CallToolResult, McpError> {
        let v = self.client.mounted_image().await.map_err(to_mcp)?;
        json_content(&v)
    }

    #[tool(description = "Delete an ISO image from the NanoKVM's storage.")]
    async fn nanokvm_delete_iso(
        &self,
        Parameters(args): Parameters<DeleteIsoArgs>,
    ) -> Result<CallToolResult, McpError> {
        self.client.delete_image(&args.file).await.map_err(to_mcp)?;
        Ok(text_ok(format!("deleted {}", args.file)))
    }

    #[tool(description = "Get the current CD-ROM exposure flag.")]
    async fn nanokvm_cdrom(&self) -> Result<CallToolResult, McpError> {
        let v = self.client.cdrom().await.map_err(to_mcp)?;
        json_content(&v)
    }

    #[tool(
        description = "Whether the NanoKVM's /data storage is writable (i.e. ISO uploads are possible)."
    )]
    async fn nanokvm_iso_upload_enabled(&self) -> Result<CallToolResult, McpError> {
        let enabled = self.client.iso_upload_enabled().await.map_err(to_mcp)?;
        Ok(text_ok(format!("upload_enabled: {enabled}")))
    }

    #[tool(
        description = "Upload a local ISO file to the NanoKVM's /data storage. local_path must be inside the operator-configured --iso-dir; anything outside is rejected. The filename used on the device is the basename. The ISO persists on /data until you remove it with nanokvm_delete_iso — clean up when you're done."
    )]
    async fn nanokvm_upload_iso(
        &self,
        Parameters(args): Parameters<UploadIsoArgs>,
    ) -> Result<CallToolResult, McpError> {
        let v = self.client.upload_iso(&args.local_path).await.map_err(to_mcp)?;
        json_content(&v)
    }

    // -------- hid management / info --------

    #[tool(description = "Reset the HID (keyboard/mouse) devices.")]
    async fn nanokvm_reset_hid(&self) -> Result<CallToolResult, McpError> {
        self.client.reset_hid().await.map_err(to_mcp)?;
        Ok(text_ok("hid reset"))
    }

    #[tool(description = "Get the current HID mode (\"normal\" or \"hid-only\").")]
    async fn nanokvm_hid_mode(&self) -> Result<CallToolResult, McpError> {
        let m = self.client.hid_mode().await.map_err(to_mcp)?;
        Ok(text_ok(format!("hid_mode: {m}")))
    }

    #[tool(
        description = "Type text character-by-character via the WS HID path. Slower than nanokvm_send_text but works without the 1024-character paste limit and exercises the per-character WS framing path. Unmappable characters are skipped."
    )]
    async fn nanokvm_type_text(
        &self,
        Parameters(args): Parameters<TypeTextArgs>,
    ) -> Result<CallToolResult, McpError> {
        let n = args.text.chars().count();
        self.client.send_text_ws(&args.text).await.map_err(to_mcp)?;
        Ok(text_ok(format!("typed {n} chars via WS")))
    }

    #[tool(description = "Get NanoKVM device information (IP, firmware, etc).")]
    async fn nanokvm_info(&self) -> Result<CallToolResult, McpError> {
        let v = self.client.info().await.map_err(to_mcp)?;
        json_content(&v)
    }

    #[tool(description = "Get NanoKVM hardware information.")]
    async fn nanokvm_hardware(&self) -> Result<CallToolResult, McpError> {
        let v = self.client.hardware().await.map_err(to_mcp)?;
        json_content(&v)
    }
}

#[tool_handler]
impl ServerHandler for NanoKvmServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::from_build_env())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_instructions(
                "MCP server for controlling a Sipeed NanoKVM device. Exposes power, \
                 keyboard, mouse, screenshot, ISO storage, and device-info tools.\n\
                 \n\
                 Mouse — pick the right family for what's on screen:\n\
                 - In an OS (Linux/Windows/macOS desktop, login manager, X/Wayland), \
                 use the absolute tools: nanokvm_move, nanokvm_click, nanokvm_tap. \
                 They take screen coordinates and teleport the cursor.\n\
                 - In pre-OS environments (BIOS / UEFI setup, GRUB, install media \
                 before the kernel takes over HID), use the relative tools: \
                 nanokvm_move_relative, nanokvm_click_relative. BIOS firmware only \
                 implements the HID boot mouse and ignores the absolute touchpad \
                 descriptor — absolute calls will appear to do random small moves \
                 instead of jumping to the target.\n\
                 - To force the cursor to a known corner via relative deltas, send \
                 something large like ±3000 in each axis; the firmware clamps at the \
                 edge.\n\
                 \n\
                 Cleanup norms (please follow):\n\
                 - If you mount an ISO with nanokvm_mount_iso, call nanokvm_unmount_iso \
                 before ending the session.\n\
                 - If you upload an ISO that won't be reused, delete it with \
                 nanokvm_delete_iso when finished. /data is shared storage; don't \
                 leave artifacts behind.\n\
                 - Power tools (nanokvm_power, nanokvm_power_cycle) physically affect \
                 the target machine. Confirm intent before calling them.\n\
                 - The host (target) and the NanoKVM are two different devices. \
                 The NanoKVM is the controlling KVM; the target is whatever is \
                 plugged into it.",
            )
    }
}

// -------- parameter types --------

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct PowerArgs {
    /// "power" (short press), "power_long" (force off), or "reset".
    pub action: String,
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct PowerCycleArgs {
    /// Milliseconds to wait between the force-off and power-on (default 3000).
    #[serde(default)]
    pub off_duration_ms: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct ExternalPowerResetArgs {
    /// Milliseconds to hold the UART break asserted (default 5000).
    #[serde(default)]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct SendTextArgs {
    pub text: String,
    /// "" for US QWERTY (default), "de" for German.
    #[serde(default)]
    pub language: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct SendKeyArgs {
    pub key: String,
    #[serde(default)]
    pub ctrl: Option<bool>,
    #[serde(default)]
    pub shift: Option<bool>,
    #[serde(default)]
    pub alt: Option<bool>,
    #[serde(default)]
    pub meta: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct XyArgs {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct ClickArgs {
    /// "left" (default), "right", or "middle".
    #[serde(default)]
    pub button: Option<String>,
    #[serde(default)]
    pub x: Option<i32>,
    #[serde(default)]
    pub y: Option<i32>,
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct ScrollArgs {
    /// Positive = scroll down, negative = scroll up.
    pub amount: i32,
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct RelativeXyArgs {
    /// Pixel delta along X (positive = right). Decomposed into multiple HID
    /// reports if magnitude exceeds 127.
    pub dx: i32,
    /// Pixel delta along Y (positive = down).
    pub dy: i32,
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct ClickRelativeArgs {
    /// "left" (default), "right", or "middle".
    #[serde(default)]
    pub button: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct ScreenshotArgs {
    #[serde(default)]
    pub max_width: Option<u32>,
    #[serde(default)]
    pub max_height: Option<u32>,
    /// JPEG quality 1..100 (default 80).
    #[serde(default)]
    pub quality: Option<u8>,
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct MountIsoArgs {
    pub file: String,
    #[serde(default)]
    pub as_cdrom: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct DeleteIsoArgs {
    /// Filename (or path on the device) to delete.
    pub file: String,
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct UploadIsoArgs {
    /// Absolute path on *this* MCP server's filesystem. Must resolve inside
    /// the operator-configured --iso-dir.
    pub local_path: String,
}

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct TypeTextArgs {
    pub text: String,
}

// -------- helpers --------

fn to_mcp(e: Error) -> McpError {
    McpError::internal_error(e.to_string(), None)
}

fn text_ok(s: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![Content::text(s.into())])
}

fn json_content(v: &serde_json::Value) -> Result<CallToolResult, McpError> {
    let s = serde_json::to_string(v)
        .map_err(|e| McpError::internal_error(e.to_string(), None))?;
    Ok(CallToolResult::success(vec![Content::text(s)]))
}

# nanokvm-mcp

An [MCP](https://modelcontextprotocol.io/) server, written in Rust, that exposes
a [Sipeed NanoKVM](https://github.com/sipeed/NanoKVM) device as a set of tools
an AI assistant (Claude, etc.) can drive: keyboard, mouse, screenshots, ISO
storage, power, and basic device info.

## Status

- Tested against NanoKVM PCIe with firmware **2.3.6** (app), **v1.4.2** (image).
- Firmware ≥ 2.3.0 only. The WebSocket HID protocol changed substantially
  between 2.2.x and 2.3.x; this port speaks the newer binary format (HID-report
  frames) and does **not** attempt the older JSON-text format.
- Auth handles both Set-Cookie and JSON-body token responses. On 2.3.6 the
  device only returns the JWT in the body; the cookie fallback path is what
  actually carries the session.

## Build

```bash
git clone https://github.com/inahga/nanokvm-mcp.git
cd nanokvm-mcp
cargo install --path .
```

This puts `nanokvm-mcp` in `~/.cargo/bin/`. Requires Rust 1.85+ (edition 2024).

For a non-installed build use `cargo build --release` and find the binary in
`target/release/nanokvm-mcp`.

## Configuration

Every flag is also readable from an env var. Env vars are what the MCP-host
JSON config sets; the CLI flags are mostly for ad-hoc testing.

| Flag              | Env var                 | Default | Description                                              |
|-------------------|-------------------------|---------|----------------------------------------------------------|
| `--host`          | `NANOKVM_HOST`          | (req'd) | NanoKVM IP or hostname                                   |
| `--user`          | `NANOKVM_USER`          | `admin` | Web UI username                                          |
| `--pass`          | `NANOKVM_PASS`          | `admin` | Web UI password                                          |
| `--screen-width`  | `NANOKVM_SCREEN_WIDTH`  | `1920`  | Screen width in pixels (for absolute mouse mapping)      |
| `--screen-height` | `NANOKVM_SCREEN_HEIGHT` | `1080`  | Screen height in pixels                                  |
| `--https`         | `NANOKVM_HTTPS`         | `false` | Use HTTPS/WSS instead of HTTP/WS                         |
| `--verify-ssl`    | `NANOKVM_VERIFY_SSL`    | `true`  | Verify TLS certificates; set `false` for self-signed     |
| `--iso-dir`       | `NANOKVM_ISO_DIR`       | (unset) | Allowlisted directory for ISO uploads. Unset disables `nanokvm_upload_iso`. |

## Claude Desktop / Claude Code config

Drop this into `~/Library/Application Support/Claude/claude_desktop_config.json`
(macOS) / `%APPDATA%\Claude\claude_desktop_config.json` (Windows), or your
Claude Code MCP config:

```json
{
  "mcpServers": {
    "nanokvm": {
      "command": "nanokvm-mcp",
      "env": {
        "NANOKVM_HOST": "192.168.1.100"
      }
    }
  }
}
```

If `nanokvm-mcp` isn't on the host's `$PATH`, use the full path returned by
`cargo install`.

## Tools

The server registers ~28 tools. They're grouped here for navigability; the
real description for each (and the JSON schema the model sees) lives in
`src/server.rs`.

### Power

| Tool                  | What it does                                                   |
|-----------------------|----------------------------------------------------------------|
| `nanokvm_power`       | Short press / long-hold / reset via the ATX header             |
| `nanokvm_power_cycle` | Force-off, wait, power on — for boards without a reset line    |
| `nanokvm_led_status`  | Read `{pwr, hdd}` LED states                                   |

### Display / capture

| Tool                  | What it does                                                   |
|-----------------------|----------------------------------------------------------------|
| `nanokvm_screenshot`  | Capture a JPEG frame from the MJPEG stream                     |
| `nanokvm_hdmi_status` | HDMI enabled flag                                              |
| `nanokvm_hdmi_reset`  | Reset HDMI capture                                             |
| `nanokvm_hdmi_enable` | Enable HDMI capture                                            |
| `nanokvm_hdmi_disable`| Disable HDMI capture (screenshots will time out until enabled) |

### Keyboard

| Tool                | What it does                                                   |
|---------------------|----------------------------------------------------------------|
| `nanokvm_send_text` | Paste text via REST (≤1024 chars) — fast bulk input            |
| `nanokvm_type_text` | Type text char-by-char via WebSocket — slower, no length limit |
| `nanokvm_send_key`  | One key (with optional `ctrl`/`shift`/`alt`/`meta`)            |

### Mouse — pick the right family for the target

In an **OS** (Linux/Windows/macOS desktop, login manager, X/Wayland), use the
absolute tools. They take screen coordinates and teleport the cursor:

| Tool             | What it does                                |
|------------------|---------------------------------------------|
| `nanokvm_move`   | Move cursor to `(x, y)`                     |
| `nanokvm_click`  | Click button, optionally at `(x, y)` first  |
| `nanokvm_tap`    | Left-click at `(x, y)` (touchscreen analog) |
| `nanokvm_scroll` | Scroll the wheel                            |

In **pre-OS** (BIOS / UEFI setup, GRUB, install media before the kernel inits
HID), use the relative tools. BIOS firmware only implements the USB HID boot
mouse and ignores the absolute touchpad descriptor — absolute calls there will
appear to do small random moves rather than jump to the target:

| Tool                      | What it does                                |
|---------------------------|---------------------------------------------|
| `nanokvm_move_relative`   | Move cursor by signed `(dx, dy)`            |
| `nanokvm_click_relative`  | Click button at current position            |

### HID management

| Tool                | What it does                                  |
|---------------------|-----------------------------------------------|
| `nanokvm_reset_hid` | Reset the keyboard/mouse USB gadget           |
| `nanokvm_hid_mode`  | Read current HID mode (`normal` / `hid-only`) |

### Storage

| Tool                         | What it does                                              |
|------------------------------|-----------------------------------------------------------|
| `nanokvm_list_images`        | List ISOs in `/data` on the NanoKVM                       |
| `nanokvm_mounted_image`      | Currently mounted image                                   |
| `nanokvm_mount_iso`          | Mount an ISO as CD-ROM or USB disk                        |
| `nanokvm_unmount_iso`        | Unmount the mounted image                                 |
| `nanokvm_cdrom`              | Read the CD-ROM exposure flag                             |
| `nanokvm_delete_iso`         | Delete an ISO from `/data` (firmware ≥ 2.3.0)             |
| `nanokvm_iso_upload_enabled` | Is `/data` writable?                                      |
| `nanokvm_upload_iso`         | Multipart-upload a local ISO file (path must be inside `--iso-dir`) |

### Info

| Tool                | What it does                                  |
|---------------------|-----------------------------------------------|
| `nanokvm_info`      | IP, firmware, image, mdns name, device key    |
| `nanokvm_hardware`  | Hardware variant (e.g. `PCIE`)                |

## Operational notes

A few things the MCP `instructions` string also tells the model at session
start:

- **Cleanup.** If you mount an ISO, unmount it before ending. If you upload or
  fetch an ISO that won't be reused, delete it. `/data` is shared.
- **Power tools physically affect the host.** Confirm intent before calling
  `nanokvm_power` or `nanokvm_power_cycle`.
- **Two devices.** The NanoKVM is the controlling KVM; the target (the host
  it's plugged into) is a separate machine. They're not always in the same
  state.

## Project layout

```
nanokvm-mcp/
├── Cargo.toml
├── README.md
└── src/
    ├── main.rs       — env / CLI parsing, stdio MCP service
    ├── auth.rs       — CryptoJS-compatible AES-256-CBC login (EVP_BytesToKey)
    ├── hid.rs        — HID scancodes, modifier bits, key lookups
    ├── client.rs     — async client: REST + WebSocket + MJPEG screenshot
    ├── server.rs     — rmcp tool surface
    ├── ws.rs         — WebSocket connect helper
    └── error.rs      — typed error enum
```

## Troubleshooting

- **`ERROR: nanokvm api returned code -2: mount image failed`** — the host
  still holds `/dev/sr0` open. SSH in and `sudo umount /mnt`, then retry.
- **Cursor wanders instead of teleporting** — you're in a pre-OS environment.
  Switch to `nanokvm_move_relative` / `nanokvm_click_relative`.
- **Screenshots are stale / identical hashes** — the MJPEG `?n=1` endpoint
  returns the cached frame until the source pushes a new one. Static screens
  hold the cache. `nanokvm_hdmi_reset` flushes it (slow, ~2 s).
- **`mount image failed` on a crafted minimal ISO** — the kernel's mass-storage
  gadget rejects backing files that aren't real ISO 9660. Use a real ISO.
- **`delete image` returns 404** — your firmware is < 2.3.0. Upgrade, or
  delete via SSH (`rm /data/<name>.iso`).

## License

MIT — see the original upstream project for history.

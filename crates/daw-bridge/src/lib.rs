//! What Kumi's own DAW bridges (REAPER, Renoise) share: the loopback wire, its handshake and auth, discovery files,
//! and the lifecycle of the bridge files installed into each DAW. Ableton's bridge has its own crate and wire.
//!
//! # The wire, `kumi-daw/1`
//!
//! The component inside the DAW (REAPER's extension, Renoise's tool) is the server; Kumi's host connects to it.
//! The JSON test vectors in `tests/vectors/` pin every rule below for each implementation (this crate, the REAPER
//! extension through it, and the Renoise tool's Lua tests).
//!
//! ## Files
//!
//! - **Secret** ([`paths::BridgePaths::secret`], `<Kumi's folder>/bridges/<daw>/bridge.secret`): written by Kumi's
//!   lifecycle, owner-only, one line. Valid after stripping one trailing `\n` or `\r\n`: 32 to 512 bytes, each
//!   0x21..=0x7E ([`auth::Secret`]). The bridge reads it again for each connection.
//! - **Reference** ([`files::Reference`]): beside the bridge's files (REAPER `<resource>/UserPlugins/
//!   kumi-bridge-reference.json`, Renoise `<tool bundle>/bridge-reference.json`), naming the secret file, the
//!   discovery folder, the first port to try and the log folder. Without one the bridge doesn't serve.
//! - **Discovery** ([`files::Discovery`]): the bridge writes `<discovery folder>/<daw>-<pid>-<key>.json` once it
//!   listens, rewrites it when the project changes and removes it when it stops. `key` is the first 16 hex of the
//!   SHA-256 of the resource folder's path, so several instances never mix ([`files::discovery_key`]). It names the
//!   pid, port, protocol, bridge version, DAW version, resource folder, project and start time.
//!
//! ## Ports
//!
//! 127.0.0.1 only. REAPER 9770–9779, Renoise 9780–9789 ([`daw::Daw::ports`]): the bridge takes the first free one from
//! the reference's port and publishes it in its discovery file. Ableton keeps 9765 and 9766.
//!
//! ## Before auth
//!
//! - Peers other than loopback are closed at once ([`loopback::peer_allowed`]); at most 4 connections wait to
//!   authenticate and 4 are authenticated.
//! - The first byte must be `{`, so an HTTP request (a browser, a DNS-rebinding page) is closed without a reply.
//! - Lines are at most 1024 bytes ([`frame::MAX_PREAUTH_LINE_BYTES`]); the handshake must finish within 3 s.
//!
//! ## Handshake (host first; mutual HMAC-SHA256 challenge)
//!
//! ```text
//! host   → bridge  {"type":"hello","protocol":"kumi-daw/1","nonce":<cn>,"client":"daw-mcp-server/x"}
//! bridge → host    {"type":"challenge","nonce":<sn>,"proof":HMAC(secret,"kumi-daw/1\nserver\n"+cn+"\n"+sn)}
//! host   → bridge  {"type":"auth","proof":HMAC(secret,"kumi-daw/1\nclient\n"+cn+"\n"+sn)}
//! bridge → host    {"type":"welcome","session":…,"daw":…,"bridge":…,"dawVersion":…,"pid":…,"resource":…,
//!                   "project":…,"maxFrameBytes":…,"heartbeatMs":…,"methods":[…],"events":[…]}
//! ```
//!
//! Nonces are 32 lowercase hex characters, proofs 64 ([`auth::proof`]). The host checks the bridge's proof before it
//! sends its own, so a program squatting on the port learns nothing. Refusals are
//! `{"type":"refused","reason":"protocol"|"auth"|"limit"|"timeout"|"handshake"}` and then a close; a protocol
//! refusal names the bridge's protocol.
//!
//! ## After auth
//!
//! One JSON object per line, UTF-8, at most 64 MiB ([`frame::MAX_FRAME_BYTES`]); a trailing `\r` is dropped and empty
//! lines are skipped. A line too long, not UTF-8 or not a frame closes the connection; unknown frame types and fields
//! are ignored. Frames ([`wire::Frame`]):
//!
//! - request `{"type":"request","id":n,"method":…,"params":{…},"deadlineMs":unix ms?}`, ids increasing, at most 2^53−1;
//! - response `{"type":"response","id":n,"ok":true,"result":…}` or `{…,"ok":false,"error":{"code":…,"message":…}}`
//!   with the codes in [`wire::code`];
//! - event `{"type":"event","seq":n,"name":…,"data":…}`, numbered from 1 on each connection;
//! - heartbeat `{"type":"heartbeat","ms":n}` every `heartbeatMs`: after 3 missed the host calls the bridge busy;
//! - bye `{"type":"bye","reason":…}` before the bridge closes.
//!
//! Every bridge answers `ping` and `status` (its identity plus `busy`).

pub mod auth;
pub mod daw;
pub mod files;
pub mod frame;
pub mod handshake;
pub mod loopback;
pub mod owner;
pub mod paths;
pub mod wire;

pub use daw::Daw;

# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Voirc is a decentralized voice chat and file sharing application built in Rust. It uses an embedded TLS-secured IRC server for signaling and WebRTC for peer-to-peer audio communication.

## Commands

```bash
# Build
cargo build --release

# Run
cargo run --release

# Build with specific features
cargo build --release --features "upnp,clipboard"

# Linux dependencies (required for audio/TLS)
sudo apt install libasound2-dev libssl-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev
```

## Architecture

The application follows a layered architecture:

### Signaling Layer (`irc_server.rs`, `irc_client.rs`)
- Embedded IRC server handles peer discovery and chat
- TCP + TLS connections with SDP payloads fragmented into 512-byte IRC messages (`WRTC:[seq/total|id]payload`)
- Both server and client use custom `MaybeTlsStream` wrapper for plain/TLS handling

### WebRTC Layer (`webrtc_peer.rs`)
- Direct P2P audio and data connections
- Uses `webrtc` crate with Opus codec (48kHz mono)
- Data channels for file transfer (`FILE:name:size` header → 16KB chunks → `FILE_END`)
- Certificate pinning via fingerprint in connection strings

### Topology (`topology.rs`)
Two-tier star topology to reduce bandwidth:
- **Tier 1 (Superpeers)**: Host and moderators form a full mesh
- **Tier 2 (Peers)**: Regular users connect to one superpeer only
- Superpeers forward audio to connected peers (SFU-like behavior)

### Audio (`voice_mixer.rs`)
- Opus codec encoding/decoding via `opus` crate
- Software audio mixing with `1/sqrt(N)` normalization
- Ring buffer for cross-thread audio data passing

### Fallback Relay (`relay.rs`)
- TCP relay on host port + 1 for users behind strict NATs
- Custom wire format: `[1 byte nick_len][nick][2 bytes payload_len][payload]`

### Security (`tls.rs`)
- Self-signed TLS certificates generated at runtime
- Certificate fingerprint embedded in `voirc://` magic links for pinning

### UI (`gui.rs`)
- eframe/egui-based native GUI
- Manages connection states, chat, voice, and file transfers

## Key Types

- `AppState` (`state.rs`): Central state management for peers, channels, roles
- `UserConfig` (`config.rs`): User settings (display name, TURN servers)
- `Role` (`config.rs`): `Host`, `Moderator`, or `Peer` - determines topology position
- `ConnState` (`config.rs`): Connection state tracking
- `IrcEvent` (`irc_client.rs`): Events from IRC server (joins, leaves, messages, WebRTC signals)

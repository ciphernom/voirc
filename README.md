# Voirc

Decentralized voice chat and file sharing application. Uses an embedded, TLS-secured IRC server for signaling, establishing WebRTC connections for high-quality audio.

**Repository:** [https://github.com/ciphernom/voirc](https://github.com/ciphernom/voirc)

## Architecture

* **Signaling:** Embedded IRC server (TCP/TLS). Handles peer discovery and chat.
* **Media:** WebRTC (UDP/ICE) with a **Superpeer Topology** to reduce bandwidth usage.
* **Fallback:** Custom TCP Audio Relay for users behind strict NATs (when P2P fails).
* **Security:** Automatic self-signed certificate generation with **Certificate Pinning** via magic links.
* **Data:** WebRTC Data Channels for direct file transfer.

## Features

* **Self-Hosted:** Built-in IRCd allows hosting rooms without external infrastructure.
* **Resilient:** Automatically falls back to a TCP relay if peer-to-peer connection fails.
* **Secure:** Magic links include certificate fingerprints to prevent Man-in-the-Middle attacks.
* **Voice:** Low-latency, multi-peer voice mixing (Opus codec).
* **File Sharing:** Drag-and-drop transfer via direct P2P data channels.
* **Magic Links:** `voirc://` strings containing IP, port, security fingerprint, and channel config.

## Build & Run

Requires stable Rust toolchain.

### Dependencies (Linux)

Ensure development headers for ALSA, OpenSSL, and X11/Wayland are installed.

```bash
sudo apt install libasound2-dev libssl-dev libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev

```

### Build

```bash
git clone https://github.com/ciphernom/voirc
cd voirc
cargo build --release

```

### Run

```bash
cargo run --release

```

## Usage

### Hosting

1. Select **Host a Room**.
2. Define port (default: 6667) and channels.
3. Click **Start Server**.
4. Share the generated `voirc://` link.

*Note: The application attempts UPnP. If that fails, it will warn you, but the TCP Relay ensures friends can often still connect.*

### Joining

1. Select **Join a Room**.
2. Paste the `voirc://` link.
3. Click **Connect**.

### In-Call

* **Text:** Type in the bottom bar.
* **Voice:** Voice activity detection (VAD) is enabled by default.
* **Files:** Drag and drop files onto the window to broadcast to all connected peers.

## License

GNU GPL v3

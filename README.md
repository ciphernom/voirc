# Voirc

Decentralized voice chat and file sharing application. Uses an embedded IRC server for signaling and coordination, establishing WebRTC mesh networks for peer-to-peer audio and data transfer.

**Repository:** [https://github.com/ciphernom/voirc](https://github.com/ciphernom/voirc)

## Architecture

* **Signaling:** Embedded IRC server (TCP). Handles peer discovery, presence, and text chat.
* **Media:** WebRTC (UDP/ICE). Handles peer-to-peer audio streaming (Opus codec).
* **Data:** WebRTC Data Channels. Handles direct file transfer.
* **Networking:** Automatic UPnP/IGD port forwarding for host nodes.
* **GUI:** Immediate mode GUI using `egui`.

## Features

* **Self-Hosted:** Built-in IRCd allows hosting rooms without external infrastructure.
* **Voice:** Low-latency, multi-peer voice mixing.
* **File Sharing:** Drag-and-drop transfer via direct P2P data channels.
* **Magic Links:** Base64-encoded connection strings (`voirc://`) containing host IP, port, and channel configuration.
* **Text Chat:** Standard IRC channel communication.

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

*Note: The application attempts to open the port via UPnP. If UPnP fails, manual port forwarding is required.*

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

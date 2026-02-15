# Design & Architecture

## 1. Signaling Protocol (IRC Tunneling)

Voirc uses a standard IRC server as the signaling plane for WebRTC.

* **Discovery:** Peers join a specific IRC channel to discover IP addresses/Ports via standard `JOIN`/`NAMES` messages.
* **SDP Exchange:** WebRTC Offer/Answer and ICE candidates are serialized to JSON and sent via IRC `PRIVMSG`.
* **Fragmentation:** IRC message limits (512 bytes) require large SDP packets to be fragmented.
* **Format:** `WRTC:[seq/total|msg_id]payload`
* **Reassembly:** Receiver buffers fragments by `msg_id` until `total` is reached before triggering WebRTC negotiation.



## 2. Audio Architecture

Audio is handled via `cpal` (hardware I/O) and `opus` (compression).

* **Input (Capture):**
* Captured mono at 48kHz.
* VAD (Voice Activity Detection) based on RMS threshold (>0.01).
* Encoded via Opus (VoIP profile) and sent to the `mpsc` channel.


* **Output (Render):**
* Incoming packets are decoded to `f32` PCM.
* **Software Mixing:** Concurrent audio streams are summed and normalized by `1/sqrt(N)` to prevent clipping.



## 3. Network Topology

* **Mesh Network:** Every client establishes a direct WebRTC `PeerConnection` with every other client in the channel.
* **Transport:**
* **Signaling:** TCP (IRC).
* **Media/Data:** UDP (WebRTC/ICE).


* **NAT Traversal:** Uses STUN (Google/Ekiga public servers) and local UPnP/IGD for the hosting node.

## 4. Data Transfer

Files are **not** sent over IRC.

* **Mechanism:** WebRTC Data Channels (`ordered: true`).
* **Protocol:**
1. Header: `FILE:name:size`
2. Chunks: Base64 encoded payload.
3. Footer: `FILE_END`.



## 5. Magic Link Format

Configuration is shared via Base64-encoded JSON strings.

* **Schema:** `voirc://<base64_json>`
* **JSON Structure:**
```json
{
  "host": "1.2.3.4",
  "port": 6667,
  "channels": ["#lobby"]
}

```

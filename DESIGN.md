# Design

**Signaling**
Tunneled over standard IRC (TCP/TLS). Large SDP payloads are fragmented to fit the 512-byte IRC message limit using the format `WRTC:[seq/total|id]payload`.

**Topology**
Superpeer / Star Topology (Tiered).
- **Tier 1 (Superpeers):** Host and Moderators form a full mesh.
- **Tier 2 (Peers):** Regular users connect only to one Superpeer (Host/Mod).
- **Routing:** Superpeers act as SFUs, forwarding audio packets to other connected peers.

**Audio**
Opus codec (VoIP profile, 48kHz mono).
Mixing: Software summation. Normalized by `soft_clip(sample) = tanh(sample * 1.5)` applied to the output buffer to prevent clipping.

**Files**
Transferred via WebRTC Data Channels (ordered, reliable).
Protocol: `FILE:name:size` header → Raw binary chunks (16KB) → `FILE_END`.

**Network**
- **Host:** Auto-forwards port via UPnP (IGD). 
- **Relay:** Fallback TCP audio relay (running on host port + 1) for clients behind strict NATs where UDP/STUN fails.
- **Security:** Self-signed TLS certificates generated on the fly.
- **Config:** `voirc://` links are Base64-encoded JSON containing host, port, channels, relay port, and the **TLS Certificate Fingerprint** for pinning.

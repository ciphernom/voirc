# Design

**Signaling**
Tunneled over standard IRC (TCP). Large SDP payloads are fragmented to fit the 512-byte IRC message limit using the format `WRTC:[seq/total|id]payload`.

**Topology**
Full mesh. Connection collisions are avoided by nickname comparison: the lexicographically smaller nickname sends the Offer, the larger waits to Answer.
Will implement Plumtree (or similar) to assist with scaling in future versions.

**Audio**
Opus codec (VoIP profile, 48kHz mono).
Mixing: Software summation. Normalized by `1/sqrt(N)` to prevent clipping. Improvements are required. 

**Files**
Transferred via WebRTC Data Channels (ordered), not IRC.
Protocol: `FILE:name:size` header → Base64 chunks (12KB) → `FILE_END`.

**Network**
Host: Auto-forwards port via UPnP (IGD).
Peers: NAT traversal via public STUN (Google/Ekiga).
Config: `voirc://` links are Base64-encoded JSON containing host, port, and channels.

// src/topology.rs
//
// Superpeer topology for audio routing.
//
// The full-mesh problem: N peers = N*(N-1)/2 connections.
// At 10 users that's 45 connections. Unworkable.
//
// Solution: superpeers (host + mods) act as audio relays.
// Regular peers connect only to ONE superpeer.
// Superpeers connect to ALL other superpeers.
//
// Result: a two-tier star topology.
//   - Tier 1: superpeer mesh (small, 2-5 nodes)
//   - Tier 2: regular peers each attached to one superpeer
//
// Audio flow:
//   peer A → superpeer X → superpeer Y → peer B
//              ↓                ↓
//           peer C           peer D
//
// A superpeer forwards audio it receives from any source to all
// other connections (except back to the sender). This is an SFU.

use crate::config::Role;
use crate::state::AppState;
use std::sync::Arc;

/// Decides whether we should initiate a WebRTC connection to `target_nick`.
///
/// Returns true if we should connect, false if we should skip.
/// The `nickname < target` tiebreaker still applies on top of this
/// (only the lexicographically-lesser peer initiates the offer).
pub async fn should_connect_to(
    state: &Arc<AppState>,
    _our_nick: &str,
    our_role: Role,
    _target_nick: &str,
    target_role: Role,
) -> bool {
    // Superpeers connect to ALL other superpeers
    if our_role.is_superpeer() && target_role.is_superpeer() {
        return true;
    }

    // Regular peers connect to superpeers only
    if !our_role.is_superpeer() && target_role.is_superpeer() {
        return true;
    }

    // Superpeers accept connections from regular peers
    if our_role.is_superpeer() && !target_role.is_superpeer() {
        return true;
    }

    // Regular peer ↔ regular peer: only if no superpeers exist at all
    // (fallback to full mesh for small rooms with no mods)
    let superpeers = state.all_superpeers().await;
    if superpeers.is_empty() {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Role;
    use crate::state::AppState;
    use std::sync::Arc;

    // Helper is now async and does not start its own runtime
    async fn make_state_with_peers(peers: Vec<(&'static str, Role)>) -> Arc<AppState> {
        let state = AppState::new();
        for (nick, role) in peers {
            state.set_peer_role(nick, role).await;
        }
        state
    }

    #[tokio::test]
    async fn test_superpeer_to_superpeer_connect() {
        let state = make_state_with_peers(vec![
            ("alice", Role::Host),
            ("bob", Role::Mod),
        ]).await;

        // Alice (Host) should connect to Bob (Mod) - both superpeers
        let should_connect = should_connect_to(&state, "alice", Role::Host, "bob", Role::Mod).await;
        assert!(should_connect);
    }

    #[tokio::test]
    async fn test_peer_to_superpeer_connect() {
        let state = make_state_with_peers(vec![
            ("alice", Role::Host),
            ("bob", Role::Peer),
        ]).await;

        // Bob (Peer) should connect to Alice (Host) - peer to superpeer
        let should_connect = should_connect_to(&state, "bob", Role::Peer, "alice", Role::Host).await;
        assert!(should_connect);
    }

    #[tokio::test]
    async fn test_superpeer_accepts_peer_connection() {
        let state = make_state_with_peers(vec![
            ("alice", Role::Host),
            ("bob", Role::Peer),
        ]).await;

        // Alice (Host) should accept connection from Bob (Peer)
        let should_connect = should_connect_to(&state, "alice", Role::Host, "bob", Role::Peer).await;
        assert!(should_connect);
    }

    #[tokio::test]
    async fn test_peer_to_peer_no_connect_with_superpeer_present() {
        let state = make_state_with_peers(vec![
            ("alice", Role::Host),
            ("bob", Role::Peer),
            ("charlie", Role::Peer),
        ]).await;

        // Bob and Charlie (both peers) should NOT connect directly when superpeer exists
        let should_connect = should_connect_to(&state, "bob", Role::Peer, "charlie", Role::Peer).await;
        assert!(!should_connect);
    }

    #[tokio::test]
    async fn test_peer_to_peer_fallback_no_superpeers() {
        let state = make_state_with_peers(vec![
            ("alice", Role::Peer),
            ("bob", Role::Peer),
        ]).await;

        // When no superpeers exist, peers should connect directly (full mesh fallback)
        let should_connect = should_connect_to(&state, "alice", Role::Peer, "bob", Role::Peer).await;
        assert!(should_connect);
    }

    #[tokio::test]
    async fn test_mod_to_mod_connect() {
        let state = make_state_with_peers(vec![
            ("alice", Role::Mod),
            ("bob", Role::Mod),
        ]).await;

        // Both mods are superpeers, should connect
        let should_connect = should_connect_to(&state, "alice", Role::Mod, "bob", Role::Mod).await;
        assert!(should_connect);
    }

    #[tokio::test]
    async fn test_peer_to_mod_connect() {
        let state = make_state_with_peers(vec![
            ("alice", Role::Mod),
            ("bob", Role::Peer),
        ]).await;

        // Peer should connect to mod
        let should_connect = should_connect_to(&state, "bob", Role::Peer, "alice", Role::Mod).await;
        assert!(should_connect);
    }
}

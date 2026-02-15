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

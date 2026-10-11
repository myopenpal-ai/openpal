//! Multi-device sync (`docs/design/MULTI_DEVICE_SYNC.md`,
//! `docs/SYNC_PAIRING_PROTOCOL.md`). [`roster`] is the local, per-device
//! record of who else is in this device's sync group — CRUD only, no
//! signature-verification logic (that belongs to the pairing protocol
//! implementation, which uses `roster` as a building block, not the other
//! way around).

pub mod artifact_push_state;
pub mod artifact_sweep;
pub mod capture;
pub mod conversations;
pub mod envelope;
pub mod field_versions;
pub mod hlc;
pub mod key_state;
pub mod mailbox_client;
pub mod outbox;
pub mod pairing;
pub mod roster;

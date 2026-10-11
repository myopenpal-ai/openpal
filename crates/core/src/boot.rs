//! Pre-unlock bootstrap primitives (`docs/SPACES_AUTHORING.md` §9
//! "Bootstrap — `setup()` splits in two"). Both submodules are pure
//! libraries here — nothing in `crates/app` calls them yet. That wiring
//! (a real `BootState`, `setup()` actually split into a pre-unlock phase
//! that acquires [`lock`] and reads [`manifest`], and a post-unlock phase
//! that builds `AppState`) is a later, dedicated phase; see this crate's
//! `crypto` module for the keys these two will eventually gate.

pub mod factory_reset;
pub mod lock;
pub mod manifest;
pub mod orphan_engine;

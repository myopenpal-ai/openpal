//! No-op stub for platforms with no multicast-lock concept. Desktop's mDNS
//! discovery (`crate::remote::discovery`, and `openpal_core::remote::client
//! ::discovery::Browser` when a desktop-to-desktop pairing scenario uses
//! it) works without any special lock; see `wifi_lock_android.rs` for the
//! real bridge Android needs (many OEM Wi-Fi stacks otherwise drop
//! multicast receive — `WifiManager.createMulticastLock`'s own docs).

pub fn acquire() -> Result<(), String> {
    Ok(())
}

pub fn release() -> Result<(), String> {
    Ok(())
}

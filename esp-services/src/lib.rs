#![no_std]
#![feature(impl_trait_in_assoc_type)]

extern crate alloc;

/// DHCP server for assigning IP addresses to clients in Access Point mode.
pub mod dhcp;
/// Captive portal DNS hijacker that resolves all queries to the device IP.
pub mod dns;
/// HTTP server integration using picoserve.
pub mod http;
/// mDNS responder for local hostname resolution (e.g., esp-device.local).
pub mod mdns;

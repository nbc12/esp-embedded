#![no_std]
#![feature(impl_trait_in_assoc_type)]
#![allow(async_fn_in_trait)]

use heapless::String;

/// Represents a Station (STA) network configuration.
#[derive(Clone, Debug)]
pub struct StaNetwork {
    /// The SSID of the network.
    pub ssid: String<64>,
    /// The password, if any.
    pub password: Option<String<64>>,
}

/// Snapshot of the current WiFi status.
#[derive(Clone, Debug)]
pub struct WifiStatus {
    pub ap_enabled: bool,
    pub ap_ssid: Option<String<64>>,
    pub sta_enabled: bool,
    pub sta_networks: heapless::Vec<StaNetwork, 8>,
    pub hostname: Option<String<64>>,
    pub country_code: Option<String<64>>,
}

/// An async trait for reading WiFi configuration.
/// 
/// This trait allows components (like web servers or status monitors) to query
/// the current WiFi settings without needing to know the underlying storage mechanism.
pub trait WifiConfig<const N: usize>: Send + Sync {
    /// Returns true if the Access Point interface should be active.
    async fn is_ap_enabled(&self) -> bool;
    /// Returns the configured SSID for the Access Point.
    async fn ap_ssid(&self) -> Option<String<64>>;
    /// Returns the configured password for the Access Point.
    async fn ap_password(&self) -> Option<String<64>>;
    /// Stored AP gateway IP as a dotted-decimal string, e.g. `"192.168.2.1"`.
    async fn ap_gateway_ip(&self) -> Option<String<64>>;
    /// Returns true if the Station (client) interface should be active.
    async fn is_sta_enabled(&self) -> bool;
    /// Returns the list of saved Station networks.
    async fn sta_networks(&self) -> heapless::Vec<StaNetwork, N>;
    /// Returns the device hostname used for DHCP and mDNS.
    async fn hostname(&self) -> Option<String<64>>;
    /// Returns the 2-character ISO country code (e.g., "US", "DE").
    async fn country_code(&self) -> [u8; 2];
}

/// An async trait for modifying WiFi configuration.
/// 
/// This trait provides methods to update WiFi settings. Implementations usually
/// persist these changes to flash and may trigger a reconnection or radio restart.
pub trait WifiManager<const N: usize>: Send + Sync {
    /// Enable or disable the Access Point interface.
    async fn set_ap_enabled(&self, enabled: bool) -> bool;
    /// Update the Access Point SSID.
    async fn set_ap_ssid(&self, ssid: &str) -> bool;
    /// Update the Access Point password.
    async fn set_ap_password(&self, password: &str) -> bool;
    /// Configure both SSID and password for the Access Point at once.
    async fn configure_ap(&self, ssid: Option<&str>, password: Option<&str>) -> bool;
    /// Persist a new AP gateway IP. Takes effect after reboot.
    async fn set_ap_gateway_ip(&self, ip: &str) -> bool;

    /// Enable or disable the Station interface.
    async fn set_sta_enabled(&self, enabled: bool) -> bool;
    /// Add a new Station network to the saved list.
    async fn add_sta_network(&self, ssid: &str, password: Option<&str>) -> bool;
    /// Save a new STA network and immediately request a connection to it.
    async fn add_and_connect_sta_network(&self, ssid: &str, password: Option<&str>) -> bool;
    /// Remove a Station network by its index in the saved list.
    async fn remove_sta_network(&self, index: usize) -> bool;

    /// Set the 2-character ISO country code. Triggers a software reset on success.
    async fn set_country_code(&self, code: &str) -> bool;
    /// Set the device hostname. Triggers a software reset on success.
    async fn set_hostname(&self, hostname: &str) -> bool;
}

#![no_std]
#![feature(impl_trait_in_assoc_type)]
#![allow(async_fn_in_trait)]

extern crate alloc;

#[cfg(all(not(feature = "ap"), not(feature = "sta")))]
compile_error!("At least one of the `ap` or `sta` cargo features must be enabled.");

pub mod config;
pub mod driver;
pub mod tasks;

pub use config::FlashBackedWifiConfig;
pub use driver::WifiDriver;

pub use esp_system_api::{StaNetwork, WifiConfig, WifiManager, WifiStatus};

// ─── Control API ─────────────────────────────────────────────────────────────

pub use tasks::{notify_ap_toggled, notify_sta_toggled, request_ap_reconfig, request_connect};

// ─── WiFi Delegation Macro ───────────────────────────────────────────────────

/// Implement `WifiConfig<N>` and `WifiManager<N>` on a struct by delegating to an internal field.
/// 
/// This is useful when you have a global state struct that contains a `FlashBackedWifiConfig`.
/// Instead of manually implementing every method, this macro handles the boilerplate.
/// 
/// # Example
/// ```
/// struct GlobalState {
///     wifi: FlashBackedWifiConfig<8>,
/// }
/// 
/// impl_wifi_delegation!(GlobalState, wifi, 8);
/// ```
#[macro_export]
macro_rules! impl_wifi_delegation {
    ($ty:ty, $field:ident, $n:expr) => {
        impl $crate::WifiConfig<$n> for $ty {
            async fn is_ap_enabled(&self) -> bool {
                self.$field.is_ap_enabled().await
            }
            async fn ap_ssid(&self) -> ::core::option::Option<::heapless::String<64>> {
                self.$field.ap_ssid().await
            }
            async fn ap_password(&self) -> ::core::option::Option<::heapless::String<64>> {
                self.$field.ap_password().await
            }
            async fn ap_gateway_ip(&self) -> ::core::option::Option<::heapless::String<64>> {
                self.$field.ap_gateway_ip().await
            }
            async fn is_sta_enabled(&self) -> bool {
                self.$field.is_sta_enabled().await
            }
            async fn sta_networks(&self) -> ::heapless::Vec<$crate::StaNetwork, $n> {
                self.$field.sta_networks().await
            }
            async fn hostname(&self) -> ::core::option::Option<::heapless::String<64>> {
                self.$field.hostname().await
            }
            async fn country_code(&self) -> [u8; 2] {
                self.$field.country_code().await
            }
        }
        impl $crate::WifiManager<$n> for $ty {
            async fn set_ap_enabled(&self, enabled: bool) -> bool {
                self.$field.set_ap_enabled(enabled).await
            }
            async fn set_ap_ssid(&self, ssid: &str) -> bool {
                self.$field.set_ap_ssid(ssid).await
            }
            async fn set_ap_password(&self, password: &str) -> bool {
                self.$field.set_ap_password(password).await
            }
            async fn configure_ap(
                &self,
                ssid: ::core::option::Option<&str>,
                password: ::core::option::Option<&str>,
            ) -> bool {
                self.$field.configure_ap(ssid, password).await
            }
            async fn set_ap_gateway_ip(&self, ip: &str) -> bool {
                self.$field.set_ap_gateway_ip(ip).await
            }
            async fn set_sta_enabled(&self, enabled: bool) -> bool {
                self.$field.set_sta_enabled(enabled).await
            }
            async fn add_sta_network(
                &self,
                ssid: &str,
                password: ::core::option::Option<&str>,
            ) -> bool {
                self.$field.add_sta_network(ssid, password).await
            }
            async fn add_and_connect_sta_network(
                &self,
                ssid: &str,
                password: ::core::option::Option<&str>,
            ) -> bool {
                self.$field.add_and_connect_sta_network(ssid, password).await
            }
            async fn remove_sta_network(&self, index: usize) -> bool {
                self.$field.remove_sta_network(index).await
            }
            async fn set_country_code(&self, code: &str) -> bool {
                self.$field.set_country_code(code).await
            }
            async fn set_hostname(&self, hostname: &str) -> bool {
                self.$field.set_hostname(hostname).await
            }
        }
    };
}

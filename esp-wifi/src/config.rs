use core::ops::Range;

use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
use esp_persistent_config::{FlashStore, String};
use esp_system_api::{StaNetwork, WifiConfig, WifiManager};
use heapless::Vec;

use crate::tasks::{notify_ap_toggled, notify_sta_toggled, request_ap_reconfig, request_connect};

// ─── Flash key constants ──────────────────────────────────────────────────────

const KEY_AP_ENABLED: u8 = 0;
const KEY_AP_SSID: u8 = 1;
const KEY_AP_PASSWORD: u8 = 2;
const KEY_AP_GATEWAY_IP: u8 = 3;
const KEY_STA_ENABLED: u8 = 4;
const KEY_HOSTNAME: u8 = 5;
const KEY_COUNTRY_CODE: u8 = 6;

const fn sta_ssid_key(i: usize) -> u8 {
    10 + (i * 2) as u8
}
const fn sta_pw_key(i: usize) -> u8 {
    11 + (i * 2) as u8
}

// ─── RAM mirror ───────────────────────────────────────────────────────────────

struct WifiConfigData<const N: usize> {
    ap_enabled: bool,
    ap_ssid: Option<String<64>>,
    ap_password: Option<String<64>>,
    ap_gateway_ip: Option<String<64>>,
    sta_enabled: bool,
    hostname: Option<String<64>>,
    country_code: Option<String<64>>,
    sta_networks: Vec<StaNetwork, N>,
}

impl<const N: usize> WifiConfigData<N> {
    const fn new() -> Self {
        Self {
            ap_enabled: option_env!("AP_SSID").is_some(),
            ap_ssid: None,
            ap_password: None,
            ap_gateway_ip: None,
            sta_enabled: option_env!("STA_SSID").is_some(),
            hostname: None,
            country_code: None,
            sta_networks: Vec::new(),
        }
    }
}

// ─── FlashBackedWifiConfig ────────────────────────────────────────────────────

/// A thread-safe, flash-backed implementation of `WifiConfig` and `WifiManager`.
/// 
/// This struct maintains a RAM mirror of the WiFi configuration and syncs changes
/// to flash storage. On first boot (when flash is uninitialized), it populates
/// default values using compile-time environment variables:
/// 
/// * `AP_SSID` - If set, enables the AP and sets the SSID.
/// * `AP_PASSWORD` - Sets the AP password.
/// * `AP_GATEWAY_IP` - Sets the AP gateway IP.
/// * `STA_SSID` - If set, enables STA mode and adds this network to slot 0.
/// * `STA_PASSWORD` - Password for the `STA_SSID` network.
/// * `HOSTNAME` - Sets the default hostname.
/// * `WIFI_COUNTRY_CODE` - Sets the country code (defaults to "01" if not set).
pub struct FlashBackedWifiConfig<const N: usize> {
    data: Mutex<CriticalSectionRawMutex, WifiConfigData<N>>,
    store: FlashStore<esp_persistent_config::AsyncSharedFlash>,
}

impl<const N: usize> FlashBackedWifiConfig<N> {
    pub const fn new() -> Self {
        Self {
            data: Mutex::new(WifiConfigData::new()),
            store: FlashStore::new(),
        }
    }

    /// Initialise flash storage, populate RAM mirror, and write compile-time
    /// env-var defaults on first boot.
    pub async fn init(&self, flash_driver: esp_persistent_config::AsyncSharedFlash, range: Range<u32>) {
        self.store.init(flash_driver, range).await;

        // ── Load scalar fields ────────────────────────────────────────────────
        // Fetch bool fields first so we can tell whether they came from flash.
        let ap_enabled_fetched = self.store.fetch::<bool>(KEY_AP_ENABLED).await;
        let sta_enabled_fetched = self.store.fetch::<bool>(KEY_STA_ENABLED).await;
        {
            let mut data = self.data.lock().await;

            if let Some(v) = ap_enabled_fetched {
                data.ap_enabled = v;
            }
            if let Some(v) = self.store.fetch::<String<64>>(KEY_AP_SSID).await {
                data.ap_ssid = Some(v);
            }
            if let Some(v) = self.store.fetch::<String<64>>(KEY_AP_PASSWORD).await {
                data.ap_password = Some(v);
            }
            if let Some(v) = self.store.fetch::<String<64>>(KEY_AP_GATEWAY_IP).await {
                data.ap_gateway_ip = Some(v);
            }
            if let Some(v) = sta_enabled_fetched {
                data.sta_enabled = v;
            }
            if let Some(v) = self.store.fetch::<String<64>>(KEY_HOSTNAME).await {
                data.hostname = Some(v);
            }
            if let Some(v) = self.store.fetch::<String<64>>(KEY_COUNTRY_CODE).await {
                data.country_code = Some(v);
            }

            // ── Load STA network slots ────────────────────────────────────────
            for i in 0..N {
                if let Some(ssid) = self.store.fetch::<String<64>>(sta_ssid_key(i)).await {
                    if !ssid.is_empty() {
                        let password = self
                            .store
                            .fetch::<String<64>>(sta_pw_key(i))
                            .await
                            .filter(|s| !s.is_empty());
                        let _ = data.sta_networks.push(StaNetwork { ssid, password });
                    }
                }
            }
        }

        // ── Write compile-time env-var defaults (first boot only) ─────────────
        let (ap_enabled_none, sta_enabled_none, ap_ssid_none, ap_pw_none, ap_gw_none, sta_was_empty, hostname_none, cc_none) = {
            let data = self.data.lock().await;
            (
                ap_enabled_fetched.is_none(),
                sta_enabled_fetched.is_none(),
                data.ap_ssid.is_none(),
                data.ap_password.is_none(),
                data.ap_gateway_ip.is_none(),
                data.sta_networks.is_empty(),
                data.hostname.is_none(),
                data.country_code.is_none(),
            )
        };

        if ap_enabled_none {
            let default = option_env!("AP_SSID").is_some();
            self.store.store(KEY_AP_ENABLED, &default).await;
        }
        if sta_enabled_none {
            let default = option_env!("STA_SSID").is_some();
            self.store.store(KEY_STA_ENABLED, &default).await;
        }
        if ap_ssid_none {
            if let Some(v) = option_env!("AP_SSID") {
                if let Ok(s) = String::try_from(v) {
                    if self.store.store(KEY_AP_SSID, &s).await {
                        self.data.lock().await.ap_ssid = Some(s);
                    }
                }
            }
        }
        if ap_pw_none {
            if let Some(v) = option_env!("AP_PASSWORD") {
                if let Ok(s) = String::try_from(v) {
                    if self.store.store(KEY_AP_PASSWORD, &s).await {
                        self.data.lock().await.ap_password = Some(s);
                    }
                }
            }
        }
        if ap_gw_none {
            if let Some(v) = option_env!("AP_GATEWAY_IP") {
                if let Ok(s) = String::try_from(v) {
                    if self.store.store(KEY_AP_GATEWAY_IP, &s).await {
                        self.data.lock().await.ap_gateway_ip = Some(s);
                    }
                }
            }
        }
        if sta_was_empty {
            if let Some(ssid) = option_env!("STA_SSID") {
                if let Ok(ssid_s) = String::try_from(ssid) {
                    let pw = option_env!("STA_PASSWORD").and_then(|p| String::try_from(p).ok());
                    let pw_stored: String<64> = pw.clone().unwrap_or_default();
                    let ok = self.store.store(sta_ssid_key(0), &ssid_s).await
                        && self.store.store(sta_pw_key(0), &pw_stored).await;
                    if ok {
                        let mut data = self.data.lock().await;
                        let _ = data.sta_networks.push(StaNetwork {
                            ssid: ssid_s,
                            password: pw,
                        });
                        data.sta_enabled = true;
                        drop(data);
                        self.store.store(KEY_STA_ENABLED, &true).await;
                    }
                }
            }
        }
        if hostname_none {
            if let Some(v) = option_env!("HOSTNAME") {
                if let Ok(s) = String::try_from(v) {
                    if self.store.store(KEY_HOSTNAME, &s).await {
                        self.data.lock().await.hostname = Some(s);
                    }
                }
            }
        }
        if cc_none {
            let default = option_env!("WIFI_COUNTRY_CODE").unwrap_or("01");
            if let Ok(s) = String::try_from(default) {
                if self.store.store(KEY_COUNTRY_CODE, &s).await {
                    self.data.lock().await.country_code = Some(s);
                }
            }
        }
    }

    async fn write_sta_slot(
        &self,
        i: usize,
        ssid: &String<64>,
        password: Option<&String<64>>,
    ) -> bool {
        let pw: String<64> = password.cloned().unwrap_or_default();
        self.store.store(sta_ssid_key(i), ssid).await
            && self.store.store(sta_pw_key(i), &pw).await
    }
}

// ─── WifiConfig impl ──────────────────────────────────────────────────────────

impl<const N: usize> WifiConfig<N> for FlashBackedWifiConfig<N> {
    async fn is_ap_enabled(&self) -> bool {
        self.data.lock().await.ap_enabled
    }

    async fn ap_ssid(&self) -> Option<String<64>> {
        self.data.lock().await.ap_ssid.clone()
    }

    async fn ap_password(&self) -> Option<String<64>> {
        self.data.lock().await.ap_password.clone()
    }

    async fn ap_gateway_ip(&self) -> Option<String<64>> {
        self.data.lock().await.ap_gateway_ip.clone()
    }

    async fn is_sta_enabled(&self) -> bool {
        self.data.lock().await.sta_enabled
    }

    async fn sta_networks(&self) -> Vec<StaNetwork, N> {
        self.data.lock().await.sta_networks.clone()
    }

    async fn hostname(&self) -> Option<String<64>> {
        self.data.lock().await.hostname.clone()
    }

    async fn country_code(&self) -> [u8; 2] {
        let data = self.data.lock().await;
        let code = data.country_code.as_deref().unwrap_or("01");
        let b = code.as_bytes();
        [
            b.first().copied().unwrap_or(b'0'),
            b.get(1).copied().unwrap_or(b'1'),
        ]
    }
}

// ─── WifiManager impl ─────────────────────────────────────────────────────────

impl<const N: usize> WifiManager<N> for FlashBackedWifiConfig<N> {
    async fn set_ap_enabled(&self, enabled: bool) -> bool {
        if self.store.store(KEY_AP_ENABLED, &enabled).await {
            self.data.lock().await.ap_enabled = enabled;
            notify_ap_toggled();
            true
        } else {
            false
        }
    }

    async fn set_ap_ssid(&self, ssid: &str) -> bool {
        if let Ok(s) = String::try_from(ssid) {
            if self.store.store(KEY_AP_SSID, &s).await {
                self.data.lock().await.ap_ssid = Some(s);
                request_ap_reconfig();
                return true;
            }
        }
        false
    }

    async fn set_ap_password(&self, password: &str) -> bool {
        if let Ok(p) = String::try_from(password) {
            if self.store.store(KEY_AP_PASSWORD, &p).await {
                self.data.lock().await.ap_password = Some(p);
                request_ap_reconfig();
                return true;
            }
        }
        false
    }

    async fn configure_ap(&self, ssid: Option<&str>, password: Option<&str>) -> bool {
        let ssid_ok = match ssid {
            Some(s) => match String::try_from(s) {
                Ok(s) => {
                    let ok = self.store.store(KEY_AP_SSID, &s).await;
                    if ok {
                        self.data.lock().await.ap_ssid = Some(s);
                    }
                    ok
                }
                Err(_) => false,
            },
            None => true,
        };
        let pw_ok = match password {
            Some(p) => match String::try_from(p) {
                Ok(p) => {
                    let ok = self.store.store(KEY_AP_PASSWORD, &p).await;
                    if ok {
                        self.data.lock().await.ap_password = Some(p);
                    }
                    ok
                }
                Err(_) => false,
            },
            None => true,
        };
        if ssid_ok && pw_ok {
            request_ap_reconfig();
            true
        } else {
            false
        }
    }

    async fn set_ap_gateway_ip(&self, ip: &str) -> bool {
        if let Ok(s) = String::try_from(ip) {
            if self.store.store(KEY_AP_GATEWAY_IP, &s).await {
                self.data.lock().await.ap_gateway_ip = Some(s);
                esp_hal::system::software_reset();
            }
        }
        false
    }

    async fn set_sta_enabled(&self, enabled: bool) -> bool {
        if self.store.store(KEY_STA_ENABLED, &enabled).await {
            self.data.lock().await.sta_enabled = enabled;
            notify_sta_toggled();
            true
        } else {
            false
        }
    }

    async fn add_sta_network(&self, ssid: &str, password: Option<&str>) -> bool {
        let Ok(ssid_s) = String::try_from(ssid) else {
            return false;
        };
        let pw_s: Option<String<64>> = match password {
            Some(p) => match String::try_from(p) {
                Ok(s) => Some(s),
                Err(_) => return false,
            },
            None => None,
        };

        let index = {
            let data = self.data.lock().await;
            if data.sta_networks.is_full() {
                return false;
            }
            data.sta_networks.len()
        };

        if self.write_sta_slot(index, &ssid_s, pw_s.as_ref()).await {
            let _ = self
                .data
                .lock()
                .await
                .sta_networks
                .push(StaNetwork { ssid: ssid_s, password: pw_s });
            true
        } else {
            false
        }
    }

    async fn add_and_connect_sta_network(&self, ssid: &str, password: Option<&str>) -> bool {
        if self.add_sta_network(ssid, password).await {
            let idx = self.data.lock().await.sta_networks.len().saturating_sub(1);
            request_connect(idx);
            true
        } else {
            false
        }
    }

    async fn remove_sta_network(&self, index: usize) -> bool {
        let networks = {
            let mut data = self.data.lock().await;
            if index >= data.sta_networks.len() {
                return false;
            }
            data.sta_networks.remove(index);
            data.sta_networks.clone()
        };

        for i in 0..N {
            let (ssid, pw) = if i < networks.len() {
                let n = &networks[i];
                (n.ssid.clone(), n.password.clone())
            } else {
                (String::default(), None)
            };
            if !self.write_sta_slot(i, &ssid, pw.as_ref()).await {
                return false;
            }
        }
        true
    }

    async fn set_country_code(&self, code: &str) -> bool {
        if code.len() != 2 || !code.is_ascii() {
            return false;
        }
        if let Ok(c) = String::try_from(code) {
            if self.store.store(KEY_COUNTRY_CODE, &c).await {
                self.data.lock().await.country_code = Some(c);
                esp_hal::system::software_reset();
            }
        }
        false
    }

    async fn set_hostname(&self, hostname: &str) -> bool {
        if let Ok(h) = String::try_from(hostname) {
            if self.store.store(KEY_HOSTNAME, &h).await {
                self.data.lock().await.hostname = Some(h);
                esp_hal::system::software_reset();
            }
        }
        false
    }
}

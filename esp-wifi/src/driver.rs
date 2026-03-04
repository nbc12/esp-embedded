use core::net::Ipv4Addr;
#[cfg(feature = "ap")]
use alloc::string::ToString;
#[cfg(feature = "sta")]
use core::str::FromStr;

pub use embassy_executor::Spawner;
pub use embassy_net::{Config, DhcpConfig, Ipv4Cidr, Stack, StackResources, StaticConfigV4};
pub use esp_hal::peripherals::WIFI;
pub use esp_radio::wifi;
use esp_radio::wifi::WifiController;

use crate::{WifiConfig, tasks::net_task};

pub const AP_SOCKET_COUNT: usize = 8;
pub const STA_SOCKET_COUNT: usize = 4;

#[derive(Clone, Copy)]
pub struct WifiStacks {
    pub ap: Option<Stack<'static>>,
    pub sta: Option<Stack<'static>>,
}

pub struct WifiResources {
    #[cfg(feature = "ap")]
    pub ap: StackResources<AP_SOCKET_COUNT>,
    #[cfg(feature = "sta")]
    pub sta: StackResources<STA_SOCKET_COUNT>,
}

impl WifiResources {
    pub fn new() -> Self {
        Self {
            #[cfg(feature = "ap")]
            ap: StackResources::new(),
            #[cfg(feature = "sta")]
            sta: StackResources::new(),
        }
    }
}

pub struct WifiDriver;

impl WifiDriver {
    /// Initialize the WiFi stacks.
    ///
    /// Which stacks are created is determined by the `ap` and `sta` cargo
    /// features. Both fields of the returned `WifiStacks` are `Option`; a
    /// field is `Some` only when its corresponding feature is enabled.
    pub async fn init<const N: usize, C: WifiConfig<N>>(
        radio_controller: &'static mut esp_radio::Controller<'static>,
        wifi_peripheral: WIFI<'static>,
        gw_ip: Ipv4Addr,
        seed: u64,
        config_provider: &C,
        resources: &'static mut WifiResources,
        spawner: &Spawner,
    ) -> (WifiStacks, WifiController<'static>) {
        let config = wifi::Config::default();
        let (mut wifi_controller, interfaces) =
            wifi::new(radio_controller, wifi_peripheral, config).unwrap();

        wifi_controller
            .set_power_saving(wifi::PowerSaveMode::None)
            .unwrap();

        // ── Build initial ModeConfig ──────────────────────────────────────────
        #[cfg(feature = "ap")]
        let ap_ssid_opt = config_provider.ap_ssid().await;
        #[cfg(feature = "ap")]
        let ap_pw_opt = config_provider.ap_password().await;

        #[cfg(feature = "ap")]
        let ap_conf = {
            let mut conf = wifi::AccessPointConfig::default()
                .with_ssid(ap_ssid_opt.as_deref().unwrap_or("esp-ap").to_string());
            if let Some(pw) = &ap_pw_opt {
                conf = conf
                    .with_password(pw.to_string())
                    .with_auth_method(wifi::AuthMethod::Wpa2Personal);
            }
            conf
        };

        #[cfg(all(feature = "ap", feature = "sta"))]
        let initial_config = wifi::ModeConfig::ApSta(
            wifi::ClientConfig::default().with_ssid(alloc::string::String::new()),
            ap_conf,
        );
        #[cfg(all(feature = "ap", not(feature = "sta")))]
        let initial_config = wifi::ModeConfig::AccessPoint(ap_conf);
        #[cfg(all(not(feature = "ap"), feature = "sta"))]
        let initial_config = wifi::ModeConfig::Client(
            wifi::ClientConfig::default().with_ssid(alloc::string::String::new()),
        );
        #[cfg(all(not(feature = "ap"), not(feature = "sta")))]
        let initial_config = wifi::ModeConfig::None;

        if !matches!(initial_config, wifi::ModeConfig::None) {
            wifi_controller.set_config(&initial_config).unwrap();
            wifi_controller.start_async().await.unwrap();
            log::info!("[WiFi] radio started");
        }

        // ── Build network stack configs ───────────────────────────────────────
        #[cfg(feature = "ap")]
        let ap_net_cfg = Config::ipv4_static(StaticConfigV4 {
            address: Ipv4Cidr::new(gw_ip, 24),
            gateway: Some(gw_ip),
            dns_servers: Default::default(),
        });

        #[cfg(not(feature = "ap"))]
        let _ = gw_ip;

        #[cfg(feature = "sta")]
        let sta_net_cfg = {
            let mut dhcp_config = DhcpConfig::default();
            if let Some(hn_str) = config_provider.hostname().await {
                if let Ok(hn) = heapless::String::<32>::from_str(hn_str.as_str()) {
                    dhcp_config.hostname = Some(hn);
                }
            }
            Config::dhcpv4(dhcp_config)
        };

        // ── Create embassy-net stacks ─────────────────────────────────────────
        #[cfg(feature = "ap")]
        let (ap_stack, ap_runner) =
            embassy_net::new(interfaces.ap, ap_net_cfg, &mut resources.ap, seed);
        #[cfg(not(feature = "ap"))]
        drop(interfaces.ap);

        #[cfg(feature = "sta")]
        let (sta_stack, sta_runner) =
            embassy_net::new(interfaces.sta, sta_net_cfg, &mut resources.sta, seed);
        #[cfg(not(feature = "sta"))]
        drop(interfaces.sta);

        #[cfg(feature = "ap")]
        spawner.spawn(net_task(ap_runner)).ok();
        #[cfg(feature = "sta")]
        spawner.spawn(net_task(sta_runner)).ok();

        (
            WifiStacks {
                ap: {
                    #[cfg(feature = "ap")]
                    { Some(ap_stack) }
                    #[cfg(not(feature = "ap"))]
                    { None }
                },
                sta: {
                    #[cfg(feature = "sta")]
                    { Some(sta_stack) }
                    #[cfg(not(feature = "sta"))]
                    { None }
                },
            },
            wifi_controller,
        )
    }
}

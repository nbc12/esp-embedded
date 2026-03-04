use alloc::string::ToString;
use embassy_net::{Runner, Stack};
use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, signal::Signal};
#[cfg(feature = "sta")]
use embassy_time::{Duration, Timer};
use esp_radio::wifi::{ModeConfig, WifiController, WifiDevice};
#[cfg(feature = "ap")]
use esp_radio::wifi::{AccessPointConfig, AuthMethod};
#[cfg(feature = "sta")]
use esp_radio::wifi::{ClientConfig, WifiEvent};

use crate::WifiConfig;

// ─── Signals ─────────────────────────────────────────────────────────────────

pub static CONNECT_REQUEST: Signal<CriticalSectionRawMutex, usize> = Signal::new();
pub static AP_RECONFIG: Signal<CriticalSectionRawMutex, ()> = Signal::new();
pub static STA_TOGGLED: Signal<CriticalSectionRawMutex, ()> = Signal::new();

pub fn request_connect(index: usize) {
    CONNECT_REQUEST.signal(index);
}
pub fn request_ap_reconfig() {
    AP_RECONFIG.signal(());
}
pub fn notify_ap_toggled() {
    AP_RECONFIG.signal(());
}
pub fn notify_sta_toggled() {
    STA_TOGGLED.signal(());
}

// ─── Tasks ────────────────────────────────────────────────────────────────────

#[embassy_executor::task(pool_size = 2)]
pub async fn net_task(mut runner: Runner<'static, WifiDevice<'static>>) {
    runner.run().await
}

// ─── State machine (STA feature only) ────────────────────────────────────────

#[cfg(feature = "sta")]
#[derive(Clone, Copy, Debug)]
enum ConnState {
    /// STA interface present but `sta_enabled = false`.
    StaDisabled,
    /// `sta_enabled = true` but no saved networks yet.
    NoNetworks,
    /// Attempting to connect to `networks[idx]`.
    Connecting(usize),
    /// Connected to `networks[idx]`.
    Connected(#[allow(dead_code)] usize),
}

#[cfg(feature = "sta")]
enum StateEvent {
    Timeout,
    ConnOk,
    ConnFail,
    Disconnected,
}

#[cfg(feature = "sta")]
async fn compute_state<const N: usize>(
    config: &impl WifiConfig<N>,
    net_idx: usize,
) -> ConnState {
    if !config.is_sta_enabled().await {
        return ConnState::StaDisabled;
    }
    let networks = config.sta_networks().await;
    if networks.is_empty() {
        return ConnState::NoNetworks;
    }
    ConnState::Connecting(net_idx.min(networks.len() - 1))
}

#[cfg(feature = "sta")]
async fn state_action(
    state: ConnState,
    controller: &mut WifiController<'static>,
) -> StateEvent {
    match state {
        ConnState::StaDisabled => core::future::pending().await,
        ConnState::NoNetworks => {
            Timer::after(Duration::from_millis(30_000)).await;
            StateEvent::Timeout
        }
        ConnState::Connecting(_) => match controller.connect_async().await {
            Ok(_) => StateEvent::ConnOk,
            Err(_) => StateEvent::ConnFail,
        },
        ConnState::Connected(_) => {
            controller
                .wait_for_events(WifiEvent::StaDisconnected.into(), false)
                .await;
            StateEvent::Disconnected
        }
    }
}

// ─── Connection loop ──────────────────────────────────────────────────────────

/// Full AP+STA connection management loop (requires `sta` feature).
#[cfg(feature = "sta")]
pub async fn run_connection<const N: usize, C: WifiConfig<N>>(
    mut controller: WifiController<'static>,
    config: &'static C,
) {
    use embassy_futures::select::{select4, Either4};

    log::info!("[WiFi] connection loop started");

    let mut net_idx: usize = 0;
    let mut state = compute_state(config, net_idx).await;
    log::info!("[WiFi] initial state: {:?}", state);

    // If STA is disabled, switch to AP-only mode so beaconing works correctly.
    if matches!(state, ConnState::StaDisabled) {
        apply_ap_reconfig(&mut controller, config).await;
    }

    loop {
        log::info!("[WiFi] state: {:?}", state);

        if let ConnState::Connecting(idx) = state {
            let networks = config.sta_networks().await;
            if let Some(network) = networks.get(idx) {
                log::info!("[WiFi] STA: trying [{idx}] \"{}\"...", network.ssid.as_str());
                let mut sta_conf =
                    ClientConfig::default().with_ssid(network.ssid.to_string());
                if let Some(pw) = &network.password {
                    sta_conf = sta_conf.with_password(pw.to_string());
                }
                let cfg = build_mode_config(Some(sta_conf), config).await;
                if let Err(e) = controller.set_config(&cfg) {
                    log::warn!("[WiFi] STA: set_config failed ({e:?})");
                }
            }
        }

        match select4(
            AP_RECONFIG.wait(),
            STA_TOGGLED.wait(),
            CONNECT_REQUEST.wait(),
            state_action(state, &mut controller),
        )
        .await
        {
            Either4::First(_) => {
                log::info!("[WiFi] AP: reconfig signal");
                if matches!(state, ConnState::Connected(_)) {
                    controller.disconnect_async().await.ok();
                }
                apply_ap_reconfig(&mut controller, config).await;
            }

            Either4::Second(_) => {
                log::info!("[WiFi] STA: toggled");
                if matches!(state, ConnState::Connected(_)) {
                    controller.disconnect_async().await.ok();
                }
                apply_ap_reconfig(&mut controller, config).await;
                net_idx = 0;
            }

            Either4::Third(idx) => {
                log::info!("[WiFi] STA: connect request for [{idx}]");
                if matches!(state, ConnState::Connected(_)) {
                    controller.disconnect_async().await.ok();
                }
                net_idx = idx;
            }

            Either4::Fourth(event) => match event {
                StateEvent::Timeout => {
                    log::warn!("[WiFi] STA: no networks saved — waiting 30 s");
                }
                StateEvent::ConnOk => {
                    log::info!("[WiFi] STA: connected (index {net_idx})");
                    state = ConnState::Connected(net_idx);
                    continue;
                }
                StateEvent::ConnFail => {
                    log::warn!("[WiFi] STA: connect failed — trying next");
                    let networks = config.sta_networks().await;
                    if !networks.is_empty() {
                        net_idx = (net_idx + 1) % networks.len();
                    }
                    Timer::after(Duration::from_millis(5_000)).await;
                }
                StateEvent::Disconnected => {
                    log::warn!("[WiFi] STA: disconnected — reconnecting in 5 s");
                    net_idx = 0;
                    Timer::after(Duration::from_millis(5_000)).await;
                }
            },
        }

        state = compute_state(config, net_idx).await;
    }
}

/// AP-only connection loop when `sta` feature is not enabled.
#[cfg(not(feature = "sta"))]
pub async fn run_connection<const N: usize, C: WifiConfig<N>>(
    mut controller: WifiController<'static>,
    config: &'static C,
) {
    log::info!("[WiFi] connection loop started (AP-only)");
    loop {
        AP_RECONFIG.wait().await;
        log::info!("[WiFi] AP: reconfig signal");
        apply_ap_reconfig(&mut controller, config).await;
    }
}

#[embassy_executor::task]
pub async fn log_sta_ip(stack: Stack<'static>) {
    stack.wait_config_up().await;
    if let Some(cfg) = stack.config_v4() {
        let ip = cfg.address.address();
        log::info!("[WiFi] STA IP: {ip} — browse http://{ip}/");
    }
}

// ─── Private helpers ──────────────────────────────────────────────────────────

#[cfg(feature = "ap")]
async fn build_ap_conf<const N: usize>(config: &impl WifiConfig<N>) -> AccessPointConfig {
    let ssid = config.ap_ssid().await;
    let pw = config.ap_password().await;
    let mut conf = AccessPointConfig::default()
        .with_ssid(ssid.as_deref().unwrap_or("esp-ap").to_string());
    if let Some(p) = pw {
        conf = conf
            .with_password(p.to_string())
            .with_auth_method(AuthMethod::Wpa2Personal);
    }
    conf
}

// build_mode_config is only needed when STA is present (called during connection loop).
#[cfg(feature = "sta")]
async fn build_mode_config<const N: usize>(
    sta_conf: Option<ClientConfig>,
    config: &impl WifiConfig<N>,
) -> ModeConfig {
    #[cfg(feature = "ap")]
    let ap_active = config.is_ap_enabled().await;

    #[cfg(feature = "ap")]
    {
        if ap_active {
            ModeConfig::ApSta(sta_conf.unwrap(), build_ap_conf(config).await)
        } else {
            ModeConfig::Client(sta_conf.unwrap())
        }
    }
    #[cfg(not(feature = "ap"))]
    {
        let _ = config;
        ModeConfig::Client(sta_conf.unwrap())
    }
}

async fn apply_ap_reconfig<const N: usize>(
    controller: &mut WifiController<'static>,
    config: &impl WifiConfig<N>,
) {
    log::info!("[WiFi] AP: applying config change — radio stopping...");
    if let Err(e) = controller.stop_async().await {
        log::error!("[WiFi] AP: stop failed ({e:?})");
        return;
    }

    #[cfg(all(feature = "ap", feature = "sta"))]
    let cfg = if !config.is_sta_enabled().await {
        if config.is_ap_enabled().await {
            ModeConfig::AccessPoint(build_ap_conf(config).await)
        } else {
            ModeConfig::None
        }
    } else {
        let dummy_sta = ClientConfig::default().with_ssid(alloc::string::String::new());
        build_mode_config(Some(dummy_sta), config).await
    };

    #[cfg(all(feature = "ap", not(feature = "sta")))]
    let cfg = if config.is_ap_enabled().await {
        ModeConfig::AccessPoint(build_ap_conf(config).await)
    } else {
        ModeConfig::None
    };

    #[cfg(all(not(feature = "ap"), feature = "sta"))]
    let cfg = {
        let dummy_sta = ClientConfig::default().with_ssid(alloc::string::String::new());
        build_mode_config(Some(dummy_sta), config).await
    };

    #[cfg(all(not(feature = "ap"), not(feature = "sta")))]
    let cfg = {
        let _ = config;
        ModeConfig::None
    };

    if matches!(cfg, ModeConfig::None) {
        log::info!("[WiFi] AP: disabled — radio stopped, not restarting.");
        return;
    }
    let cfg_name = match &cfg {
        ModeConfig::None => "None",
        ModeConfig::AccessPoint(_) => "AccessPoint",
        ModeConfig::Client(_) => "Client",
        ModeConfig::ApSta(_, _) => "ApSta",
        _ => "Other",
    };
    log::info!("[WiFi] AP: restarting as {cfg_name}");
    if let Err(e) = controller.set_config(&cfg) {
        log::error!("[WiFi] AP: set_config failed ({e:?})");
        return;
    }
    match controller.start_async().await {
        Ok(_) => log::info!("[WiFi] AP: restarted with new config."),
        Err(e) => log::error!("[WiFi] AP: start failed ({e:?})"),
    }
}

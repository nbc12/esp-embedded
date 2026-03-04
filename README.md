# esp-embedded

Reusable `no_std` Embassy crates for ESP32 firmware projects.

| Crate | Purpose |
|---|---|
| `esp-system-api` | `WifiConfig<N>` / `WifiManager<N>` traits — no HAL deps |
| `esp-session-mgmt` | LRU session store, cookie formatting utilities |
| `esp-persistent-config` | Flash KV store (`FlashStore`), `define_config!` macro |
| `esp-wifi` | `FlashBackedWifiConfig<N>`, WiFi driver init, connection state machine |
| `esp-services` | DHCP server, DNS (captive portal), mDNS |
| `esp-web-utils` | `AppState` / `OtaWriter` traits, `Authorized` + `OtaFailed` extractors, challenge-response login handler |

---

## Wiring it all together in `firmware/src/main.rs`

Below is the complete integration pattern. Each section is explained in order.

### 1. Flash partition constants (`config.rs`)

```rust
pub const MAX_STA_NETWORKS: usize = 8;
pub const FLASH_RANGE: core::ops::Range<u32> = 0x3E0000..(0x3E0000 + 0x8000);
```

`FLASH_RANGE` must point to a dedicated config partition in your `partitions.csv`. Both
`FlashBackedWifiConfig` and any `define_config!` stores share the same flash region via the
shared mutex — they use different key namespaces so they do not collide.

---

### 2. Shared flash mutex

All flash access goes through a single `CriticalSectionRawMutex`-guarded `Option<FlashStorage>`.
This lets both async (WiFi config init) and blocking (OTA) code share the same peripheral safely.

```rust
static SHARED_FLASH: Mutex<CriticalSectionRawMutex, Option<esp_storage::FlashStorage<'static>>>
    = Mutex::new(None);
```

The `unsafe` transmute below is required to satisfy the `'static` bound that
`AsyncSharedFlash` / `BlockingSharedFlash` expect. It is sound because `SHARED_FLASH` itself
is `'static`.

```rust
// Async handle — used during init and by FlashBackedWifiConfig
let async_flash = unsafe {
    let mutex = core::mem::transmute::<
        &Mutex<CriticalSectionRawMutex, Option<FlashStorage<'static>>>,
        &'static Mutex<CriticalSectionRawMutex, FlashStorage<'static>>,
    >(&SHARED_FLASH);
    esp_persistent_config::AsyncSharedFlash::new(mutex)
};

// Blocking handle — used at runtime by OTA
fn blocking_flash(&self) -> esp_persistent_config::BlockingSharedFlash {
    unsafe {
        let mutex = core::mem::transmute::<...>(&SHARED_FLASH);
        esp_persistent_config::BlockingSharedFlash::new(mutex)
    }
}
```

---

### 3. `define_config!` — flash-backed key/value config

Defines a named struct whose fields are persisted to flash. On first boot the defaults are
written; on subsequent boots the stored values are loaded into the in-RAM mirror.

```rust
esp_persistent_config::define_config! {
    struct AuthStore {
        username: heapless::String<32> = 100, heapless::String::try_from("admin").unwrap();
        password: heapless::String<32> = 101, heapless::String::try_from("admin").unwrap();
    }
}
```

The macro emits a module-level `static CONFIG: Mutex<..., AuthStore>` and an async
`init_config(flash, range)` function. Keys are `u8` values — choose ones that do not overlap
with `FlashBackedWifiConfig`'s key space (keys 0–49 are reserved by that crate; use 100+ for
your own stores).

Initialize it in `main` **after** putting flash into `SHARED_FLASH`:

```rust
*SHARED_FLASH.lock().await = Some(esp_storage::FlashStorage::new(peripherals.FLASH));
init_config(async_flash, config::FLASH_RANGE).await;
```

Access the in-RAM mirror anywhere:

```rust
CONFIG.lock().await.username.clone()
```

---

### 4. `FlashBackedWifiConfig` and `impl_wifi_delegation!`

`FlashBackedWifiConfig<N>` is a `const fn new()` type — safe to use in a `static`.
It implements both `WifiConfig<N>` (reads) and `WifiManager<N>` (writes) directly.

```rust
pub struct GlobalState {
    pub wifi: FlashBackedWifiConfig<{ config::MAX_STA_NETWORKS }>,
    // ...
}

static STATE: GlobalState = GlobalState {
    wifi: FlashBackedWifiConfig::new(),
    // ...
};

// Generates WifiConfig<N> + WifiManager<N> impls on GlobalState
// by forwarding all methods to the `wifi` field.
impl_wifi_delegation!(GlobalState, wifi, { config::MAX_STA_NETWORKS });
```

Initialize in `main` before WiFi driver init:

```rust
STATE.wifi.init(async_flash, config::FLASH_RANGE).await;
```

Compile-time defaults are read from environment variables (`AP_SSID`, `AP_PASSWORD`,
`STA_SSID`, `STA_PASSWORD`, `HOSTNAME`, `AP_GATEWAY_IP`, `WIFI_COUNTRY_CODE`) and written to
flash on first boot only. Set them in `.cargo/config.toml` (keep that file out of git — see
`.cargo/config.toml.example`).

---

### 5. WiFi driver init

`WifiDriver::init` creates the AP and/or STA net stacks depending on which Cargo features
are enabled (`ap`, `sta`). It requires `&STATE` to satisfy `WifiConfig<N>` bounds (provided
by `impl_wifi_delegation!`).

```rust
let (stacks, wifi_controller) = WifiDriver::init(
    mk_static!(esp_radio::Controller<'_>, radio_controller),
    peripherals.WIFI,
    gw_ip,
    seed,
    &STATE,
    resources,
    &spawner,
).await;
```

Returns `WifiStacks { ap: Option<Stack>, sta: Option<Stack> }`. Spawn tasks against whichever
stacks are `Some`.

---

### 6. Network service tasks

```rust
// WiFi connection state machine (reconnects on drop, handles AP reconfig signals)
spawner.spawn(connection_task(wifi_controller, &STATE)).ok();

// AP stack services
spawner.spawn(dhcp_task(ap, gw_ip)).ok();   // assigns IPs to clients
spawner.spawn(dns_task(ap, gw_ip)).ok();    // captive portal DNS

// STA stack services
spawner.spawn(mdns::run_mdns(sta, hostname)).ok();  // advertise on LAN
```

---

### 7. Implementing `AppState` for your router

`web-app` (or any crate using `esp-web-utils`) requires your global state to implement
`web_app::AppState`. The two associated types that need concrete impls are:

**`type Store`** — session storage backend:

```rust
type Store = esp_session_mgmt::LruMemoryStore<web_app::UserData, 4>;

fn session_store(&self) -> &Self::Store { &self.sessions }
```

**`type Ota`** — OTA writer backend. Create a wrapper around `esp_hal_ota::Ota`:

```rust
pub struct FirmwareOtaWriter {
    ota: esp_hal_ota::Ota<esp_persistent_config::BlockingSharedFlash>,
}

impl web_app::OtaWriter for FirmwareOtaWriter {
    fn write_chunk(&mut self, data: &[u8]) -> Result<bool, web_app::OtaError> {
        self.ota.ota_write_chunk(data).map_err(|_| web_app::OtaError::WriteFailed)
    }
    fn finalize(mut self) -> Result<core::convert::Infallible, web_app::OtaError> {
        self.ota.ota_flush(false, true).map_err(|_| web_app::OtaError::FinalizationFailed)?;
        esp_hal::system::software_reset()
    }
}

fn begin_ota(&self) -> Result<FirmwareOtaWriter, web_app::OtaError> {
    let flash = self.blocking_flash();
    let mut ota = esp_hal_ota::Ota::new(flash)
        .map_err(|_| web_app::OtaError::InitializationFailed)?;
    ota.ota_begin(0, 0).map_err(|_| web_app::OtaError::EraseFailed)?;
    Ok(FirmwareOtaWriter { ota })
}
```

Also implement `web_app::UserStorage` to supply credentials from the flash-backed config:

```rust
impl web_app::UserStorage for GlobalState {
    async fn username(&self) -> Option<heapless::String<32>> {
        Some(CONFIG.lock().await.username.clone())
    }
    async fn password(&self) -> Option<heapless::String<32>> {
        Some(CONFIG.lock().await.password.clone())
    }
}
```

---

### 8. HTTP tasks

`picoserve` needs a separate task per concurrent connection. Use `pool_size` to set the cap.
Pass separate TCP rx/tx buffers and an HTTP parse buffer per task instance.

```rust
#[embassy_executor::task(pool_size = 4)]
async fn http_task(
    task_id: usize,
    stack: Stack<'static>,
    state: &'static GlobalState,
    picoserve_config: &'static picoserve::Config,
) -> ! {
    let mut tcp_rx  = [0u8; 1536];
    let mut tcp_tx  = [0u8; 1536];
    let mut http_buf = [0u8; 2048];
    let app = make_app(state);
    picoserve::Server::new(&app, picoserve_config, &mut http_buf)
        .listen_and_serve(task_id, stack, 80, &mut tcp_rx, &mut tcp_tx)
        .await
        .into_never()
}
```

Spawn 2 tasks on the AP stack and 1 on the STA stack (pool_size must cover the total):

```rust
for i in 0..2 { spawner.spawn(http_task(i,     ap,  &STATE, &web_app::CONFIG)).ok(); }
for i in 0..1 { spawner.spawn(http_task(2 + i, sta, &STATE, &web_app::CONFIG)).ok(); }
```

---

### 9. OTA rollback protection

Call this **after** all tasks have been successfully spawned. If the device resets before
reaching this line (e.g. a task fails to spawn), the bootloader will roll back to the
previous firmware.

```rust
if let Ok(mut ota) = esp_hal_ota::Ota::new(STATE.blocking_flash()) {
    let _ = ota.ota_mark_app_valid();
}
```

---

## Flash partition layout

The config partition must be present in `partitions.csv`. The default range used above:

| Offset | Size | Usage |
|--------|------|-------|
| `0x3E0000` | 32 KB | WiFi config + app config (shared KV store) |

Both `FlashBackedWifiConfig` and `define_config!` stores use `sequential-storage` for
wear-levelled KV within this region. Keys must not overlap — `esp-wifi` reserves keys 0–49.

---

## Feature flags

All crates follow the same pattern — enable the chip feature matching your target:

```toml
esp-wifi = { git = "https://github.com/nbc12/esp-embedded.git", features = ["esp32c6", "ap", "sta"] }
```

Supported chips: `esp32`, `esp32c2`, `esp32c3`, `esp32c6`, `esp32h2`, `esp32s2`, `esp32s3`

WiFi mode features on `esp-wifi`: `ap` (access point stack), `sta` (station stack).
At least one must be enabled.

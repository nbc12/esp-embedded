#![no_std]
#![feature(impl_trait_in_assoc_type)]
#![allow(async_fn_in_trait)]

use esp_system_api::{WifiConfig, WifiManager};
pub use esp_session_mgmt::{
    CookieConfig, Expiry, Record, SessionId, SessionStore,
    format_clear_cookie, format_set_cookie,
};
use heapless::String;
use embassy_time::Instant;
use sha2::{Sha256, Digest};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use picoserve::io::Read as _;

pub use picoserve;

pub static COOKIE_CONFIG: CookieConfig = CookieConfig {
    name: "session",
    http_only: true,
    same_site: "Strict",
    secure: false,
    path: "/",
};

// ─── OTA ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtaError {
    InitializationFailed,
    ReadError,
    EraseFailed,
    WriteFailed,
    InvalidImage,
    FinalizationFailed,
}

impl core::fmt::Display for OtaError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InitializationFailed => write!(f, "OTA initialization failed"),
            Self::ReadError => write!(f, "Failed to read firmware data from source"),
            Self::EraseFailed => write!(f, "Failed to erase flash or begin OTA process"),
            Self::WriteFailed => write!(f, "Failed to write firmware chunk to flash"),
            Self::InvalidImage => write!(f, "Firmware image is invalid. Check build target."),
            Self::FinalizationFailed => write!(f, "Failed to finalize update in bootloader"),
        }
    }
}

pub trait OtaWriter {
    fn write_chunk(&mut self, data: &[u8]) -> Result<bool, OtaError>;
    fn finalize(self) -> Result<core::convert::Infallible, OtaError>;
}

// ─── Authentication ──────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserData {
    pub username: String<32>,
}

pub trait UserStorage: Send + Sync {
    async fn username(&self) -> Option<String<32>>;
    async fn password(&self) -> Option<String<32>>;
}

pub struct NonceStore<const N: usize> {
    nonces: [(u64, Instant); N],
    next_idx: usize,
}

impl<const N: usize> NonceStore<N> {
    pub const fn new() -> Self {
        Self {
            nonces: [(0, Instant::from_ticks(0)); N],
            next_idx: 0,
        }
    }

    pub fn insert(&mut self, nonce: u64) {
        self.nonces[self.next_idx] = (nonce, Instant::now());
        self.next_idx = (self.next_idx + 1) % N;
    }

    pub fn verify(&mut self, nonce: u64) -> bool {
        let now = Instant::now();
        for slot in self.nonces.iter_mut() {
            if slot.0 == nonce {
                let valid = now.saturating_duration_since(slot.1)
                    < embassy_time::Duration::from_secs(30);
                slot.0 = 0;
                return valid;
            }
        }
        false
    }
}

// ─── App State Trait ─────────────────────────────────────────────────────────

pub trait AppState: WifiConfig<8> + WifiManager<8> + UserStorage {
    type Store: SessionStore<UserData>;
    type Ota: OtaWriter;

    fn session_store(&self) -> &Self::Store;
    fn nonce_store(&self) -> &Mutex<CriticalSectionRawMutex, NonceStore<8>>;
    async fn next_random_u64(&self) -> u64;
    fn begin_ota(&self) -> Result<Self::Ota, OtaError>;
}

// ─── Authorized Extractor ────────────────────────────────────────────────────

/// Reads the session cookie and validates it against the session store.
/// Implement as `FromRequestParts` so it can appear before a body extractor.
pub struct Authorized {
    pub session: Record<UserData>,
}

impl<S: AppState> picoserve::extract::FromRequestParts<'_, &'static S> for Authorized {
    type Rejection = picoserve::response::Redirect;

    async fn from_request_parts(
        state: &&'static S,
        request_parts: &picoserve::request::RequestParts<'_>,
    ) -> Result<Self, Self::Rejection> {
        let redirect = picoserve::response::Redirect::to("/login");

        let cookie_header = request_parts
            .headers()
            .get("Cookie")
            .ok_or(redirect)?;
        let cookie_str = core::str::from_utf8(cookie_header.as_raw())
            .map_err(|_| picoserve::response::Redirect::to("/login"))?;

        let id = cookie_str
            .split(';')
            .find_map(|s| {
                let mut parts = s.trim().splitn(2, '=');
                match (parts.next(), parts.next()) {
                    (Some("session"), Some(val)) => val.parse::<u64>().ok(),
                    _ => None,
                }
            })
            .map(SessionId)
            .ok_or(picoserve::response::Redirect::to("/login"))?;

        (*state)
            .session_store()
            .load(id, Instant::now())
            .await
            .map(|session| Authorized { session })
            .ok_or(picoserve::response::Redirect::to("/login"))
    }
}

// ─── OTA Extractor ───────────────────────────────────────────────────────────

/// Streams the request body through `AppState::begin_ota()`.
/// On success the device reboots — this value is only produced on failure.
pub struct OtaFailed(pub OtaError);

impl<'r, S: AppState> picoserve::extract::FromRequest<'r, &'static S> for OtaFailed {
    type Rejection = core::convert::Infallible;

    async fn from_request<R: picoserve::io::Read>(
        state: &&'static S,
        _parts: picoserve::request::RequestParts<'r>,
        body: picoserve::request::RequestBody<'r, R>,
    ) -> Result<Self, Self::Rejection> {
        match run_ota(*state, body).await {
            Ok(infallible) => match infallible {},
            Err(e) => Ok(OtaFailed(e)),
        }
    }
}

async fn run_ota<S: AppState, R: picoserve::io::Read>(
    state: &S,
    body: picoserve::request::RequestBody<'_, R>,
) -> Result<core::convert::Infallible, OtaError> {
    log::info!("[OTA] Starting upload...");
    let mut writer = state.begin_ota()?;
    let mut reader = body.reader();
    let mut buf = [0u8; 4096];
    let mut total_bytes = 0usize;

    loop {
        let n = match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => return Err(OtaError::ReadError),
        };
        total_bytes += n;
        match writer.write_chunk(&buf[..n])? {
            true => {
                log::info!("[OTA] Last chunk written. Total: {} bytes", total_bytes);
                break;
            }
            false => {
                if total_bytes % (64 * 1024) == 0 {
                    log::info!("[OTA] Written {} KB...", total_bytes / 1024);
                }
            }
        }
    }

    writer.finalize()
}

// ─── Login Handler ───────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub struct LoginData {
    pub user: String<32>,
    pub response: String<64>,
    pub nonce: u64,
}

pub async fn handle_login<S: AppState>(
    state: &S,
    data: LoginData,
) -> impl picoserve::response::IntoResponse {
    use picoserve::response::StatusCode;

    let mut cookie_buf = String::<128>::new();

    let (status, body): (StatusCode, &'static str) =
        if !state.nonce_store().lock().await.verify(data.nonce) {
            (StatusCode::UNAUTHORIZED, "Nonce expired or invalid")
        } else if let (Some(u), Some(p)) = (state.username().await, state.password().await) {
            if u == data.user {
                let mut hasher = Sha256::new();
                hasher.update(data.nonce.to_be_bytes());
                hasher.update(u.as_bytes());
                hasher.update(p.as_bytes());
                let expected = hasher.finalize();

                let mut expected_hex = String::<64>::new();
                for b in expected {
                    let _ = core::fmt::write(&mut expected_hex, format_args!("{:02x}", b));
                }

                if expected_hex == data.response {
                    let token = state.next_random_u64().await;
                    let record = Record {
                        id: SessionId(token),
                        data: UserData { username: u },
                        expiry: Expiry::OnInactivity(embassy_time::Duration::from_secs(3600)),
                        last_active: Instant::now(),
                    };
                    state.session_store().save(record.clone()).await;
                    let _ = format_set_cookie(&record, &COOKIE_CONFIG, &mut cookie_buf);
                    (StatusCode::OK, "Login successful")
                } else {
                    (StatusCode::UNAUTHORIZED, "Invalid credentials")
                }
            } else {
                (StatusCode::UNAUTHORIZED, "Invalid credentials")
            }
        } else {
            (StatusCode::UNAUTHORIZED, "Invalid credentials")
        };

    picoserve::response::Response::new(status, body).with_header("Set-Cookie", cookie_buf)
}

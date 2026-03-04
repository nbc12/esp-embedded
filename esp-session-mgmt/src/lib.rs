#![no_std]
#![allow(async_fn_in_trait)]

use embassy_time::{Instant, Duration};
use heapless::String;

/// The ID type for sessions, similar to tower-sessions.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SessionId(pub u64);

/// Expiry policy for a session.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Expiry {
    /// Expire after a period of inactivity.
    OnInactivity(Duration),
    /// Expire at a specific absolute time.
    AtDateTime(Instant),
}

/// A session record containing the ID, the data payload, and the expiry information.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record<D> {
    pub id: SessionId,
    pub data: D,
    pub expiry: Expiry,
    pub last_active: Instant,
}

impl<D> Record<D> {
    pub fn is_expired(&self, now: Instant) -> bool {
        match self.expiry {
            Expiry::OnInactivity(duration) => now > self.last_active + duration,
            Expiry::AtDateTime(instant) => now > instant,
        }
    }
}

/// The core trait for session storage backends.
pub trait SessionStore<D>: Send + Sync {
    /// Load a session by ID, but only if it hasn't expired.
    async fn load(&self, id: SessionId, now: Instant) -> Option<Record<D>>;
    /// Save or update a session record.
    async fn save(&self, record: Record<D>) -> bool;
    /// Delete a session record.
    async fn delete(&self, id: SessionId) -> bool;
    /// Check if an ID exists (used for collision mitigation).
    async fn exists(&self, id: SessionId) -> bool;
}

// ─── LRU Memory Store Implementation ────────────────────────────────────────

pub struct LruMemoryStore<D, const N: usize> {
    records: embassy_sync::mutex::Mutex<embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex, [Option<Record<D>>; N]>,
}

impl<D: Clone + Send, const N: usize> LruMemoryStore<D, N> {
    pub const fn new() -> Self {
        // This is a workaround for array initialization in no_std
        // In a real app, we'd use a more ergonomic way or unsafe if needed for speed.
        Self {
            records: embassy_sync::mutex::Mutex::new([const { None }; N]),
        }
    }
}

impl<D: Clone + Send + Sync, const N: usize> SessionStore<D> for LruMemoryStore<D, N> {
    async fn load(&self, id: SessionId, now: Instant) -> Option<Record<D>> {
        let mut records = self.records.lock().await;
        for slot in records.iter_mut() {
            if let Some(record) = slot {
                if record.id == id {
                    if record.is_expired(now) {
                        *slot = None;
                        return None;
                    }
                    record.last_active = now;
                    return Some(record.clone());
                }
            }
        }
        None
    }

    async fn save(&self, record: Record<D>) -> bool {
        let mut records = self.records.lock().await;
        let mut oldest_idx = 0;
        let mut earliest_activity = Instant::MAX;

        for (i, slot) in records.iter_mut().enumerate() {
            // Update existing
            if let Some(r) = slot {
                if r.id == record.id {
                    *slot = Some(record);
                    return true;
                }
                if r.last_active < earliest_activity {
                    earliest_activity = r.last_active;
                    oldest_idx = i;
                }
            } else {
                // Fill empty slot
                *slot = Some(record);
                return true;
            }
        }

        // LRU Eviction
        records[oldest_idx] = Some(record);
        true
    }

    async fn delete(&self, id: SessionId) -> bool {
        let mut records = self.records.lock().await;
        for slot in records.iter_mut() {
            if let Some(r) = slot {
                if r.id == id {
                    *slot = None;
                    return true;
                }
            }
        }
        false
    }

    async fn exists(&self, id: SessionId) -> bool {
        let records = self.records.lock().await;
        records.iter().flatten().any(|r| r.id == id)
    }
}

// ─── Cookie Utilities ────────────────────────────────────────────────────────

pub struct CookieConfig {
    pub name: &'static str,
    pub http_only: bool,
    pub same_site: &'static str,
    pub secure: bool,
    pub path: &'static str,
}

impl Default for CookieConfig {
    fn default() -> Self {
        Self {
            name: "session",
            http_only: true,
            same_site: "Strict",
            secure: true,
            path: "/",
        }
    }
}

pub fn format_set_cookie<D>(record: &Record<D>, config: &CookieConfig, buf: &mut String<128>) -> Result<(), core::fmt::Error> {
    let max_age = match record.expiry {
        Expiry::OnInactivity(d) => d.as_secs(),
        Expiry::AtDateTime(i) => i.saturating_duration_since(record.last_active).as_secs(),
    };

    core::fmt::write(buf, format_args!(
        "{}={}; Path={}; Max-Age={}; SameSite={}{}{}",
        config.name,
        record.id.0,
        config.path,
        max_age,
        config.same_site,
        if config.http_only { "; HttpOnly" } else { "" },
        if config.secure { "; Secure" } else { "" }
    ))
}

pub fn format_clear_cookie(config: &CookieConfig, buf: &mut String<128>) -> Result<(), core::fmt::Error> {
    core::fmt::write(buf, format_args!(
        "{}={}; Path={}; Max-Age=0; SameSite={}{}{}",
        config.name,
        "",
        config.path,
        config.same_site,
        if config.http_only { "; HttpOnly" } else { "" },
        if config.secure { "; Secure" } else { "" }
    ))
}

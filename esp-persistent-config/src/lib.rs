#![no_std]
#![feature(impl_trait_in_assoc_type)]

//! # Flash Persistence for ESP32
//!
//! This crate provides a thread-safe, flash-backed key-value store.
//! It supports sharing the underlying flash peripheral across multiple components
//! (like WiFi, Auth, and OTA) using an Embassy Mutex.

pub use embassy_sync::{blocking_mutex::raw::CriticalSectionRawMutex, mutex::Mutex};
pub use esp_storage::FlashStorage;
pub use sequential_storage::cache::NoCache;
pub use sequential_storage::map::{MapConfig, MapStorage, Value};

pub use embassy_sync;
pub use heapless::String;
pub use pastey;

use embedded_storage_async::nor_flash::{NorFlash, ReadNorFlash};
use embedded_storage::nor_flash::ErrorType;

// ─── Shared Flash Adapter ─────────────────────────────────────────────────────

/// An adapter that allows sharing a `FlashStorage` instance via an Embassy Mutex.
#[derive(Copy, Clone)]
pub struct AsyncSharedFlash {
    mutex: &'static Mutex<CriticalSectionRawMutex, FlashStorage<'static>>,
}

impl AsyncSharedFlash {
    pub const fn new(mutex: &'static Mutex<CriticalSectionRawMutex, FlashStorage<'static>>) -> Self {
        Self { mutex }
    }
}

impl ErrorType for AsyncSharedFlash {
    type Error = <FlashStorage<'static> as embedded_storage::nor_flash::ErrorType>::Error;
}

impl ReadNorFlash for AsyncSharedFlash {
    const READ_SIZE: usize = 1;

    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        let mut flash = self.mutex.lock().await;
        embedded_storage::nor_flash::ReadNorFlash::read(&mut *flash, offset, bytes)
    }

    fn capacity(&self) -> usize { 0 }
}

impl NorFlash for AsyncSharedFlash {
    const WRITE_SIZE: usize = <FlashStorage<'static> as embedded_storage::nor_flash::NorFlash>::WRITE_SIZE;
    const ERASE_SIZE: usize = <FlashStorage<'static> as embedded_storage::nor_flash::NorFlash>::ERASE_SIZE;

    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        let mut flash = self.mutex.lock().await;
        embedded_storage::nor_flash::NorFlash::write(&mut *flash, offset, bytes)
    }

    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        let mut flash = self.mutex.lock().await;
        embedded_storage::nor_flash::NorFlash::erase(&mut *flash, from, to)
    }
}

/// An adapter that allows sharing a `FlashStorage` instance via an Embassy Mutex using blocking traits.
pub struct BlockingSharedFlash {
    mutex: &'static Mutex<CriticalSectionRawMutex, FlashStorage<'static>>,
}

impl BlockingSharedFlash {
    pub const fn new(mutex: &'static Mutex<CriticalSectionRawMutex, FlashStorage<'static>>) -> Self {
        Self { mutex }
    }
}

impl embedded_storage::nor_flash::ErrorType for BlockingSharedFlash {
    type Error = <FlashStorage<'static> as embedded_storage::nor_flash::ErrorType>::Error;
}

impl embedded_storage::ReadStorage for BlockingSharedFlash {
    type Error = <FlashStorage<'static> as embedded_storage::nor_flash::ErrorType>::Error;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        if let Ok(mut flash) = self.mutex.try_lock() {
            embedded_storage::ReadStorage::read(&mut *flash, offset, bytes)
        } else {
            panic!("Flash mutex contention in blocking context")
        }
    }

    fn capacity(&self) -> usize { 0 }
}

impl embedded_storage::nor_flash::ReadNorFlash for BlockingSharedFlash {
    const READ_SIZE: usize = 1;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        embedded_storage::ReadStorage::read(self, offset, bytes)
    }

    fn capacity(&self) -> usize { 0 }
}

impl embedded_storage::nor_flash::NorFlash for BlockingSharedFlash {
    const WRITE_SIZE: usize = <FlashStorage<'static> as embedded_storage::nor_flash::NorFlash>::WRITE_SIZE;
    const ERASE_SIZE: usize = <FlashStorage<'static> as embedded_storage::nor_flash::NorFlash>::ERASE_SIZE;

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        if let Ok(mut flash) = self.mutex.try_lock() {
            embedded_storage::nor_flash::NorFlash::write(&mut *flash, offset, bytes)
        } else {
            panic!("Flash mutex contention in blocking context")
        }
    }

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        if let Ok(mut flash) = self.mutex.try_lock() {
            embedded_storage::nor_flash::NorFlash::erase(&mut *flash, from, to)
        } else {
            panic!("Flash mutex contention in blocking context")
        }
    }
}

impl embedded_storage::Storage for BlockingSharedFlash {
    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        embedded_storage::nor_flash::NorFlash::write(self, offset, bytes)
    }
}

// ─── Flash Engine ─────────────────────────────────────────────────────────────

/// The underlying type for the flash-backed key-value map.
pub type FlashMap<S> = MapStorage<u8, S, NoCache>;

/// A 4-byte aligned buffer for flash operations.
#[repr(align(4))]
pub struct AlignedBuf<const N: usize>(pub [u8; N]);

/// A thread-safe, flash-backed key-value store.
pub struct FlashStore<S: NorFlash> {
    pub storage: Mutex<CriticalSectionRawMutex, Option<FlashMap<S>>>,
}

impl<S: NorFlash> FlashStore<S> {
    pub const fn new() -> Self {
        Self {
            storage: Mutex::new(None),
        }
    }

    pub async fn init(
        &self,
        storage_driver: S,
        range: core::ops::Range<u32>,
    ) {
        let storage: FlashMap<S> = MapStorage::new(
            storage_driver,
            MapConfig::new(range),
            NoCache::new(),
        );
        *self.storage.lock().await = Some(storage);
    }

    pub async fn store<T>(&self, key: u8, value: &T) -> bool
    where
        T: for<'a> Value<'a>,
    {
        let mut flash = self.storage.lock().await;
        let Some(storage) = flash.as_mut() else {
            return false;
        };
        let mut buf = AlignedBuf([0u8; 128]);
        storage.store_item(&mut buf.0, &key, value).await.is_ok()
    }

    pub async fn fetch<T>(&self, key: u8) -> Option<T>
    where
        T: for<'a> Value<'a>,
    {
        let mut flash = self.storage.lock().await;
        let Some(storage) = flash.as_mut() else {
            return None;
        };
        let mut buf = AlignedBuf([0u8; 128]);
        storage
            .fetch_item::<T>(&mut buf.0, &key)
            .await
            .ok()
            .flatten()
    }
}

// ─── Configuration Macro ──────────────────────────────────────────────────────

#[macro_export]
macro_rules! define_config {
    (
        struct $struct_name:ident {
            $(
                $field_name:ident : $field_type:ty = $key:expr, $default:expr ;
            )*
        }
    ) => {
        pub struct $struct_name {
            $( pub $field_name: $field_type, )*
        }

        // Static is zero-initialized (valid for heapless::String, bool, numeric types).
        // The actual defaults are written to flash on first boot in init_config.
        pub static CONFIG: $crate::embassy_sync::mutex::Mutex<$crate::embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex, $struct_name> =
            $crate::embassy_sync::mutex::Mutex::new(unsafe { core::mem::zeroed() });

        static STORE: $crate::FlashStore<$crate::AsyncSharedFlash> = $crate::FlashStore::new();

        pub async fn init_config(flash_driver: $crate::AsyncSharedFlash, range: core::ops::Range<u32>) {
            STORE.init(flash_driver, range).await;

            $(
                match STORE.fetch::<$field_type>($key).await {
                    Some(val) => { CONFIG.lock().await.$field_name = val; }
                    None => {
                        // First boot: write the default to flash and apply it.
                        let default = $default;
                        STORE.store($key, &default).await;
                        CONFIG.lock().await.$field_name = default;
                    }
                }
            )*
        }

        $(
            $crate::pastey::paste! {
                pub async fn [<set_ $field_name>](val: $field_type) -> bool {
                    if STORE.store($key, &val).await {
                        CONFIG.lock().await.$field_name = val;
                        true
                    } else {
                        false
                    }
                }
            }
        )*
    };
}

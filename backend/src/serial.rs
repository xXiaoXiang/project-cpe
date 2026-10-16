/*
 * @Author: 1orz cloudorzi@gmail.com
 * @Date: 2025-12-07 07:33:11
 * @LastEditors: 1orz cloudorzi@gmail.com
 * @LastEditTime: 2025-12-13 12:46:14
 * @FilePath: /udx710-backend/backend/src/serial.rs
 * @Description: 
 * 
 * Copyright (c) 2025 by 1orz, All Rights Reserved. 
 */
//! DBus/AT Command Serialization Module
//!
//! This module provides a global lock to serialize all DBus and AT command operations
//! to prevent "org.ofono.Error.InProgress: Operation already in progress" errors.

use std::future::Future;
use tokio::sync::Mutex;
use std::time::{Duration, Instant};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Global mutex to serialize DBus/AT operations
static DBUS_LOCK: Mutex<()> = Mutex::const_new(());

// Radio initialization can keep ofono busy even when our own queue is empty.
// Back off background reads without delaying interactive writes behind a sleep.
static READ_BACKOFF: Mutex<(Option<Instant>, u32, Option<String>)> = Mutex::const_new((None, 0, None));
static MODEM_CHANGES: AtomicUsize = AtomicUsize::new(0);

/// Suspend background diagnostics during a SIM/radio change without holding the
/// command mutex across LPAC or taking the same mutex recursively.
pub struct ModemChange;

pub fn modem_change() -> ModemChange {
    MODEM_CHANGES.fetch_add(1, Ordering::SeqCst);
    ModemChange
}

impl Drop for ModemChange {
    fn drop(&mut self) {
        MODEM_CHANGES.fetch_sub(1, Ordering::SeqCst);
    }
}

pub async fn with_modem_read<T, F>(operation: &str, f: F) -> zbus::Result<T>
where
    F: Future<Output = zbus::Result<T>>,
{
    // Reject new background reads before they queue behind a long radio write.
    if MODEM_CHANGES.load(Ordering::SeqCst) > 0 {
        return Err(zbus::Error::Failure("ModemReadDeferred: SIM/radio change in progress".into()));
    }
    let _guard = DBUS_LOCK.lock().await;
    if MODEM_CHANGES.load(Ordering::SeqCst) > 0 {
        return Err(zbus::Error::Failure("ModemReadDeferred: SIM/radio change in progress".into()));
    }
    let mut backoff = READ_BACKOFF.lock().await;
    if backoff.0.is_some_and(|until| Instant::now() < until) {
        return Err(zbus::Error::Failure("ModemReadDeferred: waiting for Modem recovery".into()));
    }
    let result = f.await;
    if result.as_ref().err().is_some_and(|error| error.to_string().contains("org.ofono.Error.InProgress")) {
        backoff.1 = (backoff.1 + 1).min(4);
        backoff.2 = Some(operation.to_string());
        backoff.0 = Some(Instant::now() + Duration::from_secs((5 * (1 << (backoff.1 - 1))).min(30)));
    } else if result.is_ok() && backoff.2.as_deref() == Some(operation) {
        *backoff = (None, 0, None);
    }
    result
}

/// Execute a future while holding the global DBus lock
///
/// This ensures that only one DBus/AT operation can be in progress at a time,
/// preventing "Operation already in progress" errors from ofono.
///
/// # Example
/// ```rust
/// let result = with_serial(async {
///     send_at_command(&conn, "AT+CGSN").await
/// }).await;
/// ```
pub async fn with_serial<T, F>(f: F) -> T
where
    F: Future<Output = T>,
{
    let _guard = DBUS_LOCK.lock().await;
    f.await
}

/// LPAC 通过独立 AT 设备访问 eUICC，只需与自身串行，不占用 DBus 全局锁
static EUICC_LOCK: Mutex<()> = Mutex::const_new(());

pub async fn with_euicc_serial<T, F>(f: F) -> T
where
    F: Future<Output = T>,
{
    let _guard = EUICC_LOCK.lock().await;
    f.await
}


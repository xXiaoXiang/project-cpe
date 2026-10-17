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
use std::sync::Arc;

/// Global mutex to serialize DBus/AT operations
static DBUS_LOCK: Mutex<()> = Mutex::const_new(());

// Radio initialization can keep ofono busy even when our own queue is empty.
// Back off background reads without delaying interactive writes behind a sleep.
static READ_BACKOFF: Mutex<(Option<Instant>, u32, Option<String>)> = Mutex::const_new((None, 0, None));
static MODEM_CHANGES: AtomicUsize = AtomicUsize::new(0);
static READ_GENERATION: AtomicUsize = AtomicUsize::new(0);

pub fn invalidate_modem_snapshots() {
    READ_GENERATION.fetch_add(1, Ordering::SeqCst);
}

/// Each data source has its own lock: concurrent readers share a successful
/// fetch, while unrelated properties remain available during slow AT commands.
/// Errors are never cached. A generation change also rejects in-flight results.
pub struct ReadSnapshot<T> {
    version: AtomicUsize,
    value: Mutex<Option<(String, usize, usize, Instant, Arc<T>)>>,
}

impl<T> ReadSnapshot<T> {
    pub const fn new() -> Self {
        Self { version: AtomicUsize::new(0), value: Mutex::const_new(None) }
    }

    pub fn invalidate(&self) {
        self.version.fetch_add(1, Ordering::SeqCst);
    }

    pub async fn get<F>(&self, scope: &str, ttl: Duration, fetch: F) -> zbus::Result<Arc<T>>
    where
        F: Future<Output = zbus::Result<T>>,
    {
        self.get_inner(scope, ttl, true, fetch).await
    }

    /// Local CPU/network sampling is independent of radio state changes.
    pub async fn get_local<F>(&self, ttl: Duration, fetch: F) -> zbus::Result<Arc<T>>
    where
        F: Future<Output = zbus::Result<T>>,
    {
        self.get_inner("local", ttl, false, fetch).await
    }

    async fn get_inner<F>(&self, scope: &str, ttl: Duration, modem: bool, fetch: F) -> zbus::Result<Arc<T>>
    where
        F: Future<Output = zbus::Result<T>>,
    {
        let mut slot = self.value.lock().await;
        let generation = if modem { READ_GENERATION.load(Ordering::SeqCst) } else { 0 };
        let version = self.version.load(Ordering::SeqCst);
        let changing = modem && MODEM_CHANGES.load(Ordering::SeqCst) > 0;
        if !changing {
            if let Some((owner, saved_generation, saved_version, at, value)) = slot.as_ref() {
                if owner == scope && *saved_generation == generation && *saved_version == version && at.elapsed() < ttl {
                    return Ok(Arc::clone(value));
                }
            }
        }
        // Drop expired/superseded values even if this fetch fails.
        *slot = None;
        let value = Arc::new(fetch.await?);
        if (modem && generation != READ_GENERATION.load(Ordering::SeqCst)) || version != self.version.load(Ordering::SeqCst) {
            return Err(zbus::Error::Failure("ModemReadDeferred: state changed during read".into()));
        }
        if !changing {
            *slot = Some((scope.to_owned(), generation, version, Instant::now(), Arc::clone(&value)));
        }
        Ok(value)
    }
}

/// Submit a write once. Its reply can fail or lag behind the actual state;
/// repeatedly verify the requested state independently until the deadline.
pub async fn confirm_modem_state<T, W, R, F>(write: W, mut read: R, budget: Duration, poll: Duration) -> zbus::Result<T>
where
    W: Future<Output = zbus::Result<()>>,
    R: FnMut() -> F,
    F: Future<Output = zbus::Result<Option<T>>>,
{
    tokio::pin!(write);
    let deadline = tokio::time::Instant::now() + budget;
    let mut ticks = tokio::time::interval(poll);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut write_finished = false;
    let mut detail = "oFono has not confirmed the requested Online state".to_string();
    loop {
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(deadline) => {
                return Err(zbus::Error::Failure(format!(
                    "飞行模式切换等待超时（{} 秒），设备仍可能继续处理，请稍后检查状态。{}", budget.as_secs(), detail)));
            }
            result = &mut write, if !write_finished => {
                write_finished = true;
                if let Err(error) = result { detail = error.to_string(); }
            }
            _ = ticks.tick() => {
                // Bound each probe so a stuck GetProperties cannot consume the
                // entire confirmation window or prevent the next check.
                let probe_end = deadline.min(tokio::time::Instant::now() + Duration::from_secs(5));
                match tokio::time::timeout_at(probe_end, read()).await {
                    Ok(Ok(Some(state))) => return Ok(state),
                    Ok(Err(error)) => detail = error.to_string(),
                    Err(_) => detail = "oFono 状态读取暂未响应".to_string(),
                    _ => {}
                }
            }
        }
    }
}

/// Suspend background diagnostics during a SIM/radio change without holding the
/// command mutex across LPAC or taking the same mutex recursively.
pub struct ModemChange;

pub fn modem_change() -> ModemChange {
    MODEM_CHANGES.fetch_add(1, Ordering::SeqCst);
    invalidate_modem_snapshots();
    ModemChange
}

impl Drop for ModemChange {
    fn drop(&mut self) {
        invalidate_modem_snapshots();
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

/// Writes invalidate snapshots and pause diagnostics, including while queued.
/// A serialized read alone must never masquerade as a SIM/radio transition.
pub async fn with_modem_write<T, F>(f: F) -> T
where
    F: Future<Output = T>,
{
    let _change = modem_change();
    with_serial(f).await
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

#[cfg(test)]
mod snapshot_tests {
    use super::*;
    use tokio::sync::Notify;

    // One test owns the module's global command/change state. Its concurrent
    // tasks deliberately exercise the same snapshot and independent snapshots.
    #[tokio::test]
    async fn snapshots_coalesce_expire_and_reject_obsolete_results() {
        let cache = Arc::new(ReadSnapshot::<usize>::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let mut readers = Vec::new();
        for _ in 0..16 {
            let cache = Arc::clone(&cache);
            let calls = Arc::clone(&calls);
            readers.push(tokio::spawn(async move {
                cache.get("bus-a", Duration::from_secs(30), async {
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::task::yield_now().await;
                    Ok(7)
                }).await.unwrap()
            }));
        }
        for reader in readers { assert_eq!(*reader.await.unwrap(), 7); }
        assert_eq!(calls.load(Ordering::SeqCst), 1, "concurrent GETs must share one fetch");

        assert_eq!(*cache.get("bus-b", Duration::from_secs(30), async { Ok(8) }).await.unwrap(), 8,
            "connections must not share snapshots");
        assert_eq!(*cache.get("bus-b", Duration::ZERO, async { Ok(9) }).await.unwrap(), 9,
            "expired snapshots must refresh");
        cache.invalidate();
        assert!(cache.get("bus-b", Duration::from_secs(30), async {
            Err(zbus::Error::Failure("temporary failure".into()))
        }).await.is_err());
        assert_eq!(*cache.get("bus-b", Duration::from_secs(30), async { Ok(10) }).await.unwrap(), 10,
            "failed fetches must neither cache errors nor return expired data");

        // Simulate a slow cell read. A cheap independent property read must
        // complete while it is blocked, then invalidate the old cell result.
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        cache.invalidate();
        let pending = {
            let cache = Arc::clone(&cache);
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            tokio::spawn(async move {
                cache.get("bus-b", Duration::from_secs(30), async {
                    entered.notify_one();
                    release.notified().await;
                    Ok(11)
                }).await
            })
        };
        entered.notified().await;
        let independent = ReadSnapshot::new();
        assert_eq!(*tokio::time::timeout(Duration::from_secs(1),
            independent.get("bus-b", Duration::from_secs(1), async { Ok(99) }))
            .await.unwrap().unwrap(), 99);
        cache.invalidate();
        release.notify_one();
        assert!(pending.await.unwrap().unwrap_err().to_string().contains("ModemReadDeferred"));
        assert_eq!(*cache.get("bus-b", Duration::from_secs(30), async { Ok(12) }).await.unwrap(), 12);

        // Write guard must bypass previous cached values for confirmed readback
        // and must not preserve intermediate states when the operation ends.
        {
            let _change = modem_change();
            assert!(with_modem_read("qos", async { Ok(()) }).await.is_err());
            assert_eq!(*cache.get("bus-b", Duration::from_secs(30), async { Ok(13) }).await.unwrap(), 13);
            assert_eq!(*cache.get("bus-b", Duration::from_secs(30), async { Ok(14) }).await.unwrap(), 14);
        }
        assert_eq!(*cache.get("bus-b", Duration::from_secs(30), async { Ok(15) }).await.unwrap(), 15);
        let reader_entered = Arc::new(Notify::new());
        let release_reader = Arc::new(Notify::new());
        let reader = {
            let entered = Arc::clone(&reader_entered);
            let release = Arc::clone(&release_reader);
            tokio::spawn(with_serial(async move {
                entered.notify_one();
                release.notified().await;
            }))
        };
        reader_entered.notified().await;
        assert_eq!(*cache.get("bus-b", Duration::from_secs(30), async { Ok(999) }).await.unwrap(), 15,
            "a concurrent serialized GET must not invalidate cached data");
        let diagnostic = tokio::spawn(with_modem_read("location", async { Ok(42) }));
        tokio::task::yield_now().await;
        release_reader.notify_one();
        reader.await.unwrap();
        assert_eq!(diagnostic.await.unwrap().unwrap(), 42,
            "a diagnostic queues behind another read instead of reporting a SIM change");

        with_modem_write(async { Ok::<(), ()>(()) }).await.unwrap();
        assert_eq!(*cache.get("bus-b", Duration::from_secs(30), async { Ok(16) }).await.unwrap(), 16,
            "writes must invalidate old snapshots");

        let in_flight = cache.get("bus-b", Duration::ZERO, async {
            let _change = modem_change();
            Ok(17)
        }).await;
        assert!(in_flight.unwrap_err().to_string().contains("ModemReadDeferred"),
            "a SIM/radio change during fetch must reject the old generation");

        // A disconnected client cancels its fetch without leaving the source
        // locked or partially populated.
        let entered = Arc::new(Notify::new());
        let pending = {
            let cache = Arc::clone(&cache);
            let entered = Arc::clone(&entered);
            tokio::spawn(async move {
                cache.get("bus-b", Duration::ZERO, async {
                    entered.notify_one();
                    std::future::pending::<zbus::Result<usize>>().await
                }).await
            })
        };
        entered.notified().await;
        pending.abort();
        let _ = pending.await;
        assert_eq!(*cache.get("bus-b", Duration::from_secs(30), async { Ok(18) }).await.unwrap(), 18);

        let local = ReadSnapshot::new();
        assert_eq!(*local.get_local(Duration::from_secs(30), async { Ok(21) }).await.unwrap(), 21);
        let _change = modem_change();
        assert_eq!(*local.get_local(Duration::from_secs(30), async { Ok(22) }).await.unwrap(), 21,
            "radio writes must not discard unrelated CPU/network samples");
    }

    #[tokio::test]
    async fn confirmation_retries_after_failed_write_and_failed_probe() {
        let mut attempts = 0;
        let result = confirm_modem_state(async { Err(zbus::Error::Failure("write timed out".into())) }, || {
            attempts += 1;
            std::future::ready(match attempts {
                1 => Err(zbus::Error::Failure("temporary read failure".into())),
                2 => Ok(None),
                _ => Ok(Some(true)),
            })
        }, Duration::from_secs(1), Duration::from_millis(5)).await;
        assert!(result.unwrap());
        assert_eq!(attempts, 3);
    }

    #[tokio::test]
    async fn confirmation_does_not_wait_for_late_write_reply_or_accept_unconfirmed_success() {
        let mut attempts = 0;
        let result = confirm_modem_state(std::future::pending(), || {
            attempts += 1;
            std::future::ready(Ok((attempts == 2).then_some(false)))
        }, Duration::from_secs(1), Duration::from_millis(5)).await;
        assert!(!result.unwrap());
        assert_eq!(attempts, 2);

        let error = confirm_modem_state(async { Ok(()) }, || std::future::ready(Ok(None::<bool>)),
            Duration::from_millis(25), Duration::from_millis(5)).await.unwrap_err();
        assert!(error.to_string().contains("超时"), "a SetProperty reply alone must not confirm the switch");

        let started = Instant::now();
        let result = confirm_modem_state(async { Ok(()) }, || std::future::pending::<zbus::Result<Option<bool>>>(),
            Duration::from_millis(25), Duration::from_millis(5)).await;
        assert!(result.is_err());
        assert!(started.elapsed() < Duration::from_secs(1), "a hung status read must honor the overall deadline");
    }
}


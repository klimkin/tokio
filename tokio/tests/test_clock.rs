#![warn(rust_2018_idioms)]
#![cfg(any(
    feature = "full",
    all(
        target_os = "emscripten",
        feature = "rt",
        feature = "macros",
        feature = "test-util"
    )
))]

use tokio::time::{self, Duration, Instant};

#[tokio::test]
async fn resume_lets_time_move_forward_instead_of_resetting_it() {
    let start = Instant::now();
    time::pause();
    time::advance(Duration::from_secs(10)).await;
    let advanced_by_ten_secs = Instant::now();
    assert!(advanced_by_ten_secs - start > Duration::from_secs(10));
    assert!(advanced_by_ten_secs - start < Duration::from_secs(11));
    time::resume();
    assert!(advanced_by_ten_secs < Instant::now());
    assert!(Instant::now() - advanced_by_ten_secs < Duration::from_secs(1));
}

#[tokio::test]
async fn can_pause_after_resume() {
    let start = Instant::now();
    time::pause();
    time::advance(Duration::from_secs(10)).await;
    time::resume();
    time::pause();
    time::advance(Duration::from_secs(10)).await;
    assert!(Instant::now() - start > Duration::from_secs(20));
    assert!(Instant::now() - start < Duration::from_secs(21));
}

#[tokio::test(start_paused = true)]
async fn repeated_clock_changes_preserve_deadlines() {
    for _ in 0..32 {
        time::resume();
        time::pause();
        let start = Instant::now();
        time::advance(Duration::from_millis(1)).await;
        assert_eq!(start.elapsed(), Duration::from_millis(1));
        time::sleep_until(start + Duration::from_millis(2)).await;
        assert!(start.elapsed() >= Duration::from_millis(2));
        assert!(start.elapsed() < Duration::from_millis(3));
    }
}

#[tokio::test]
#[should_panic]
async fn freezing_time_while_frozen_panics() {
    time::pause();
    time::pause();
}

#[tokio::test]
#[should_panic]
async fn advancing_time_when_time_is_not_frozen_panics() {
    time::advance(Duration::from_secs(1)).await;
}

#[tokio::test]
#[should_panic]
async fn resuming_time_when_not_frozen_panics() {
    time::pause();
    time::resume();
    time::resume();
}

#[cfg(not(any(target_os = "wasi", target_os = "emscripten")))]
#[test]
fn advance_wakes_parked_driver() {
    use std::sync::mpsc;

    for enable_io in [false, true] {
        let (go_tx, go_rx) = mpsc::channel();
        let mut builder = tokio::runtime::Builder::new_current_thread();
        builder
            .enable_time()
            .start_paused(true)
            .on_thread_park(move || {
                let _ = go_tx.send(());
            });
        if enable_io {
            builder.enable_io();
        }
        let rt = builder.build().unwrap();

        rt.block_on(async {
            let start = Instant::now();
            let duration = Duration::from_secs(60);
            let (timer_complete_tx, timer_complete_rx) = mpsc::channel();
            let handle = tokio::runtime::Handle::current();
            // The blocking task inhibits auto-advance, so the driver parks in
            // real time. It stays alive until the timer fires, so its release
            // cannot be what wakes the driver.
            let worker = tokio::task::spawn_blocking(move || {
                go_rx
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(|_| "runtime did not start parking")?;
                std::thread::sleep(Duration::from_millis(20));
                handle.block_on(time::advance(duration));
                timer_complete_rx
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(|_| "parked timer did not complete before watchdog")
            });

            time::sleep(duration).await;
            let elapsed = start.elapsed();
            let _ = timer_complete_tx.send(());
            worker.await.unwrap().unwrap();
            assert_eq!(elapsed, duration);
        });
    }
}

#[cfg(not(any(target_os = "wasi", target_os = "emscripten")))]
#[test]
fn resume_wakes_parked_driver() {
    use std::sync::{mpsc, Arc, Mutex};

    for enable_io in [false, true] {
        let (go_tx, go_rx) = mpsc::channel();
        let (returned_tx, returned_rx) = tokio::sync::oneshot::channel();
        let returned_tx = Arc::new(Mutex::new(Some(returned_tx)));
        let mut builder = tokio::runtime::Builder::new_current_thread();
        builder
            .enable_time()
            .start_paused(true)
            .on_thread_park(move || {
                let _ = go_tx.send(());
            })
            .on_thread_unpark({
                let returned_tx = returned_tx.clone();
                move || {
                    if let Some(tx) = returned_tx.lock().unwrap().take() {
                        let _ = tx.send(Ok(()));
                    }
                }
            });
        if enable_io {
            builder.enable_io();
        }
        let rt = builder.build().unwrap();

        let handle = rt.handle().clone();
        let (done_tx, done_rx) = mpsc::channel::<()>();
        // No timer is pending, so the driver parks without a timeout and only
        // the resume notification can return it.
        let worker = std::thread::spawn(move || {
            if go_rx.recv_timeout(Duration::from_secs(5)).is_ok() {
                std::thread::sleep(Duration::from_millis(20));
                let _enter = handle.enter();
                time::resume();
            }
            if done_rx.recv_timeout(Duration::from_secs(5)).is_err() {
                if let Some(tx) = returned_tx.lock().unwrap().take() {
                    let _ = tx.send(Err("parked driver did not return before watchdog"));
                }
            }
        });

        let result = rt.block_on(returned_rx).unwrap();
        drop(done_tx);
        worker.join().unwrap();
        result.unwrap();
    }
}

#[cfg(not(any(target_os = "wasi", target_os = "emscripten")))]
mod parked_backend {
    use super::*;
    use std::sync::{mpsc, Arc, Mutex};

    enum WakeSource {
        Advance,
        BlockingRelease,
    }

    #[test]
    fn public_advance_returns_parked_backend() {
        assert_backend_returns(WakeSource::Advance, false);
    }

    #[test]
    fn public_advance_returns_parked_backend_with_io() {
        assert_backend_returns(WakeSource::Advance, true);
    }

    #[test]
    fn blocking_release_returns_parked_backend() {
        assert_backend_returns(WakeSource::BlockingRelease, false);
    }

    #[test]
    fn blocking_release_returns_parked_backend_with_io() {
        assert_backend_returns(WakeSource::BlockingRelease, true);
    }

    fn assert_backend_returns(source: WakeSource, enable_io: bool) {
        let (returned_tx, returned_rx) = tokio::sync::oneshot::channel();
        let returned = Arc::new(Mutex::new(Some(returned_tx)));
        let (go_tx, go_rx) = mpsc::channel();
        let go = Arc::new(Mutex::new(Some(go_tx)));
        let mut builder = tokio::runtime::Builder::new_current_thread();
        builder
            .enable_time()
            .start_paused(true)
            .on_thread_park({
                let go = go.clone();
                move || {
                    if let Some(go) = go.lock().unwrap().take() {
                        go.send(()).unwrap();
                    }
                }
            })
            .on_thread_unpark({
                let returned = returned.clone();
                move || {
                    if let Some(returned) = returned.lock().unwrap().take() {
                        let _ = returned.send(Ok(()));
                    }
                }
            });
        if enable_io {
            builder.enable_io();
        }
        let rt = builder.build().unwrap();
        let worker = match source {
            WakeSource::Advance => {
                let handle = rt.handle().clone();
                Some(std::thread::spawn(move || {
                    go_rx.recv().unwrap();
                    handle.block_on(time::advance(Duration::from_millis(1)));
                }))
            }
            WakeSource::BlockingRelease => {
                drop(rt.spawn_blocking(move || go_rx.recv().unwrap()));
                None
            }
        };

        let (finished_tx, finished_rx) = mpsc::channel();
        let watchdog = std::thread::spawn(move || {
            if finished_rx.recv_timeout(Duration::from_secs(5)).is_err() {
                if let Some(returned) = returned.lock().unwrap().take() {
                    let _ = returned.send(Err("parked backend did not return before watchdog"));
                    if let Some(go) = go.lock().unwrap().take() {
                        let _ = go.send(());
                    }
                }
            }
        });
        let result = rt.block_on(returned_rx).unwrap();
        let _ = finished_tx.send(());
        watchdog.join().unwrap();
        if let Some(worker) = worker {
            worker.join().unwrap();
        }
        result.unwrap();
    }

    #[test]
    fn overlapping_blocking_tasks_hold_clock_until_final_release() {
        use std::sync::atomic::{AtomicBool, Ordering};

        for enable_io in [false, true] {
            let (parked_tx, parked_rx) = mpsc::channel();
            let mut builder = tokio::runtime::Builder::new_current_thread();
            builder
                .enable_time()
                .start_paused(true)
                .on_thread_park(move || {
                    let _ = parked_tx.send(());
                });
            if enable_io {
                builder.enable_io();
            }
            let rt = builder.build().unwrap();
            let start = {
                let _enter = rt.enter();
                Instant::now()
            };
            let (started_tx, started_rx) = mpsc::channel();
            let (first_tx, first_rx) = mpsc::channel();
            let (last_tx, last_rx) = mpsc::channel();
            let first = rt.spawn_blocking({
                let started = started_tx.clone();
                move || {
                    started.send(()).unwrap();
                    let _ = first_rx.recv();
                }
            });
            let last_released = Arc::new(AtomicBool::new(false));
            let last = rt.spawn_blocking({
                let last_released = last_released.clone();
                move || {
                    started_tx.send(()).unwrap();
                    let _ = last_rx.recv();
                    last_released.store(true, Ordering::Release);
                }
            });
            started_rx.recv().unwrap();
            started_rx.recv().unwrap();

            let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
            let (timer_complete_tx, timer_complete_rx) = mpsc::channel();
            let handle = rt.handle().clone();
            let worker = std::thread::spawn(move || {
                let result = (|| {
                    parked_rx
                        .recv_timeout(Duration::from_secs(5))
                        .map_err(|_| "runtime did not park with two blockers")?;
                    first_tx.send(()).unwrap();
                    handle.block_on(first).unwrap();
                    parked_rx
                        .recv_timeout(Duration::from_secs(5))
                        .map_err(|_| "runtime did not park with final blocker")?;
                    let _enter = handle.enter();
                    if start.elapsed() != Duration::ZERO {
                        return Err("clock moved while final blocker was running");
                    }
                    std::thread::sleep(Duration::from_millis(20));
                    if start.elapsed() != Duration::ZERO {
                        return Err("clock moved while final blocker was running");
                    }
                    last_tx.send(()).unwrap();
                    handle.block_on(last).unwrap();
                    timer_complete_rx
                        .recv_timeout(Duration::from_secs(5))
                        .map_err(|_| "final blocking release did not wake timer")
                })();
                let _ = finished_tx.send(result);
            });

            let result = rt.block_on(async {
                let timer = time::sleep(Duration::from_secs(60));
                tokio::pin!(timer);
                tokio::pin!(finished_rx);
                tokio::select! {
                    result = &mut finished_rx => return result.unwrap(),
                    _ = &mut timer => {
                        if !last_released.load(Ordering::Acquire) {
                            return Err("timer expired while final blocker was running");
                        }
                    }
                }
                timer_complete_tx.send(()).unwrap();
                let result = finished_rx.await.unwrap();
                assert_eq!(start.elapsed(), Duration::from_secs(60));
                result
            });
            worker.join().unwrap();
            result.unwrap();
        }
    }
}

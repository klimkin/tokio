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
#![cfg(not(miri))] // Too slow on Miri.

use std::future::Future;
use std::task::Context;

use futures::task::noop_waker_ref;

use tokio::time::{self, Duration, Instant};
use tokio_test::{assert_elapsed, assert_pending, assert_ready, task};

#[tokio::test(start_paused = true)]
async fn immediate_sleep() {
    let now = Instant::now();

    let sleep = time::sleep_until(now);

    tokio::pin!(sleep);

    assert!(!sleep.is_elapsed());

    sleep.as_mut().await;
    assert_elapsed!(now, ms(0));
    assert!(sleep.is_elapsed());
}

#[tokio::test]
async fn is_elapsed() {
    time::pause();

    let sleep = time::sleep(Duration::from_millis(10));

    tokio::pin!(sleep);

    assert!(!sleep.is_elapsed());

    assert!(futures::poll!(sleep.as_mut()).is_pending());

    assert!(!sleep.is_elapsed());

    sleep.as_mut().await;

    assert!(sleep.is_elapsed());
}

#[tokio::test]
async fn delayed_sleep_level_0() {
    time::pause();

    for &i in &[1, 10, 60] {
        let now = Instant::now();
        let dur = ms(i);

        time::sleep_until(now + dur).await;

        assert_elapsed!(now, dur);
    }
}

#[tokio::test]
async fn sub_ms_delayed_sleep() {
    time::pause();

    for _ in 0..5 {
        let now = Instant::now();
        let deadline = now + ms(1) + Duration::new(0, 1);

        time::sleep_until(deadline).await;

        assert_elapsed!(now, ms(1));
    }
}

#[tokio::test(start_paused = true)]
async fn sleep_auto_advance_from_fractional_time() {
    for phase_us in [0, 100, 500, 900, 999] {
        for (deadline_us, expected_ms) in [
            (1_000, 1),
            (1_001, 2),
            (10_500, 11),
            (17_000, 17),
            (17_001, 18),
            (17_999, 18),
        ] {
            let start = Instant::now();
            time::advance(Duration::from_micros(phase_us)).await;

            let deadline = start + Duration::from_micros(deadline_us);
            let sleep = time::sleep_until(deadline);
            tokio::pin!(sleep);
            assert_pending!(futures::poll!(sleep.as_mut()));

            sleep.await;

            assert_eq!(
                start.elapsed(),
                Duration::from_millis(expected_ms),
                "phase {phase_us} us, deadline {deadline_us} us"
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn sub_ms_deadline_elapses_at_next_ms_tick() {
    let start = Instant::now();
    let mut sleep = task::spawn(time::sleep_until(start + Duration::from_micros(10_500)));
    assert_pending!(sleep.poll());

    time::advance(Duration::from_micros(10_500)).await;
    assert_pending!(sleep.poll());

    time::advance(Duration::from_micros(499)).await;
    assert_pending!(sleep.poll());

    time::advance(Duration::from_micros(1)).await;
    assert!(sleep.is_woken());
    assert_ready!(sleep.poll());
}

#[test]
fn fractional_sleep_across_wheel_levels() {
    for boundary_ms in [
        1,
        64,
        4096,
        262_144,
        16_777_216,
        1_073_741_824,
        68_719_476_736,
    ] {
        for phase_ns in [1, 999_999] {
            for (near_boundary, extra_ns) in [(false, 0), (false, 1), (true, 0), (true, 1)] {
                tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .start_paused(true)
                    .build()
                    .unwrap()
                    .block_on(async {
                        let start = Instant::now();
                        let phase = Duration::from_nanos(phase_ns);
                        let elapsed = if near_boundary {
                            ms(boundary_ms - 1) + phase
                        } else {
                            phase
                        };
                        time::advance(elapsed).await;

                        time::sleep_until(start + ms(boundary_ms) + Duration::from_nanos(extra_ns))
                            .await;
                        assert_eq!(
                            start.elapsed(),
                            ms(boundary_ms + extra_ns),
                            "boundary {boundary_ms} ms, phase {phase_ns} ns, near {near_boundary}, offset {extra_ns} ns"
                        );
                    });
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn fractional_sleep_across_top_level_rotations() {
    let start = Instant::now();
    time::advance(Duration::from_nanos(999_999)).await;

    let deadline_ms = (2_u64 << 36) + 5;
    let sleep = time::sleep_until(start + ms(deadline_ms) + Duration::from_nanos(1));
    tokio::pin!(sleep);
    assert_pending!(futures::poll!(sleep.as_mut()));

    time::sleep_until(start + ms(1)).await;
    assert_eq!(start.elapsed(), ms(1));

    sleep.await;
    assert_eq!(start.elapsed(), ms(deadline_ms + 1));
}

#[tokio::test(start_paused = true)]
async fn reset_and_reuse_sleep_at_fractional_time() {
    for (old_ms, new_ms) in [(17, 1), (1, 17), (64, 65), (65, 64)] {
        time::sleep(ms(1)).await;
        let start = Instant::now();
        time::advance(Duration::from_nanos(999_999)).await;

        let sleep = time::sleep_until(start + ms(old_ms));
        tokio::pin!(sleep);
        assert_pending!(futures::poll!(sleep.as_mut()));
        sleep.as_mut().reset(start + ms(new_ms));
        sleep.as_mut().await;
        assert_eq!(start.elapsed(), ms(new_ms));

        time::advance(Duration::from_nanos(999_999)).await;
        sleep.as_mut().reset(start + ms(new_ms + 1));
        sleep.as_mut().await;
        assert_eq!(start.elapsed(), ms(new_ms + 1));

        time::advance(Duration::from_nanos(900_000)).await;
        let before = Instant::now();
        sleep.as_mut().reset(start + ms(new_ms));
        assert_ready!(futures::poll!(sleep.as_mut()));
        assert_eq!(Instant::now(), before);
    }
}

#[tokio::test(start_paused = true)]
async fn cancel_earliest_sleep_at_fractional_time() {
    let start = Instant::now();
    time::advance(ms(63) + Duration::from_nanos(999_999)).await;

    let mut early = Box::pin(time::sleep_until(start + ms(64)));
    let mut survivor = Box::pin(time::sleep_until(start + ms(65)));
    assert_pending!(futures::poll!(early.as_mut()));
    assert_pending!(futures::poll!(survivor.as_mut()));

    drop(early);
    survivor.await;
    assert_eq!(start.elapsed(), ms(65));
}

#[tokio::test(start_paused = true)]
async fn fractional_deadlines_wake_more_than_one_batch() {
    let start = Instant::now();
    time::advance(Duration::from_nanos(999_999)).await;

    let mut sleeps = Vec::new();
    for index in 0..96 {
        let offset_ns = [1_000_001, 1_500_000, 1_999_999][index % 3];
        let mut sleep = task::spawn(time::sleep_until(start + Duration::from_nanos(offset_ns)));
        assert_pending!(sleep.poll());
        sleeps.push(sleep);
    }
    sleeps.truncate(65);
    let mut next_tick = task::spawn(Box::pin(time::sleep_until(
        start + Duration::from_nanos(2_000_001),
    )));
    assert_pending!(next_tick.poll());

    time::advance(ms(1)).await;
    for sleep in &mut sleeps {
        assert!(!sleep.is_woken());
        assert_pending!(sleep.poll());
    }

    time::advance(Duration::from_nanos(1)).await;
    for sleep in &mut sleeps {
        assert!(sleep.is_woken());
        assert_ready!(sleep.poll());
    }
    assert_pending!(next_tick.poll());
    assert_eq!(start.elapsed(), ms(2));

    next_tick.into_inner().await;
    assert_eq!(start.elapsed(), ms(3));
}

#[tokio::test(start_paused = true)]
async fn runnable_yields_preserve_fractional_time() {
    let start = Instant::now();
    time::advance(Duration::from_nanos(999_999)).await;

    let sleep = time::sleep_until(start + ms(1));
    tokio::pin!(sleep);
    assert_pending!(futures::poll!(sleep.as_mut()));

    for _ in 0..128 {
        tokio::task::yield_now().await;
        assert_eq!(start.elapsed(), Duration::from_nanos(999_999));
    }

    sleep.await;
    assert_eq!(start.elapsed(), ms(1));
}

#[tokio::test(start_paused = true)]
async fn resume_and_pause_preserve_timer_epoch() {
    let start = Instant::now();
    let sleep = time::sleep(Duration::from_secs(10));
    tokio::pin!(sleep);
    assert_pending!(futures::poll!(sleep.as_mut()));

    time::resume();
    std::thread::sleep(Duration::from_micros(70));
    time::pause();

    let deadline = Instant::now() + Duration::from_nanos(500_001);
    let expected_ms = deadline
        .duration_since(start)
        .as_nanos()
        .div_ceil(1_000_000) as u64;
    sleep.as_mut().reset(deadline);
    sleep.await;
    assert_eq!(start.elapsed(), ms(expected_ms));
}

#[tokio::test]
async fn delayed_sleep_wrapping_level_0() {
    time::pause();

    time::sleep(ms(5)).await;

    let now = Instant::now();
    time::sleep_until(now + ms(60)).await;

    assert_elapsed!(now, ms(60));
}

#[tokio::test]
async fn reset_future_sleep_before_fire() {
    time::pause();

    let now = Instant::now();

    let mut sleep = task::spawn(Box::pin(time::sleep_until(now + ms(100))));
    assert_pending!(sleep.poll());

    let mut sleep = sleep.into_inner();

    sleep.as_mut().reset(Instant::now() + ms(200));
    sleep.await;

    assert_elapsed!(now, ms(200));
}

#[tokio::test]
async fn reset_past_sleep_before_turn() {
    time::pause();

    let now = Instant::now();

    let mut sleep = task::spawn(Box::pin(time::sleep_until(now + ms(100))));
    assert_pending!(sleep.poll());

    let mut sleep = sleep.into_inner();

    sleep.as_mut().reset(now + ms(80));
    sleep.await;

    assert_elapsed!(now, ms(80));
}

#[tokio::test]
async fn reset_past_sleep_before_fire() {
    time::pause();

    let now = Instant::now();

    let mut sleep = task::spawn(Box::pin(time::sleep_until(now + ms(100))));
    assert_pending!(sleep.poll());

    let mut sleep = sleep.into_inner();

    time::sleep(ms(10)).await;

    sleep.as_mut().reset(now + ms(80));
    sleep.await;

    assert_elapsed!(now, ms(80));
}

#[tokio::test]
async fn reset_future_sleep_after_fire() {
    time::pause();

    let now = Instant::now();
    let mut sleep = Box::pin(time::sleep_until(now + ms(100)));

    sleep.as_mut().await;
    assert_elapsed!(now, ms(100));

    sleep.as_mut().reset(now + ms(110));
    sleep.await;
    assert_elapsed!(now, ms(110));
}

#[tokio::test]
async fn reset_sleep_to_past() {
    time::pause();

    let now = Instant::now();

    let mut sleep = task::spawn(Box::pin(time::sleep_until(now + ms(100))));
    assert_pending!(sleep.poll());

    time::sleep(ms(50)).await;

    assert!(!sleep.is_woken());

    sleep.as_mut().reset(now + ms(40));

    // TODO: is this required?
    //assert!(sleep.is_woken());

    assert_ready!(sleep.poll());
}

#[cfg(not(target_os = "wasi"))] // Wasi doesn't support panic recovery
#[test]
#[should_panic]
fn creating_sleep_outside_of_context() {
    let now = Instant::now();

    // This creates a delay outside of the context of a mock timer. This tests
    // that it will panic.
    let _fut = time::sleep_until(now + ms(500));
}

#[tokio::test]
async fn greater_than_max() {
    const YR_5: u64 = 5 * 365 * 24 * 60 * 60 * 1000;

    time::pause();
    time::sleep_until(Instant::now() + ms(YR_5)).await;
}

#[tokio::test]
async fn short_sleeps() {
    for _ in 0..1000 {
        tokio::time::sleep(std::time::Duration::from_millis(0)).await;
    }
}

#[tokio::test]
async fn multi_long_sleeps() {
    tokio::time::pause();

    for _ in 0..5u32 {
        tokio::time::sleep(Duration::from_secs(
            // about a year
            365 * 24 * 3600,
        ))
        .await;
    }

    let deadline = tokio::time::Instant::now()
        + Duration::from_secs(
            // about 10 years
            10 * 365 * 24 * 3600,
        );

    tokio::time::sleep_until(deadline).await;

    assert!(tokio::time::Instant::now() >= deadline);
}

#[tokio::test]
async fn long_sleeps() {
    tokio::time::pause();

    let deadline = tokio::time::Instant::now()
        + Duration::from_secs(
            // about 10 years
            10 * 365 * 24 * 3600,
        );

    tokio::time::sleep_until(deadline).await;

    assert!(tokio::time::Instant::now() >= deadline);
    assert!(tokio::time::Instant::now() <= deadline + Duration::from_millis(1));
}

#[tokio::test]
async fn reset_after_firing() {
    let timer = tokio::time::sleep(std::time::Duration::from_millis(1));
    tokio::pin!(timer);

    let deadline = timer.deadline();

    timer.as_mut().await;
    assert_ready!(timer
        .as_mut()
        .poll(&mut Context::from_waker(noop_waker_ref())));
    timer
        .as_mut()
        .reset(tokio::time::Instant::now() + std::time::Duration::from_secs(600));

    assert_ne!(deadline, timer.deadline());

    assert_pending!(timer
        .as_mut()
        .poll(&mut Context::from_waker(noop_waker_ref())));
    assert_pending!(timer
        .as_mut()
        .poll(&mut Context::from_waker(noop_waker_ref())));
}

#[tokio::test]
async fn exactly_max() {
    time::pause();
    time::sleep(Duration::MAX).await;
}

#[tokio::test]
async fn issue_5183() {
    time::pause();

    let big = std::time::Duration::from_secs(u64::MAX / 10);
    // This is a workaround since awaiting sleep(big) will never finish.
    #[rustfmt::skip]
    tokio::select! {
	biased;
        _ = tokio::time::sleep(big) => {}
        _ = tokio::time::sleep(std::time::Duration::from_nanos(1)) => {}
    }
}

#[tokio::test]
async fn no_out_of_bounds_close_to_max() {
    time::pause();
    time::sleep(Duration::MAX - Duration::from_millis(1)).await;
}

#[tokio::test(start_paused = true)]
async fn long_wait_sleep_does_not_break_other_timers() {
    tokio::spawn(time::sleep(ms(10 << 36)));

    time::advance(ms((1 << 30) - 1)).await;

    let start = Instant::now();
    time::sleep(ms(10)).await;
    assert_elapsed!(start, ms(10));
}

#[tokio::test(start_paused = true)]
async fn long_wait_sleep_does_not_corrupt_wheel() {
    use futures::poll;

    tokio::spawn(time::sleep(ms(10 << 36)));

    time::advance(ms((1 << 30) - 1)).await;

    let mut first = Box::pin(time::sleep(ms(10)));
    assert_pending!(poll!(first.as_mut()));
    let mut second = Box::pin(time::sleep(ms(20)));
    assert_pending!(poll!(second.as_mut()));

    time::advance(ms(30)).await;

    drop(first);

    time::advance(ms(1 << 31)).await;

    drop(second);
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

#[tokio::test]
async fn drop_after_reschedule_at_new_scheduled_time() {
    use futures::poll;

    tokio::time::pause();

    let start = tokio::time::Instant::now();

    let mut a = Box::pin(tokio::time::sleep(Duration::from_millis(5)));
    let mut b = Box::pin(tokio::time::sleep(Duration::from_millis(5)));
    let mut c = Box::pin(tokio::time::sleep(Duration::from_millis(10)));

    let _ = poll!(&mut a);
    let _ = poll!(&mut b);
    let _ = poll!(&mut c);

    b.as_mut().reset(start + Duration::from_millis(10));
    a.await;

    drop(b);
}

#[tokio::test]
async fn drop_from_wake() {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::task::Context;

    let panicked = Arc::new(AtomicBool::new(false));
    let list: Arc<Mutex<Vec<Pin<Box<tokio::time::Sleep>>>>> = Arc::new(Mutex::new(Vec::new()));

    let arc_wake = Arc::new(DropWaker(panicked.clone(), list.clone()));
    let arc_wake = futures::task::waker(arc_wake);

    tokio::time::pause();

    {
        let mut lock = list.lock().unwrap();

        for _ in 0..100 {
            let mut timer = Box::pin(tokio::time::sleep(Duration::from_millis(10)));

            let _ = timer.as_mut().poll(&mut Context::from_waker(&arc_wake));

            lock.push(timer);
        }
    }

    tokio::time::sleep(Duration::from_millis(11)).await;

    assert!(
        !panicked.load(Ordering::SeqCst),
        "panicked when dropping timers"
    );

    #[derive(Clone)]
    struct DropWaker(
        Arc<AtomicBool>,
        Arc<Mutex<Vec<Pin<Box<tokio::time::Sleep>>>>>,
    );

    impl futures::task::ArcWake for DropWaker {
        fn wake_by_ref(arc_self: &Arc<Self>) {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                *arc_self.1.lock().expect("panic in lock") = Vec::new()
            }));

            if result.is_err() {
                arc_self.0.store(true, Ordering::SeqCst);
            }
        }
    }
}

#[tokio::test]
async fn drop_from_waker_drop() {
    use std::pin::Pin;
    use std::sync::Arc;
    use std::task::{Context, Wake, Waker};
    use tokio::time::Sleep;

    struct TimerOwner {
        secondary: Option<Pin<Box<Sleep>>>,
    }

    impl Wake for TimerOwner {
        fn wake(self: Arc<Self>) {}
    }

    impl Drop for TimerOwner {
        fn drop(&mut self) {
            drop(self.secondary.take());
        }
    }

    let mut secondary = Box::pin(tokio::time::sleep(Duration::from_secs(3600)));
    assert_pending!(secondary
        .as_mut()
        .poll(&mut Context::from_waker(noop_waker_ref())));

    let waker = Waker::from(Arc::new(TimerOwner {
        secondary: Some(secondary),
    }));
    let mut primary = Box::pin(tokio::time::sleep(Duration::from_secs(3600)));
    assert_pending!(primary.as_mut().poll(&mut Context::from_waker(&waker)));

    drop(waker);
    drop(primary);
}

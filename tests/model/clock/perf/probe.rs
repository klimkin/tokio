use std::ffi::{c_int, c_long};
use std::future::{poll_fn, Future};
use std::io::Write;
use std::pin::pin;
use std::sync::{mpsc, Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};
use tokio::runtime::Builder;
use tokio::sync::oneshot;

#[repr(C)]
struct Timespec {
    seconds: c_long,
    nanos: c_long,
}

unsafe extern "C" {
    fn clock_gettime(clock: c_int, time: *mut Timespec) -> c_int;
}

fn cpu_ns(clock: c_int) -> u128 {
    let mut time = Timespec {
        seconds: 0,
        nanos: 0,
    };
    assert_eq!(unsafe { clock_gettime(clock, &mut time) }, 0);
    time.seconds as u128 * 1_000_000_000 + time.nanos as u128
}

#[derive(Default)]
struct Samples {
    wake: Vec<u128>,
    timer: Vec<u128>,
    errors: usize,
    pending_timers: usize,
    active_blockers: usize,
    wall_ns: u128,
    process_cpu_ns: u128,
    runtime_thread_cpu_ns: u128,
}

struct Measurement {
    wall: Instant,
    process_cpu: u128,
    thread_cpu: u128,
}

impl Measurement {
    fn start() -> Self {
        Self {
            wall: Instant::now(),
            process_cpu: cpu_ns(2),
            thread_cpu: cpu_ns(3),
        }
    }

    fn finish(self, samples: &mut Samples) {
        samples.runtime_thread_cpu_ns = cpu_ns(3) - self.thread_cpu;
        samples.process_cpu_ns = cpu_ns(2) - self.process_cpu;
        samples.wall_ns = self.wall.elapsed().as_nanos();
    }
}

fn quantile(values: &[u128], percentile: usize) -> u128 {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    sorted[((sorted.len() - 1) * percentile).div_ceil(100)]
}

async fn pending(sleep: std::pin::Pin<&mut tokio::time::Sleep>) -> bool {
    let mut sleep = sleep;
    poll_fn(|cx| Poll::Ready(sleep.as_mut().poll(cx).is_pending())).await
}

async fn wake_samples(count: usize) -> Vec<u128> {
    let (requests, receiver) = mpsc::channel::<oneshot::Sender<Instant>>();
    let producer = std::thread::spawn(move || {
        while let Ok(sender) = receiver.recv() {
            std::thread::sleep(Duration::from_millis(2));
            sender.send(Instant::now()).unwrap();
        }
    });
    let mut samples = Vec::with_capacity(count);
    for _ in 0..count {
        let (sender, receiver) = oneshot::channel();
        requests.send(sender).unwrap();
        samples.push(receiver.await.unwrap().elapsed().as_nanos());
    }
    drop(requests);
    producer.join().unwrap();
    samples
}

#[cfg(feature = "io")]
async fn tcp_samples(count: usize) -> Vec<u128> {
    use std::io::Read;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let epoch = Instant::now();
    let producer = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket.set_nodelay(true).unwrap();
        for _ in 0..count {
            let mut ack = [0];
            socket.read_exact(&mut ack).unwrap();
            std::thread::sleep(Duration::from_millis(2));
            let timestamp = epoch.elapsed().as_nanos() as u64;
            socket.write_all(&timestamp.to_ne_bytes()).unwrap();
        }
    });
    let socket = tokio::net::TcpStream::connect(address).await.unwrap();
    socket.set_nodelay(true).unwrap();
    let mut samples = Vec::with_capacity(count);
    for _ in 0..count {
        loop {
            socket.writable().await.unwrap();
            match socket.try_write(&[1]) {
                Ok(1) => break,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                result => panic!("TCP acknowledgement failed: {result:?}"),
            }
        }
        let mut buffer = [0; 8];
        let mut filled = 0;
        while filled < buffer.len() {
            socket.readable().await.unwrap();
            match socket.try_read(&mut buffer[filled..]) {
                Ok(0) => panic!("unexpected TCP EOF"),
                Ok(bytes) => filled += bytes,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("TCP read failed: {error}"),
            }
        }
        samples.push(epoch.elapsed().as_nanos() - u64::from_ne_bytes(buffer) as u128);
    }
    producer.join().unwrap();
    samples
}

async fn paused_case(case: &str, count: usize, hold_ms: u64) -> Samples {
    let (stop, release) = mpsc::channel();
    let (started, started_receiver) = mpsc::sync_channel(0);
    let blocker = tokio::task::spawn_blocking(move || {
        started.send(()).unwrap();
        release.recv().unwrap();
    });
    started_receiver.recv().unwrap();
    let origin = tokio::time::Instant::now();
    tokio::time::advance(Duration::from_nanos(999_999)).await;
    let frozen = tokio::time::Instant::now();
    let mut timer = match case {
        "paused-cpu" => Some(Box::pin(tokio::time::sleep_until(
            origin + Duration::from_millis(1),
        ))),
        "advance" => Some(Box::pin(tokio::time::sleep_until(
            origin + Duration::from_secs(3600),
        ))),
        _ => None,
    };
    if let Some(timer) = &mut timer {
        assert!(pending(timer.as_mut()).await, "timer must be pending");
    }
    let mut samples = Samples {
        pending_timers: usize::from(timer.is_some()),
        active_blockers: 1,
        ..Samples::default()
    };
    println!("{{\"event\":\"ready\",\"case\":\"{case}\",\"pending_timers\":{},\"active_blockers\":1,\"fractional_remaining_ns\":{}}}", samples.pending_timers, usize::from(case == "paused-cpu"));
    std::io::stdout().flush().unwrap();
    let measurement = Measurement::start();
    if case == "paused-cpu" {
        let (sender, receiver) = oneshot::channel();
        let producer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(hold_ms));
            sender.send(()).unwrap();
        });
        receiver.await.unwrap();
        producer.join().unwrap();
    } else if case == "wake" {
        samples.wake = wake_samples(count).await;
    } else if case == "tcp" {
        #[cfg(feature = "io")]
        {
            samples.wake = tcp_samples(count).await;
        }
        #[cfg(not(feature = "io"))]
        panic!("TCP case requires io feature");
    } else {
        let handle = tokio::runtime::Handle::current();
        let timestamp = Arc::new(Mutex::new(None));
        let producer_timestamp = timestamp.clone();
        let (finish, finished) = mpsc::channel();
        let producer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            let _entered = handle.enter();
            *producer_timestamp.lock().unwrap() = Some(Instant::now());
            let mut advance = pin!(tokio::time::advance(Duration::from_secs(3600)));
            let mut context = Context::from_waker(Waker::noop());
            assert!(advance.as_mut().poll(&mut context).is_pending());
            finished.recv().unwrap();
        });
        timer.as_mut().unwrap().as_mut().await;
        samples
            .wake
            .push(timestamp.lock().unwrap().unwrap().elapsed().as_nanos());
        finish.send(()).unwrap();
        producer.join().unwrap();
    }
    if matches!(case, "paused-cpu" | "wake" | "tcp") {
        samples.errors += usize::from(tokio::time::Instant::now() != frozen);
        if let Some(timer) = &mut timer {
            samples.errors += usize::from(!pending(timer.as_mut()).await);
        }
    }
    measurement.finish(&mut samples);
    drop(timer);
    stop.send(()).unwrap();
    blocker.await.unwrap();
    samples
}

fn resume_case() -> Samples {
    let (parked, parks) = mpsc::channel();
    let (unparked, unparks) = mpsc::channel();
    let (handle_sender, handles) = mpsc::sync_channel(0);
    let (stop, stopped) = oneshot::channel();
    let runtime_thread = std::thread::spawn(move || {
        let mut builder = Builder::new_current_thread();
        builder
            .enable_time()
            .start_paused(true)
            .on_thread_park(move || {
                let _ = parked.send(());
            })
            .on_thread_unpark(move || {
                let _ = unparked.send(Instant::now());
            });
        #[cfg(feature = "io")]
        builder.enable_io();
        let runtime = builder.build().unwrap();
        handle_sender.send(runtime.handle().clone()).unwrap();
        let measurement = Measurement::start();
        runtime.block_on(async {
            stopped.await.unwrap();
        });
        let mut samples = Samples::default();
        measurement.finish(&mut samples);
        samples
    });
    let handle = handles.recv().unwrap();
    parks.recv().unwrap();
    std::thread::sleep(Duration::from_millis(20));
    while unparks.try_recv().is_ok() {}
    println!(
        "{{\"event\":\"ready\",\"case\":\"resume\",\"pending_timers\":0,\"active_blockers\":0}}"
    );
    std::io::stdout().flush().unwrap();
    let timestamp = Instant::now();
    {
        let _entered = handle.enter();
        tokio::time::resume();
    }
    let returned = unparks.recv().unwrap();
    let latency = returned.duration_since(timestamp).as_nanos();
    stop.send(()).unwrap();
    let mut samples = runtime_thread.join().unwrap();
    samples.wake.push(latency);
    samples
}

async fn running_case(count: usize) -> Samples {
    let measurement = Measurement::start();
    let timers = tokio::spawn(async move {
        let origin = tokio::time::Instant::now();
        let mut late = Vec::with_capacity(count);
        let mut errors = 0;
        for index in 1..=count {
            let requested =
                origin + Duration::from_millis(index as u64 * 2) + Duration::from_nanos(1);
            let sleep = tokio::time::sleep_until(requested);
            let deadline = sleep.deadline();
            sleep.await;
            let observed = tokio::time::Instant::now();
            errors += usize::from(observed < deadline);
            late.push(observed.saturating_duration_since(deadline).as_nanos());
        }
        (late, errors)
    });
    let wake = wake_samples(count).await;
    let (timer, errors) = timers.await.unwrap();
    let mut samples = Samples {
        wake,
        timer,
        errors,
        ..Samples::default()
    };
    measurement.finish(&mut samples);
    samples
}

fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    let case = args.get(1).expect("case required");
    assert!(["paused-cpu", "wake", "tcp", "advance", "resume", "timers"].contains(&case.as_str()));
    let count = args.get(2).map_or(200, |value| value.parse().unwrap());
    let hold_ms = args.get(3).map_or(800, |value| value.parse().unwrap());
    let mut builder = Builder::new_current_thread();
    builder.enable_time().start_paused(case != "timers");
    #[cfg(feature = "io")]
    builder.enable_io();
    let runtime = builder.build().unwrap();
    let samples = if case == "resume" {
        resume_case()
    } else {
        runtime.block_on(async {
            if case == "timers" {
                running_case(count).await
            } else {
                paused_case(case, count, hold_ms).await
            }
        })
    };
    println!("{{\"event\":\"result\",\"case\":\"{case}\",\"wall_ns\":{},\"process_cpu_ns\":{},\"runtime_thread_cpu_ns\":{},\"wake_count\":{},\"timer_count\":{},\"errors\":{},\"pending_timers\":{},\"active_blockers\":{},\"wake_p50_ns\":{},\"wake_p99_ns\":{},\"wake_max_ns\":{},\"timer_p50_ns\":{},\"timer_p99_ns\":{},\"timer_max_ns\":{}}}",
        samples.wall_ns, samples.process_cpu_ns, samples.runtime_thread_cpu_ns,
        samples.wake.len(), samples.timer.len(), samples.errors, samples.pending_timers, samples.active_blockers,
        quantile(&samples.wake, 50), quantile(&samples.wake, 99), quantile(&samples.wake, 100),
        quantile(&samples.timer, 50), quantile(&samples.timer, 99), quantile(&samples.timer, 100));
    std::process::exit(i32::from(samples.errors != 0));
}

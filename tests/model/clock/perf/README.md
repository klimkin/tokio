# Compare Linux clock responsiveness

`run.py` builds one standalone release probe against each supplied Tokio checkout.
Generated manifests, binaries, logs, and results stay outside the repository.
One generated `Cargo.lock` pins dependencies for every build.
The script never edits either Tokio checkout or its dependency declarations.

The comparison covers I/O disabled and enabled, standard and `parking_lot` locks,
and idle and loaded scheduling. Each configuration runs baseline then fix five
times. The default probe process uses one allowed CPU. The loaded configuration
starts at most four separate CPU workers, capped at half the allowed CPUs.
One worker shares the probe CPU. Each remaining worker uses another allowed CPU.
This creates scheduling contention without saturating the host.

## Run the comparison

Supply absolute paths to the baseline checkout, final checkout, output directory,
and dedicated Cargo target directory. Finish other heavy tests before measuring.

```sh
python3 tests/model/clock/perf/run.py prepare \
  --base /path/to/base --fix /path/to/fix \
  --output /path/to/results --target /path/to/target-perf
python3 tests/model/clock/perf/run.py run \
  --base /path/to/base --fix /path/to/fix \
  --output /path/to/results --target /path/to/target-perf \
  --name final-comparison
```

Preparation needs cached Cargo dependencies because builds use `--offline`.
Changing a checkout HEAD or probe source requires preparation again.
`--cpu` selects an allowed CPU explicitly. `--smoke --repetitions 1 --samples 10
--hold-ms 50 --watchdog 1` runs an idle validation pass.
Smoke results do not establish performance differences.

## Read the measurements

Each workload produces observed work counts and error counts.
The orchestrator checks counts before accepting a run.
CPU measurements use Linux process and thread CPU clocks in nanoseconds.
The process metric includes the runtime, blocking task, and in-process producer.
The runtime thread metric excludes producers and blocking threads.
Both exclude the separate load workers and Python orchestrator.
CPU intervals begin after the paused workload acknowledges its setup.
The resume probe instead measures CPU from before initial parking through
shutdown, including the parking settlement interval. Use its wake latency for
responsiveness comparisons; its CPU interval differs from the paused workloads.

| Case | Measured path and completed work |
| --- | --- |
| `paused-cpu` | A running blocking task inhibits auto-advance. The clock sits 1 ns before a timer wheel boundary. A polled `Sleep` remains pending during the default 800 ms interval. A producer then sends one completion signal. CPU samples cover this interval. |
| `wake` | Producer timestamp immediately before oneshot send to completion of the receiver await. Default 200 sends, separated by 2 ms producer sleeps. No timers can rescue missed wakes. |
| `tcp` | Producer timestamp immediately before a localhost TCP write to receipt of its complete timestamp. Default 200 messages with one acknowledgement per message and 2 ms producer sleeps. I/O builds only. No timer rescue. |
| `timers` | Default 200 running-clock timers alongside 200 producer wakes. Timer lateness uses completion time minus each actual `Sleep::deadline()`. Deadlines are absolute 2 ms intervals plus 1 ns. The result includes Tokio timer rounding and OS scheduling. Early completions count as errors. |
| `advance` | A blocking task inhibits auto-advance while a `Sleep` waits one hour. An entered external thread polls public `advance(1 hour)` once. Latency ends when the registered `Sleep` completes. The producer stays alive and sends no task wake. |
| `resume` | A timer-free runtime waits for a shutdown oneshot. An entered external thread calls public `resume()`. Latency ends at `on_thread_unpark`, after the actual backend wait returns. The shutdown sender runs only after this observation. |

`advance` and `resume` expose missing notifications in the pre-PR baseline.
An independent subprocess watchdog terminates a hung process after 3 seconds.
No watchdog uses a Tokio timer or sends a wake to the measured runtime.
A baseline watchdog expiry after setup counts as a known correctness failure.
It produces no successful latency sample and cannot support a speedup claim.
Unexpected failures make the orchestration command fail after saving results.
The resume callback measures backend return, not a task completing.
Spurious backend returns can occur, so successful runs alone do not prove
notification correctness for every possible interleaving.

`raw.jsonl` retains every child output, failure, and watchdog expiry.
`summary.json` gives each metric's median, minimum, and maximum across valid runs.
Latency fields are per-run p50 and p99 values, not pooled percentiles.
`metadata.json` records CPU affinity, host load, process evidence, compiler,
checkout revisions, source hash, and shared lockfile hash.
Five runs per side establish repetition, not hard latency guarantees.
Treat differences smaller than run variation as unmeasured.

Run tracers separately from reported measurements. For example, invoke
`strace -c -o trace.txt /path/to/results/builds/fix-io1-std/probe paused-cpu`.
`epoll_wait` or `futex` counts reveal whether backend waits repeat during
inhibition. `perf stat` can provide CPU and context-switch evidence when host
permissions allow it. Report tracing limits and observed scheduler contention.

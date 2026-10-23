# Paused clock model checks

This suite separates the clock-change wake protocol from automatic clock
advancement. `ClockProtocol` and its response observer retain the wake protocol
with the automatic advance that predates `Clock::try_auto_advance`.
`ClockLifecycle` checks the clock-lock operation `Clock::try_auto_advance` and
preserves the original behavior at master `13614790` as explicit diagnostics.
`WaitPolicy` enumerates timeout arithmetic. No production code depends on these
finite TLA+ models.

The default command runs correctness checks, deliberate defects, reachability
witnesses, and runner self-checks. A successful deliberate-defect run means TLC
found the exact expected property violation. Witnesses establish reachability;
they are not correctness theorems.

```sh
python3 tests/model/clock/check.py \
  --tlc-jar /absolute/path/to/tla2tools.jar \
  --results /absolute/path/outside/repository/clock-checks
```

Python 3.10 or newer and Java are required. The results directory must be new and
outside the checkout. Each case has a 90-second subprocess timeout and one TLC
worker. Named selections use the same manifest and diagnostic checks.

```sh
python3 tests/model/clock/check.py \
  --tlc-jar /absolute/path/to/tla2tools.jar \
  --results /absolute/path/outside/repository/clock-advance \
  --case response.advance.during \
  --case mutant.advance.response
```

The jar comes from the official
[TLA+ tools v1.7.4 release](https://github.com/tlaplus/tlaplus/releases/tag/v1.7.4).
That release contains `TLC2 Version 2.19 of 08 August 2024 (rev: 5a47802)`.
The runner requires SHA-256
`936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`.
It neither downloads nor commits the jar. The original release's SHA-1 is
`bee4a54f3ee3d4afc347c3240ec2d9e93b075104`.

Each case records the model sources, generated configuration, command, complete
TLC output, state counts, and result. `summary.json` records every selected
result. Java temporary files and TLC state files also stay in the results tree.
The seed and fingerprint polynomial are fixed. Full source snapshots avoid
relying on a later checkout's contents when reviewing an old counterexample.

## Model ownership and source map

`ClockProtocol.tla` owns the historical wake-protocol transitions.
`ClockResponse.tla` instantiates those transitions and updates observer state.
It adds only independent ticks and the actual-wait-age guard on timeout return. `WaitPolicy.tla` owns the shared
floor-and-cap operator and its finite enumeration. `check.py` owns the case table
and diagnostic validation. `ClockLifecycle.tla` separately owns clock-lock
eligibility, sampled-time validation, finite backend timeout expiry, repeated
controls, and two-timer delivery.

The protocol has one timer, one driver, a clock mutex abstraction, a time flag,
a backend notification token, and one or two independent callers. Each caller
has its own mutation, flag publication, backend swap, mutex handoff, and signal
phases. Two releases can overlap at any of these phases. The serial-release case
starts the final release only after the first request was observed and its
notification was consumed, with the driver again asleep while inhibited.

Paths below are relative to the repository root.

| Source | Model action or field |
| --- | --- |
| `tokio/src/time/clock.rs`, public `resume` and `advance` | `Mutate` changes the mutex-protected clock before separate wake work. |
| `tokio/src/time/clock.rs`, `inhibit_auto_advance` and `allow_auto_advance` | `Register` increments inhibition; release `Mutate` decrements it. |
| `tokio/src/time/clock.rs`, `state` and `now` | `Snapshot` samples clock mode and count. `Read` samples timer time earlier. `Process` samples timer time again. |
| `tokio/src/time/clock.rs`, internal `Clock::advance` | Historical protocol `Auto` rechecks running mode but does not recheck inhibition. |
| `tokio/src/time/clock.rs`, `Clock::try_auto_advance` | Lifecycle `Auto` atomically checks paused mode, zero inhibition, and equality with sampled time before adding the capped duration. |
| `tokio/src/runtime/blocking/schedule.rs`, `BlockingSchedule::release` | Release mutation followed by the same full wake chain as public clock changes. |
| `tokio/src/runtime/time/handle.rs`, `Handle::unpark` | `Flag` stores the time flag. |
| `tokio/src/runtime/driver.rs`, `Handle::unpark` | Time-flag publication precedes `Swap`; neither implies the other has completed. |
| `tokio/src/runtime/park.rs`, `park_timeout` | `Fast` consumes an existing token before zero-timeout handling. `Arm`, `BeginWait`, and `Return` preserve enrollment and unconditional timed-return cleanup. |
| `tokio/src/runtime/park.rs`, `unpark` | `Swap` retains `Notified`. `Handoff` waits for the park mutex and releases it. Separate `Signal` notifies the current waiter. |
| `tokio/src/runtime/time/mod.rs`, `park_internal` | `Read` computes remaining duration. Zero bypasses `Snapshot` and automatic advance. `Process` follows the backend return. |
| `tokio/src/runtime/time/mod.rs`, `park_thread_timeout` and `did_wake` | `Snapshot`, zero backend poll, `FlagCheck`, and `Auto` remain separate transitions. |
| `tokio/src/runtime/time/mod.rs`, `park_timeout_duration` | Shared `Policy` applies the positive inhibited floor before the scheduler cap. |
| `tokio/src/runtime/time/mod.rs`, timer-present `park_internal` backend wait | Lifecycle `WaitReturn` consumes a retained notification. Separate `TimeoutReturn` returns from a finite positive wait sampled as running or inhibited. |

Clock mutex operations are atomic. SeqCst flag and backend atomics are separate
transitions. A backend token cannot wake an enrolled condvar waiter until
`Signal`, timeout, or an enabled spurious return. The park mutex remains owned
between `Arm` and `BeginWait`; the latter atomically releases it and enrolls the
waiter. `Handoff` and `Signal` stay separate, so timeout, cleanup, and re-entry
can interleave between them. `Return` abstracts condvar mutex reacquisition and
backend cleanup together; notification callers hold that mutex only for the
handoff and do not change shared state while holding it.

`Before` starts at a reachable inhibited `Fast` phase and requires the mutation
before enrollment. `During` starts in a reachable inhibited wait. `All` starts
at `Read` with caller mutation unrestricted, so all initial ordering windows
remain reachable. Snapshot diagnostics start paused without inhibition and
allow the competing operation between flag consumption and `Auto`.

## Checked properties

Untimed protocol correctness cases check finite state types, timer delivery only when
due, retained notification, automatic-advance snapshot eligibility, and zero
wait bypass. Weak fairness applies separately to each caller and to the single
driver. At most one driver phase is enabled at a time, so driver fairness cannot
choose a different driver phase indefinitely. Wake-dependent progress cases
disable timeout and spurious rescue. The all-window case enables both for
safety coverage; no fairness assumption promises eventual timeout rescue.

A request is acknowledged by an operation-specific fresh observation.

| Operation | Acknowledgment |
| --- | --- |
| Deadline-crossing advance | `Process` reads the mutation epoch and delivers the due timer. |
| Partial advance | `Read` reads the mutation epoch and recomputes a still-positive remaining duration. |
| Resume | `Snapshot` observes the running clock state at the mutation epoch. |
| Release | `Snapshot` observes the inhibition count at the mutation epoch. |

Setting a flag, writing a token, or reading timer time after release does not
acknowledge release. A snapshot can acknowledge before its caller finishes
waking. Caller completion remains separate, and experiment settlement requires
both caller completion and acknowledgment. The timer-progress case also waits
for timer completion. A zero scheduler cap has request progress but no claimed
timer-completion property.

`WaitPolicy` enumerates 48 combinations of states `{Running, Paused, Inhibited}`,
remaining durations `{0, 1, 2, 8}`, and caps `{none, 0, 1, 2}`. One timer tick is
two logical duration units. The integer `3` encodes no cap; it is not a duration
cap of three. Checks cover zero preservation, precision outside inhibited mode,
cap dominance, and the positive inhibited floor without a cap. The remove-floor
mutant fails at positive sub-tick duration `1`, inhibited state, and no cap.
A cap below one timer tick intentionally defeats the floor.

## Conditional response bound

Observer ticks are real-time quanta. Clock durations are separate logical
units. No equality between a quantum and a millisecond is assumed. The observer
does not advance running virtual time, so resume cases keep the timer future
and claim fresh reconsideration, not its eventual wall-clock expiration.

`Tick` is always enabled, including after response, a bound violation, or a
service-assumption violation. Every tick toggles parity, so it remains a
nonstuttering action after counters saturate. No response deadline guards a
protocol action or disables time. Wait age starts at actual `BeginWait`, or at
zero for the initialized `During` wait. Request age starts at mutation. These
ages are independent.

The observer monitors a separate service debt for each caller and for the
driver. A caller is runnable only when a real next source action is enabled.
A backend handoff blocked on the park mutex incurs no caller service debt;
the driver holding that mutex must run within its own allowance. A sleeping
driver incurs no CPU debt until signaled or its timeout expires. A missing-wake
mutant finishes its caller, so it creates no invented wake-service obligation.

For each currently runnable action, at most its configured number of ticks can
pass before it executes. Exceeding any allowance permanently clears `held`.
Ticks and source transitions continue afterward. `ConditionalResponse` checks
`held => (~pending \/ age <= Bound)`. Weak fairness alone supplies no finite
response bound.

The implemented action graph gives this conservative finite-burst bound,
where `N` is the caller count, `C` the caller allowance, `B` the backend
allowance, and `S` the driver allowance.

```text
Bound = N * (2*C + 3*B + 9*S) + 1
```

Each caller has at most two caller actions, `Mutate` and `Flag`, and three
backend actions, `Swap`, `Handoff`, and `Signal`. Counting `Mutate` also covers
an overlapping caller that has yet to mutate after the oldest request starts.
Each notification wave needs at most one old driver iteration followed by a
fresh observation. The longest branch counted is
`Read -> Snapshot -> Fast -> Arm -> BeginWait -> NotifyReturn -> Process -> Read -> Snapshot`,
which has nine actions. The paused zero-poll branch replaces enrollment and
return with `FlagCheck` and `Auto` and is shorter. A due advance ends at
`Process`; partial advance ends at `Read`. Mutex handoff delay is already
covered by the driver actions that release it. There are at most `N` request
waves. Adding one tick gives an explicit boundary allowance. This bound is
conservative; it is not a measured OS latency or a minimal bound.

Checked allowances are `C = B = S = 1`, giving bounds `15` and `29`. The old wait
timeout is `Bound + 4`, giving `19` and `33` quanta. The timeout represents the
initial positive wait in this finite scenario. Later waits reuse that timeout;
no claim about precise duration-to-wall-clock conversion follows. All timed
cases use positive uncapped waits; timeout-policy arithmetic is checked
separately. The missing-wake counterexamples violate response while `held` is
true and the old wait remains unexpired.

A new mutation while pending updates the newest epoch without resetting age.
`OldestAgePreserved` checks this as an action-temporal property in every positive
response case. The overlap witness shows positive age on both sides of a second
mutation. If an earlier request has already been acknowledged, a later mutation
legitimately starts a new age interval.

## Deliberate defects and witnesses

The manifest includes these exact negative contracts.

| Case | Required failure |
| --- | --- |
| `mutant.advance.untimed` | Sole temporal property `RequestProgress`, with a stuttering counterexample and no timeout rescue. |
| `mutant.advance.response` | `ConditionalResponse` after a due advance omits waking. |
| `mutant.advance.partial` | `ConditionalResponse` after a partial advance omits waking. |
| `mutant.resume.response` | `ConditionalResponse` after resume omits waking. |
| `mutant.release.final` | `ConditionalResponse` after the final release omits waking, after the first release was observed and its token consumed. |
| `mutant.token.retention` | `BackendRetention` when an unpark before enrollment drops its token. |
| `mutant.token.response` | `ConditionalResponse` for the same dropped token. |
| `mutant.policy.floor` | `PositiveInhibitedFloor` for a positive sub-tick inhibited wait without a cap. |

Witness cases reach mutations before and during park, both inequalities between
time flag and backend token, final nested release, overlap with retained age,
a positive-age response while assumptions hold, time continuing after a sticky
assumption violation, and missing-wake failure before the old timeout.

The snapshot diagnostics deliberately violate `WitnessRejected` and
`WitnessInhibitedAdvance`. The first reaches a stale paused snapshot followed
by resume, then internal advance rejection. The second registers inhibition
after a paused snapshot, then reaches internal advance while inhibition is
positive. These are synchronization-model witnesses, not claims that runtime
ownership permits the same caller placement in a real program. Successful
progress configurations retain inhibition during advance or resume, or use only
valid releases with no new registrations. They do not silently add an
inhibition recheck to internal `Auto`. Neither diagnostic justifies a production
change without a reachable Rust reproduction.

## Repeated clock operations and two deadlines

`ClockLifecycle.tla` separately models `Clock::try_auto_advance` with rule
`Conditional`. Its `Auto` action holds the clock lock while checking paused
mode, zero inhibition, and equality with the time sampled before duration
calculation. It adds the capped duration only when all checks hold. Otherwise,
it keeps time unchanged and continues to timer processing and the next read.
The clock lock never spans backend polling.

Rule `Original` retains the relative automatic advance, its running-clock error,
and its missing inhibition recheck at master `13614790`. Original-behavior
diagnostics expose those source behaviors. `NoPaused`, `NoInhibit`, and
`StaleRelative` remove one of the new checks for deliberate-defect cases. These models abstract Rust
synchronization; their counterexamples do not establish runtime reachability.

The controller executes one finite valid program while the driver processes
deadlines `{2, 4}`. The five programs are fixed finite inputs.

| Workload | Controller operations |
| --- | --- |
| `Advances` | Advance, advance. |
| `ResumePause` | Resume, pause. |
| `Inhibit` | Register, release. |
| `Mixed` | Register, advance, release, advance. |
| `Nested` | Register, register, advance, release, release, advance. |

Flag publication and retained backend-token publication are separate actions. Registration and pause publish neither.
The backend abstracts notification delivery as a retained token. `WaitReturn`
consumes that token. Separate `TimeoutReturn` models expiry of a finite positive
backend wait sampled as running or inhibited. Both return to timer processing
without advancing clock time. `FiniteTimeout` enables expiry in every lifecycle
case except its deliberate missing-timeout defect. The original protocol model
separately checks the condvar mutex, enrollment, and signal path.

`Read` samples clock time and remaining duration. `Snapshot` samples mode and
inhibition and applies the shared wait policy. `Poll`, `FlagCheck`, and `Auto`
remain separate actions. The sampled target is `sampledNow + effective`, so a
scheduler cap can place it before the earliest timer deadline. Automatic
advance must respect that smaller target as well.

Clock arithmetic never clamps to either deadline. `MaxTime` is a checked type
bound, `2 * LastDeadline + ExternalDelta`, where `ExternalDelta` counts the
program's one-unit manual advances. A read at or beyond the final deadline
bypasses automatic advance. Every automatic commit starts from a read below
`LastDeadline` and adds at most `LastDeadline`. External increments after that
read total at most `ExternalDelta`. No transition uses `MaxTime` as an enabling
guard.

All lifecycle correctness cases check finite types, monotonic clock time,
timer delivery only at or after each timer's own deadline, and eventual
controller and timer completion. Conditional-rule cases also check that every
automatic commit is paused and uninhibited, never exceeds its sampled capped
target, and never panics. Ten conditional cases combine the five controller
programs with no cap and a one-unit cap. Two original-rule cases retain baseline
coverage without asserting the new automatic-commit properties.

Completion assumes weak fairness for the finite controller and driver, and
finite expiry of positive backend waits. `TimeoutReturn` belongs to `Driver`, so
a wait without a token cannot persist forever under that fairness assumption.
All enabled returns leave the wait phase. Programs end paused without
inhibition. In particular, bare resume followed by pause can leave a wait
sampled as running after its only wake token was consumed. Finite timeout
expiry lets the driver reread the paused clock and resume automatic advance.
No running-clock rate, finite response bound, or OS service guarantee follows.
This untimed expiry assumption does not weaken the historical response model,
which separately measures whether notification beats the old timeout.

The lifecycle diagnostics require exact invariant violations for resume racing
an old paused snapshot, inhibition registration before automatic commit, and a
manual advance invalidating a sampled relative duration. The capped diagnostic
also requires `AutoWithinTarget` to fail with a cap of one unit. Separate
witnesses reach repeated manual advances, registration after the paused
snapshot, stale sampled time, skipped automatic commit, and delivery of both
deadlines. Three automatic-commit defects each require the exact named failure
of `AutoEligible` or `AutoWithinTarget`. A fourth lifecycle defect disables
finite timeout expiry for bare resume followed by pause and requires an exact
`Completion` temporal counterexample. It configures only that temporal property
so the runner can identify the failure unambiguously.

This model covers the timer-present driver branch. The no-timer scheduler-limit
branch, running-clock elapsed time, arbitrary timer sets, and unbounded external
control sequences remain outside this finite lifecycle check. Backend response
bounds remain limited to the original protocol observer's one- or two-request
bursts. The two models do not compose into a source-level proof.

## Runner trust boundary

Positive results require exit `0`, TLC's complete-success diagnostic, a final
state summary with an empty queue, and the completed-run footer. Invariant
negatives require exit `12`, the exact named invariant, and a real state trace.
Temporal negatives require exit `13`, TLC's temporal diagnostic, a loop trace,
and exactly one configured temporal property because this TLC version omits
its name. Extra errors, wrong exit kinds, missing summaries, and incomplete
traces fail the case.

Runner self-checks send a malformed module to the actual jar and require parse
exit `150`. They also execute and kill a deliberately timed-out child. Validator
checks reject unrelated properties, wrong exit codes, truncated logs, Java
errors, clean success under a negative expectation, and malformed expected
property names. A timeout, parser failure, or any other nonzero status cannot
count as an expected counterexample.

## Scope

These results exhaust the selected finite state spaces. They do not prove
arbitrary timer sets, unbounded blocker populations, unbounded external clock
changes, platform I/O backends, shutdown, integer overflow, or real scheduler
service guarantees. Clock time saturates at the one deadline in the original
protocol and response models. The lifecycle model uses two deadlines and unsaturated time. There is no
running-clock rate model and no CPU-utilization conclusion from the timeout
floor. The protocol abstracts synchronization behavior; Loom tests of actual
Rust wiring and runtime integration tests remain complementary evidence.

## Recorded run

The complete default run passed on 2026-10-21 using the pinned jar and Java
25.0.4.1 on Linux. It contains 36 correctness cases, 12 deliberate defects,
20 witnesses including the snapshot and lifecycle diagnostics, and the runner
self-check. All 68 model subprocesses completed. The command above regenerates
the results; exact counts are also in each `result.json`.

| Representative case | Distinct states |
| --- | ---: |
| `protocol.advance.during` | 8 |
| `protocol.all-windows` | 143 |
| `protocol.release.overlap` | 1,274 |
| `response.advance.during` | 1,009 |
| `response.advance.all-windows` | 2,050 |
| `response.resume.all-windows` | 4,051 |
| `response.release.all-windows` | 2,923 |
| `response.advance.partial` | 2,005 |
| `response.release.serial` | 5,191 |
| `response.release.overlap` | 41,039 |
| `lifecycle.advances.original` | 1,477 |
| `lifecycle.nested.original` | 6,054 |
| `lifecycle.advances.cap3` | 1,899 |
| `lifecycle.resumepause.cap3` | 577 |
| `lifecycle.resumepause.cap1` | 1,129 |
| `lifecycle.nested.cap1` | 11,835 |
| `mutant.lifecycle.timeout` | 475 |
| `policy.precision-floor-cap` | 48 |

The due-advance and resume missing-wake traces fail with age `16`, wait age `16`,
and `held = TRUE`, before old timeout `19`. The final-release trace fails with
age `30`, wait age `30`, and `held = TRUE`, before old timeout `33`. These state
values distinguish missing notification from eventual timeout rescue.

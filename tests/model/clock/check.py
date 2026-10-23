#!/usr/bin/env python3
from __future__ import annotations

import argparse
from collections.abc import Callable
from dataclasses import asdict, dataclass, replace
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time

JAR_SHA256 = "936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88"
VERSION = "TLC2 Version 2.19 of 08 August 2024 (rev: 5a47802)"
SOURCE = Path(__file__).resolve().parent
REPOSITORY = SOURCE.parents[2]
IDENTIFIER = re.compile(r"[A-Za-z][A-Za-z0-9_]*\Z")


@dataclass(frozen=True)
class Counterexample:
    kind: str
    property: str


@dataclass(frozen=True)
class Case:
    name: str
    module: str
    constants: dict[str, str | int | bool]
    invariants: tuple[str, ...]
    temporal: tuple[str, ...] = ()
    expected: Counterexample | None = None
    timeout: float = 90
    category: str = "correctness"


def protocol(name: str, *, operation: str = "Advance", window: str = "During",
             mutation: str = "None", callers: int = 1, timer: bool = False, advance: int = 2,
             rescue: bool = False, spurious: bool = False, cap: int = 3,
             temporal: tuple[str, ...] = ("RequestProgress",),
             expected: Counterexample | None = None,
             witness: str | None = None) -> Case:
    constants = dict(Operation=operation, Window=window, Mutation=mutation,
                     Callers=callers, Rescue=rescue, Spurious=spurious, Cap=cap,
                     NeedTimer=timer, AdvanceAmount=advance)
    invariants = ("TypeOK", "TimerNotEarly", "BackendRetention", "AutoSnapshotGate", "ZeroWaitBypassesAuto")
    if mutation == "DropToken":
        invariants = ("TypeOK", "TimerNotEarly")
    if witness:
        invariants += (witness,)
        temporal = ()
        expected = Counterexample("invariant", witness)
    elif expected and expected.kind == "invariant":
        invariants += (expected.property,)
    return Case(name, "ClockProtocol", constants, invariants, temporal, expected,
                category="witness" if witness else "mutant" if expected else "correctness")


def response(name: str, *, expected: str | None = None, witness: str | None = None,
             **options: object) -> Case:
    base = protocol(name, rescue=True, temporal=(), **options)
    constants = base.constants | dict(CallerBudget=1, BackendBudget=1, DriverBudget=1)
    invariant = witness or expected or "ConditionalResponse"
    return replace(base, module="ClockResponse", constants=constants,
                   invariants=("TypeOK", invariant), temporal=("OldestAgePreserved",) if not expected and not witness else (),
                   expected=Counterexample("invariant", invariant) if witness or expected else None,
                   category="witness" if witness else "mutant" if expected else "correctness")


def lifecycle(name: str, *, workload: str = "Advances", rule: str = "Conditional", cap: int = 3,
              expected: str | None = None, witness: str | None = None,
              completion: bool = False, finite_timeout: bool = True) -> Case:
    invariant = witness or expected
    invariants = ("TypeOK", "TimerNotEarly")
    if invariant:
        invariants += (invariant,)
    elif rule == "Conditional":
        invariants += ("AutoEligible", "AutoWithinTarget", "NoPanic")
    return Case(name, "ClockLifecycle", dict(Workload=workload, Rule=rule, Cap=cap,
                                             FiniteTimeout=finite_timeout),
                invariants, ("ClockMonotonic",) + (("Completion",) if completion else ()),
                Counterexample("invariant", invariant) if invariant else None,
                category="witness" if witness else "mutant" if expected else "correctness")


def cases() -> tuple[Case, ...]:
    matrix: list[Case] = []
    for operation in ("Advance", "Resume", "Release"):
        for window in ("Before", "During"):
            suffix = f"{operation.lower()}.{window.lower()}"
            matrix += [protocol(f"protocol.{suffix}", operation=operation, window=window),
                       response(f"response.{suffix}", operation=operation, window=window)]
        matrix.append(response(f"response.{operation.lower()}.all-windows",
                               operation=operation, window="All"))
    matrix += [
        protocol("protocol.advance.partial", advance=1),
        response("response.advance.partial", advance=1),
        response("mutant.advance.partial", advance=1, mutation="NoWake", expected="ConditionalResponse"),
        protocol("protocol.all-windows", window="All", rescue=True, spurious=True),
        protocol("protocol.release.timer", operation="Release", timer=True,
                 temporal=("TimerProgress",)),
        protocol("protocol.release.cap-zero", operation="Release", cap=0, window="All"),
        protocol("protocol.release.overlap", operation="Release", callers=2, window="All"),
        response("response.release.overlap", operation="Release", callers=2),
        response("response.release.serial", operation="Release", callers=2, window="Serial"),
        protocol("mutant.advance.untimed", mutation="NoWake",
                 expected=Counterexample("temporal", "RequestProgress")),
        response("mutant.advance.response", mutation="NoWake", expected="ConditionalResponse"),
        response("mutant.resume.response", operation="Resume", mutation="NoWake", expected="ConditionalResponse"),
        response("mutant.release.final", operation="Release", callers=2, window="Serial",
                 mutation="NoFinalWake", expected="ConditionalResponse"),
        protocol("mutant.token.retention", window="Before", mutation="DropToken", temporal=(),
                 expected=Counterexample("invariant", "BackendRetention")),
        response("mutant.token.response", window="Before", mutation="DropToken", expected="ConditionalResponse"),
        protocol("witness.before", window="Before", witness="WitnessBefore"),
        protocol("witness.during", witness="WitnessDuring"),
        protocol("witness.flag-only", window="Before", witness="WitnessFlagOnly"),
        protocol("witness.token-only", operation="Release", window="All", witness="WitnessTokenOnly"),
        protocol("witness.nested-release", operation="Release", callers=2, window="Serial", witness="WitnessNested"),
        protocol("diagnostic.resume-snapshot", operation="Resume", window="Race", witness="WitnessRejected"),
        protocol("diagnostic.inhibit-snapshot", operation="Inhibit", window="Race", witness="WitnessInhibitedAdvance"),
        response("witness.oldest-age", operation="Release", callers=2, witness="WitnessOverlap"),
        response("witness.responded", witness="WitnessResponded"),
        response("witness.time-continues", witness="WitnessTimeContinues"),
        response("witness.before-old-timeout", mutation="NoWake", witness="WitnessBeforeOldTimeout"),
        lifecycle("lifecycle.advances.original", rule="Original", completion=True),
        lifecycle("lifecycle.nested.original", workload="Nested", rule="Original", completion=True),
        lifecycle("diagnostic.lifecycle.resume", rule="Original", workload="ResumePause", witness="NoPanic"),
        lifecycle("diagnostic.lifecycle.inhibit", rule="Original", workload="Inhibit", witness="AutoEligible"),
        lifecycle("diagnostic.lifecycle.stale", rule="Original", witness="AutoWithinTarget"),
        lifecycle("diagnostic.lifecycle.cap-target", rule="Original", cap=1, witness="AutoWithinTarget"),
        lifecycle("witness.lifecycle.repeated", witness="WitnessRepeated"),
        lifecycle("witness.lifecycle.registration", workload="Inhibit", witness="WitnessRegistration"),
        lifecycle("witness.lifecycle.stale", witness="WitnessStale"),
        lifecycle("witness.lifecycle.timers", witness="WitnessTimers"),
        lifecycle("witness.lifecycle.skipped", witness="WitnessSkipped"),
        replace(lifecycle("mutant.lifecycle.timeout", workload="ResumePause", finite_timeout=False),
                temporal=("Completion",), expected=Counterexample("temporal", "Completion"),
                category="mutant"),
        lifecycle("mutant.lifecycle.running", workload="ResumePause", rule="NoPaused", expected="AutoEligible"),
        lifecycle("mutant.lifecycle.inhibit", workload="Inhibit", rule="NoInhibit", expected="AutoEligible"),
        lifecycle("mutant.lifecycle.stale", rule="StaleRelative", expected="AutoWithinTarget"),
        Case("policy.precision-floor-cap", "WaitPolicy", {"RemoveFloor": False},
             ("ZeroPreserved", "CapDominates", "Precision", "PositiveInhibitedFloor")),
        Case("mutant.policy.floor", "WaitPolicy", {"RemoveFloor": True},
             ("PositiveInhibitedFloor",), expected=Counterexample("invariant", "PositiveInhibitedFloor"),
             category="mutant"),
    ]
    for workload in ("Advances", "ResumePause", "Inhibit", "Mixed", "Nested"):
        for cap in (3, 1):
            matrix.append(lifecycle(f"lifecycle.{workload.lower()}.cap{cap}",
                                    workload=workload, cap=cap, completion=True))
    return tuple(matrix)


def config(case: Case) -> str:
    names = (case.module, *case.constants, *case.invariants, *case.temporal)
    if not all(IDENTIFIER.fullmatch(name) for name in names):
        raise ValueError("invalid TLA identifier")
    if case.expected:
        if not IDENTIFIER.fullmatch(case.expected.property):
            raise ValueError("invalid expected property")
        configured = case.invariants if case.expected.kind == "invariant" else case.temporal
        if case.expected.kind not in {"invariant", "temporal"} or case.expected.property not in configured:
            raise ValueError("expected property is not configured")
        if case.expected.kind == "temporal" and case.temporal != (case.expected.property,):
            raise ValueError("temporal negative must configure exactly one property")
    lines = ["SPECIFICATION Spec", "CHECK_DEADLOCK FALSE", "CONSTANTS"]
    for key, value in case.constants.items():
        literal = str(value).upper() if isinstance(value, bool) else json.dumps(value)
        lines.append(f"{key} = {literal}")
    if case.invariants:
        lines.append("INVARIANTS " + " ".join(case.invariants))
    if case.temporal:
        lines.append("PROPERTIES " + " ".join(case.temporal))
    return "\n".join(lines) + "\n"


def validate(case: Case, status: int, output: str) -> dict[str, object]:
    config(case)
    if VERSION not in output:
        raise ValueError("missing pinned TLC identity")
    if not re.search(r"^Finished in .+ at \(.+\)$", output, re.MULTILINE):
        raise ValueError("missing completed TLC footer")
    counts = re.findall(r"^(\d+) states generated, (\d+) distinct states found, (\d+) states left on queue\.$",
                        output, re.MULTILINE)
    if not counts or int(counts[-1][1]) == 0:
        raise ValueError("missing state summary")
    errors = re.findall(r"^Error: (.+)$", output, re.MULTILINE)
    expected = case.expected
    if expected is None:
        if status != 0 or errors or "Model checking completed. No error has been found." not in output:
            raise ValueError(f"expected complete success, got exit {status}: {errors}")
        if counts[-1][2] != "0":
            raise ValueError("positive exploration left queued states")
    else:
        if not re.search(r"^State 1: <Initial predicate>$", output, re.MULTILINE):
            raise ValueError("missing real counterexample trace")
        if not re.search(r"^(?:/\\ )?(?:p|o|sample) = ", output, re.MULTILINE):
            raise ValueError("missing counterexample state assignment")
        if expected.kind == "invariant":
            expected_error = f"Invariant {expected.property} is violated."
            initial_error = f"Invariant {expected.property} is violated by the initial state:"
            allowed = {expected_error, initial_error, "The behavior up to this point is:"}
            if status != 12 or not any(error in {expected_error, initial_error} for error in errors):
                raise ValueError(f"wrong invariant diagnostic, exit {status}: {errors}")
        else:
            allowed = {"Temporal properties were violated.", "The following behavior constitutes a counter-example:"}
            if status != 13 or set(errors) != allowed:
                raise ValueError(f"wrong temporal diagnostic, exit {status}: {errors}")
            if not re.search(r"(^State \d+: Stuttering$|^Back to state \d+)", output, re.MULTILINE):
                raise ValueError("missing temporal counterexample loop")
        if not set(errors) <= allowed:
            raise ValueError(f"unrelated TLC error: {errors}")
    return {"exit_code": status, "distinct_states": int(counts[-1][1]),
            "generated_states": int(counts[-1][0]), "category": case.category,
            "expected": asdict(expected) if expected else "complete success"}


def run_process(command: list[str], directory: Path, timeout: float) -> tuple[int, str]:
    try:
        result = subprocess.run(command, cwd=directory, stdout=subprocess.PIPE,
                                stderr=subprocess.STDOUT, text=True, timeout=timeout, check=False)
    except subprocess.TimeoutExpired as error:
        output = error.stdout or b""
        if isinstance(output, bytes):
            output = output.decode(errors="replace")
        (directory / "tlc.log").write_text(output)
        raise ValueError(f"subprocess timed out after {timeout}s") from error
    (directory / "tlc.log").write_text(result.stdout)
    return result.returncode, result.stdout


def command(jar: Path, directory: Path, module: Path, cfg: Path) -> list[str]:
    temporary = directory / "tmp"
    temporary.mkdir(exist_ok=True)
    return ["java", f"-Djava.io.tmpdir={temporary}", "-Xmx1g", "-XX:+UseParallelGC",
            "-cp", str(jar), "tlc2.TLC", "-workers", "1", "-seed", "1", "-fp", "0",
            "-metadir", str(directory / "states"), "-config", str(cfg), module.name]


def run_case(case: Case, jar: Path, root: Path) -> dict[str, object]:
    directory = root / case.name
    directory.mkdir()
    for model in SOURCE.glob("*.tla"):
        shutil.copyfile(model, directory / model.name)
    cfg = directory / f"{case.module}.cfg"
    cfg.write_text(config(case))
    argv = command(jar, directory, SOURCE / f"{case.module}.tla", cfg)
    (directory / "command.json").write_text(json.dumps(argv, indent=2) + "\n")
    started = time.monotonic()
    try:
        status, output = run_process(argv, directory, case.timeout)
        result = validate(case, status, output) | {"passed": True}
    except (ValueError, OSError) as error:
        result = {"passed": False, "error": str(error)}
    result |= {"case": case.name, "seconds": round(time.monotonic() - started, 3)}
    (directory / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result), flush=True)
    return result


def self_check(jar: Path, root: Path) -> dict[str, object]:
    directory = root / "runner.self-check"
    directory.mkdir()
    base = protocol("invalid", mutation="NoWake", expected=Counterexample("temporal", "RequestProgress"))
    cfg = directory / "Invalid.cfg"
    cfg.write_text(config(base))
    module = directory / "Invalid.tla"
    module.write_text("---- MODULE Invalid ----\nTHIS IS INVALID\n====\n")
    rejected: list[str] = []

    def must_reject(label: str, action: Callable[[], object]) -> None:
        try:
            action()
        except (ValueError, OSError):
            rejected.append(label)
        else:
            raise ValueError(f"validator accepted {label}")

    invalid_command = command(jar, directory, module, cfg)
    (directory / "command.json").write_text(json.dumps(invalid_command, indent=2) + "\n")
    status, output = run_process(invalid_command, directory, 15)
    if status != 150 or "Parsing or semantic analysis failed" not in output:
        raise ValueError(f"invalid-module self-check did not reach parser: exit {status}")
    (directory / "invalid-module.log").write_text(output)
    must_reject("actual TLC invalid module", lambda: validate(base, status, output))
    must_reject("actual subprocess timeout", lambda: run_process(
        [sys.executable, "-c", "import time; time.sleep(5)"], directory, 0.01))
    footer = "\n2 states generated, 2 distinct states found, 0 states left on queue.\nFinished in 00s at (test)\n"
    invariant = replace(base, invariants=("BackendRetention",), temporal=(),
                        expected=Counterexample("invariant", "BackendRetention"))
    sample = VERSION + "\nError: Invariant BackendRetention is violated.\nError: The behavior up to this point is:\nState 1: <Initial predicate>\np = 0\n" + footer
    validate(invariant, 12, sample)
    for label, bad_status, bad_output in (
        ("unrelated property", 12, sample.replace("Invariant BackendRetention", "Invariant Other")),
        ("wrong exit kind", 150, sample),
        ("truncated output", 12, sample[:sample.index("Finished in")]),
        ("Java failure", 12, sample + "Error: Java failure\n"),
        ("clean negative", 0, VERSION + "\nModel checking completed. No error has been found.\n" + footer),
    ):
        must_reject(label, lambda code=bad_status, text=bad_output: validate(invariant, code, text))
    must_reject("malformed property", lambda: config(replace(invariant, expected=Counterexample("invariant", "Bad Name"))))
    result = {"case": "runner.self-check", "passed": True, "rejected": rejected}
    (directory / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result), flush=True)
    return result


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Run the pinned, finite clock verification matrix without downloading tools.")
    parser.add_argument("--tlc-jar", type=Path, required=True)
    parser.add_argument("--results", type=Path, required=True, help="new directory outside repository")
    parser.add_argument("--case", action="append", default=[], help="exact case name; repeat to select")
    args = parser.parse_args()
    jar = args.tlc_jar.resolve()
    root = args.results.resolve()
    try:
        if hashlib.sha256(jar.read_bytes()).hexdigest() != JAR_SHA256:
            raise ValueError("TLC jar SHA256 does not match pinned official artifact")
        if root.is_relative_to(REPOSITORY):
            raise ValueError("results must be outside repository")
        matrix = cases()
        names = {case.name for case in matrix} | {"runner.self-check"}
        if len(names) != len(matrix) + 1:
            raise ValueError("duplicate case name")
        if unknown := set(args.case) - names:
            raise ValueError(f"unknown cases: {sorted(unknown)}")
        selected = [case for case in matrix if not args.case or case.name in args.case]
        include_self = not args.case or "runner.self-check" in args.case
        if not selected and not include_self:
            raise ValueError("empty case selection")
        root.mkdir(parents=True, exist_ok=False)
        (root / "tool.json").write_text(json.dumps({"jar": str(jar), "sha256": JAR_SHA256,
                                                    "version": VERSION}, indent=2) + "\n")
        results = [run_case(case, jar, root) for case in selected]
        if include_self:
            results.append(self_check(jar, root))
        (root / "summary.json").write_text(json.dumps(results, indent=2) + "\n")
        return 0 if all(result["passed"] for result in results) else 1
    except (OSError, ValueError) as error:
        print(f"verification failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())

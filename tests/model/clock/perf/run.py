#!/usr/bin/env python3
"""Build identical probes against two local Tokio checkouts; retain raw results."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import statistics
import subprocess
import sys
import time


HERE = Path(__file__).resolve().parent
CASES = ("paused-cpu", "wake", "timers", "tcp", "advance", "resume")
METRICS = ("process_cpu_ns", "runtime_thread_cpu_ns", "wake_p50_ns", "wake_p99_ns", "timer_p50_ns", "timer_p99_ns")


def command(args, **kwargs):
    return subprocess.run(args, text=True, check=True, **kwargs)


def git(root, *args):
    return command(["git", "-C", str(root), *args], capture_output=True).stdout.strip()


def runtime_diff_hash(root):
    return hashlib.sha256(git(root, "diff", "HEAD", "--", "tokio/src", "tokio/Cargo.toml").encode()).hexdigest()


def manifest(root):
    return f'''[package]
name = "clock-response-probe"
version = "0.0.0"
edition = "2021"

[workspace]

[features]
io = ["tokio/net"]
parking = ["tokio/parking_lot"]

[dependencies]
tokio = {{ path = {json.dumps(str(root / "tokio"))}, default-features = false, features = ["rt", "time", "sync", "test-util"] }}
'''


def prepare(args):
    args.output.mkdir(parents=True, exist_ok=True)
    provenance = {"source_sha256": hashlib.sha256((HERE / "probe.rs").read_bytes()).hexdigest(), "implementations": {}}
    lock = args.output / "Cargo.lock"
    for label, root in (("base", args.base), ("fix", args.fix)):
        provenance["implementations"][label] = {"root": str(root), "head": git(root, "rev-parse", "HEAD"), "dirty": git(root, "status", "--short"), "runtime_diff_sha256": runtime_diff_hash(root)}
        for io in (False, True):
            for parking in (False, True):
                name = f"{label}-io{int(io)}-{'parking' if parking else 'std'}"
                directory = args.output / "builds" / name
                (directory / "src").mkdir(parents=True, exist_ok=True)
                (directory / "Cargo.toml").write_text(manifest(root))
                shutil.copyfile(HERE / "probe.rs", directory / "src" / "main.rs")
                cargo = ["cargo", "--offline"]
                if not lock.exists():
                    command(cargo + ["generate-lockfile", "--manifest-path", str(directory / "Cargo.toml")])
                    shutil.copyfile(directory / "Cargo.lock", lock)
                shutil.copyfile(lock, directory / "Cargo.lock")
                features = [feature for enabled, feature in ((io, "io"), (parking, "parking")) if enabled]
                build = cargo + ["build", "--release", "--locked", "--manifest-path", str(directory / "Cargo.toml"), "--target-dir", str(args.target)]
                if features:
                    build += ["--features", ",".join(features)]
                with (directory / "build.log").open("w") as log:
                    command(build, stdout=log, stderr=subprocess.STDOUT)
                shutil.copy2(args.target / "release" / "clock-response-probe", directory / "probe")
                print(f"built {name}", flush=True)
    provenance["lock_sha256"] = hashlib.sha256(lock.read_bytes()).hexdigest()
    provenance["rustc"] = command(["rustc", "--version", "--verbose"], capture_output=True).stdout
    (args.output / "build-provenance.json").write_text(json.dumps(provenance, indent=2) + "\n")


def machine():
    return {
        "time_unix": time.time(),
        "platform": platform.platform(),
        "cpu_model": next((line.split(":", 1)[1].strip() for line in Path("/proc/cpuinfo").read_text().splitlines() if line.startswith("model name")), "unknown"),
        "cpus_allowed": sorted(os.sched_getaffinity(0)),
        "load_average": os.getloadavg(),
        "uptime": command(["uptime"], capture_output=True).stdout.strip(),
        "processes": command(["ps", "-eo", "pid,comm,pcpu,args", "--sort=-pcpu"], capture_output=True).stdout.splitlines()[:20],
    }


def load_processes(cpus, count):
    processes = []
    for cpu in cpus[:count]:
        processes.append(subprocess.Popen(["taskset", "-c", str(cpu), sys.executable, "-c", "while True: pass"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
    return processes


def run(args):
    if args.repetitions < 5 and not args.smoke:
        raise SystemExit("comparisons require at least five repetitions; use --smoke for harness checks")
    provenance = json.loads((args.output / "build-provenance.json").read_text())
    for label, root in (("base", args.base), ("fix", args.fix)):
        if root.resolve() != Path(provenance["implementations"][label]["root"]).resolve():
            raise SystemExit("checkout path changed; prepare again before running")
    for item in provenance["implementations"].values():
        if git(item["root"], "rev-parse", "HEAD") != item["head"]:
            raise SystemExit("checkout HEAD changed; prepare again before running")
        if runtime_diff_hash(item["root"]) != item["runtime_diff_sha256"]:
            raise SystemExit("runtime source changed; prepare again before running")
    source_sha = hashlib.sha256((HERE / "probe.rs").read_bytes()).hexdigest()
    if provenance["source_sha256"] != source_sha:
        raise SystemExit("probe source changed; prepare again before running")
    directory = args.output / args.name
    directory.mkdir(parents=True, exist_ok=False)
    cpus = sorted(os.sched_getaffinity(0))
    cpu = args.cpu if args.cpu is not None else cpus[0]
    if cpu not in cpus:
        raise SystemExit("requested CPU outside allowed affinity")
    cpus.remove(cpu)
    cpus.insert(0, cpu)
    load_count = min(4, len(cpus) // 2)
    if not load_count and not args.smoke:
        raise SystemExit("loaded comparisons require at least two allowed CPUs")
    info = {"before": machine(), "probe_cpu": cpu, "loaded_workers": load_count, "loaded_cpus": cpus[:load_count], "arguments": vars(args).copy(), "build_provenance": provenance}
    info["arguments"] = {key: str(value) if isinstance(value, Path) else value for key, value in info["arguments"].items()}
    (directory / "metadata.json").write_text(json.dumps(info, indent=2) + "\n")
    records = []
    with (directory / "raw.jsonl").open("w") as output:
        for loaded in (False, True):
            if args.smoke and loaded:
                continue
            loaders = load_processes(cpus, load_count) if loaded else []
            if loaded:
                time.sleep(0.2)
                info["loaded_before"] = machine()
            try:
                for io in (False, True):
                    for parking in (False, True):
                        for case in CASES:
                            if case == "tcp" and not io:
                                continue
                            for repetition in range(args.repetitions):
                                labels = ("base", "fix")
                                for label in labels:
                                    name = f"{label}-io{int(io)}-{'parking' if parking else 'std'}"
                                    binary = args.output / "builds" / name / "probe"
                                    record = {"implementation": label, "io": io, "parking_lot": parking, "loaded": loaded, "case": case, "repetition": repetition, "started_unix": time.time()}
                                    try:
                                        result = subprocess.run(["taskset", "-c", str(cpu), str(binary), case, str(args.samples), str(args.hold_ms)], text=True, capture_output=True, timeout=args.watchdog)
                                        record.update(returncode=result.returncode, stdout=result.stdout, stderr=result.stderr)
                                        events = [json.loads(line) for line in result.stdout.splitlines() if line.startswith("{")]
                                        record["ready"] = next((event for event in events if event["event"] == "ready"), None)
                                        record["result"] = next((event for event in events if event["event"] == "result"), None)
                                        expected_wakes = args.samples if case in ("wake", "tcp", "timers") else int(case in ("advance", "resume"))
                                        expected_timers = args.samples if case == "timers" else 0
                                        observed = record["result"]
                                        record["valid"] = bool(result.returncode == 0 and observed and observed["errors"] == 0 and observed["wake_count"] == expected_wakes and observed["timer_count"] == expected_timers)
                                    except subprocess.TimeoutExpired as error:
                                        stdout = error.stdout or b""
                                        stderr = error.stderr or b""
                                        record.update(valid=False, watchdog_expired=True, stdout=stdout.decode() if isinstance(stdout, bytes) else stdout, stderr=stderr.decode() if isinstance(stderr, bytes) else stderr)
                                        events = [json.loads(line) for line in record["stdout"].splitlines() if line.startswith("{")]
                                        record["ready"] = next((event for event in events if event["event"] == "ready"), None)
                                    record["known_baseline_missing_wake"] = bool(label == "base" and case in ("advance", "resume") and record.get("watchdog_expired") and record.get("ready"))
                                    output.write(json.dumps(record) + "\n")
                                    output.flush()
                                    records.append(record)
                                    print(f"{name} {'loaded' if loaded else 'idle'} {case} #{repetition + 1} {'ok' if record['valid'] else 'FAILED'}", flush=True)
            finally:
                if loaded:
                    info["loaded_after"] = machine()
                for process in loaders:
                    process.terminate()
                for process in loaders:
                    process.wait()
    groups = {}
    for record in records:
        key = (record["implementation"], record["io"], record["parking_lot"], record["loaded"], record["case"])
        groups.setdefault(key, []).append(record)
    summary = []
    for key, rows in groups.items():
        valid = [row["result"] for row in rows if row["valid"]]
        entry = dict(zip(("implementation", "io", "parking_lot", "loaded", "case"), key))
        entry.update(valid_runs=len(valid), failed_runs=len(rows) - len(valid))
        for metric in METRICS:
            values = [row[metric] for row in valid]
            entry[metric] = {"median": statistics.median(values), "min": min(values), "max": max(values)} if values else None
        entry["process_cpu_percent"] = statistics.median(row["process_cpu_ns"] / row["wall_ns"] * 100 for row in valid) if valid else None
        summary.append(entry)
    (directory / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    info["after"] = machine()
    (directory / "metadata.json").write_text(json.dumps(info, indent=2) + "\n")
    print(f"saved {directory}")
    unexpected = [record for record in records if not record["valid"] and not record["known_baseline_missing_wake"]]
    if unexpected:
        raise SystemExit(f"{len(unexpected)} unexpected failed runs; inspect raw.jsonl")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("prepare", "run"))
    parser.add_argument("--base", type=Path, required=True)
    parser.add_argument("--fix", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--target", type=Path, required=True)
    parser.add_argument("--name", default="comparison")
    parser.add_argument("--repetitions", type=int, default=5)
    parser.add_argument("--samples", type=int, default=200)
    parser.add_argument("--hold-ms", type=int, default=800)
    parser.add_argument("--watchdog", type=float, default=3)
    parser.add_argument("--cpu", type=int)
    parser.add_argument("--smoke", action="store_true")
    args = parser.parse_args()
    for name in ("base", "fix", "output", "target"):
        setattr(args, name, getattr(args, name).resolve())
    if args.samples <= 0 or args.hold_ms <= 0 or args.watchdog <= 0:
        parser.error("samples, hold-ms, watchdog must be positive")
    (prepare if args.action == "prepare" else run)(args)


if __name__ == "__main__":
    main()

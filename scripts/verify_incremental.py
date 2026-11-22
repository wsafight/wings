import argparse
from contextlib import contextmanager
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import resource
import signal
import subprocess
import sys
import time

import verify_workspace as workspace


ROOT = Path(__file__).resolve().parent.parent
GLOBAL_INPUTS = ("Cargo.toml", "Cargo.lock", ".cargo/", "rust-toolchain", "rust-toolchain.toml", "scripts/", ".github/", "AGENTS.md")
SMOKE_PACKAGES = {"wings-abi", "wings-ir", "wings-codegen", "wings-artifact", "wings-compile", "wings-vm"}


def git(root, *arguments):
    return subprocess.check_output(["git", *arguments], cwd=root)


def tracked_inputs(root):
    return sorted(set(os.fsdecode(name) for name in git(
        root, "ls-files", "--cached", "--others", "--exclude-standard", "-z"
    ).split(b"\0") if name))


def changed_files(root, base):
    changes = set()
    if base:
        ancestor = git(root, "merge-base", base, "HEAD").decode().strip()
        changes.update(git(root, "diff", "--name-only", "--no-renames", "-z", ancestor, "HEAD").split(b"\0"))
    for arguments in (("diff", "--cached"), ("diff",), ("ls-files", "--others", "--exclude-standard")):
        options = ("-z",) if arguments[0] == "ls-files" else ("--name-only", "--no-renames", "-z")
        changes.update(git(root, *arguments, *options).split(b"\0"))
    return sorted(os.fsdecode(name) for name in changes if name)


def package_graph(root):
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--no-deps", "--locked", "--format-version=1"], cwd=root
    ))
    members = set(metadata["workspace_members"])
    packages = {package["name"]: package for package in metadata["packages"] if package["id"] in members}
    graph = {name: {dependency["name"] for dependency in package["dependencies"] if dependency["name"] in packages}
             for name, package in packages.items()}
    directories = {name: str(Path(package["manifest_path"]).parent.relative_to(root)) + "/"
                   for name, package in packages.items()}
    return graph, directories


def closure(seeds, graph, reverse=False):
    selected = set(seeds)
    while True:
        added = {name for name, dependencies in graph.items() if dependencies & selected} if reverse else {
            dependency for name in selected for dependency in graph[name]
        }
        if added <= selected:
            return selected
        selected.update(added)


def make_plan(changes, graph, directories, all_tasks=False):
    seeds = set(graph) if all_tasks else set()
    python = site = all_tasks
    smoke = all_tasks
    reasons = []
    documentation = False
    for filename in changes:
        owners = {name for name, directory in directories.items() if filename.startswith(directory)}
        if owners:
            seeds.update(owners)
        elif filename in {"Cargo.toml", "Cargo.lock", "rust-toolchain", "rust-toolchain.toml"} or filename.startswith(".cargo/"):
            seeds.update(graph)
        elif filename.startswith("examples/"):
            seeds.update({"wings-compile", "wings-vm"} & graph.keys())
            smoke = True
        elif filename.startswith(("scripts/", ".github/workflows/")) or filename == "AGENTS.md":
            python = True
            if filename in {"scripts/verify_smoke.py", "scripts/verify_scalar_aot.py"}:
                seeds.update({"wings-compile", "wings-vm"} & graph.keys())
                smoke = True
        elif filename.startswith(("docs/", "site/")) or filename in {"README.md", "README.zh-CN.md"}:
            site = True
            documentation = True
        elif filename not in {"LICENSE", ".gitignore"}:
            seeds.update(graph)
            python = site = smoke = True
            reasons.append(f"unmapped path: {filename}; expanded to all incremental checks")
    packages = sorted(closure(seeds, graph, reverse=True))
    smoke = smoke or bool(SMOKE_PACKAGES & set(packages))
    sources = [directories[name] for name in sorted(closure(set(packages), graph))]
    tasks = []

    def add(name, kind, inputs, commands=(), dependencies=()):
        tasks.append({"name": name, "kind": kind, "inputs": sorted(set((*GLOBAL_INPUTS, *inputs))),
                      "commands": commands, "dependencies": list(dependencies)})

    if python or packages or documentation:
        add("python", "command", ["examples/"], [[sys.executable, "-m", "unittest", "discover", "-s", "scripts", "-p", "*_tests.py"]])
    if packages:
        options = [option for package in packages for option in ("-p", package)]
        add("format", "command", list(directories.values()), [["cargo", "fmt", "--all", "--", "--check"]])
        add("isolation", "isolation", list(directories.values()))
        add("lint", "command", [*sources, "examples/"],
            [["cargo", "clippy", "--all-targets", "--locked", "--no-deps", *options, "--", "-D", "warnings"]], ["format"])
        add("rust-tests", "rust-tests", [*sources, "examples/"], dependencies=["lint", "isolation"])
        examples = sorted({Path(filename).stem for filename in changes if any(
            filename.startswith(directory + "examples/") and filename.endswith(".rs")
            for directory in directories.values()
        ) and (ROOT / filename).is_file()})
        tasks[-1]["targets"] = ["--lib", "--bins", "--tests", *[
            option for example in examples for option in ("--example", example)
        ]]
    if smoke:
        add("aot-smoke", "command", [*directories.values(), "examples/"], [
            ["cargo", "build", "--locked", "--target-dir", "target", "-p", "wings-compile", "-p", "wings-vm", "--bins", "--example", "mvp_runtime"],
            [sys.executable, "scripts/verify_smoke.py", "--target-dir", "target"],
        ], ["rust-tests"] if packages else [])
    if site:
        add("site", "command", ["site/", "docs/", "README.md", "README.zh-CN.md"],
            [["npm", "--prefix", "site", "ci"], ["npm", "--prefix", "site", "run", "build"]])
    return {"level": "incremental", "changed_files": changes, "packages": packages, "reasons": reasons,
            "tasks": tasks, "release_gate_run": False, "performance_acceptance_run": False}


def matches(filename, selectors):
    return any(filename.startswith(selector) if selector.endswith("/") else filename == selector for selector in selectors)


def fingerprint(root, task, context):
    checksum = hashlib.sha256(json.dumps([task, context], sort_keys=True).encode())
    for filename in tracked_inputs(root):
        if not matches(filename, task["inputs"]):
            continue
        source = root / filename
        checksum.update(os.fsencode(filename) + b"\0")
        if source.is_symlink():
            checksum.update(b"symlink\0" + os.fsencode(os.readlink(source)))
        elif source.is_file():
            checksum.update(str(source.stat().st_mode & 0o777).encode() + b"\0")
            checksum.update(source.read_bytes())
        else:
            checksum.update(b"deleted\0")
        checksum.update(b"\0")
    return checksum.hexdigest()


def source_snapshot(root):
    return fingerprint(root, {"inputs": tracked_inputs(root)}, {})


def execution_context(plan):
    tools = [["rustc", "-vV"], ["cargo", "--version"]] if plan["packages"] or any(
        task["name"] == "aot-smoke" for task in plan["tasks"]
    ) else []
    if any(task["name"] == "site" for task in plan["tasks"]):
        tools.extend([["node", "--version"], ["npm", "--version"]])
    configuration = []
    for folder in (ROOT, *ROOT.parents, Path.home() / ".cargo"):
        for filename in ("config", "config.toml"):
            source = folder / filename if folder.name == ".cargo" else folder / ".cargo" / filename
            if source.is_file():
                configuration.append([str(source), workspace.digest(source)])
    environment = {name: value for name, value in os.environ.items() if name not in {"_", "SHLVL", "CODEX_TOOL_CALL_ID"}}
    return {"platform": platform.platform(), "machine": platform.machine(), "python": sys.version,
            "python_executable": sys.executable, "configuration": configuration,
            "environment_sha256": hashlib.sha256(json.dumps(environment, sort_keys=True).encode()).hexdigest(),
            "tools": [subprocess.check_output(command, cwd=ROOT, text=True).strip() for command in tools]}


@contextmanager
def exclusive_run(directory):
    directory.mkdir(parents=True, exist_ok=True)
    with (directory / "lock").open("a") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise ValueError("another incremental verification is running; retry after it finishes") from None
        try:
            yield
        finally:
            fcntl.flock(lock, fcntl.LOCK_UN)


def run_command(command, root, log, timeout=900):
    def limits():
        resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
        resource.setrlimit(resource.RLIMIT_CPU, (timeout, timeout))

    print("+ " + " ".join(command), flush=True)
    child = subprocess.Popen(command, cwd=root, stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
                             start_new_session=True, preexec_fn=limits)
    try:
        child.wait(timeout=timeout)
        if child.returncode:
            raise ValueError(f"command exited {child.returncode}: {' '.join(command)}")
    finally:
        workspace.stop_group(child)


def verify_isolation(root, log):
    for package, forbidden in (
        ("wings-lsp", r"^(?:cranelift-|wings-(?:codegen|ir|vm|compile|abi|artifact)\s)"),
        ("wings-vm", r"^(?:cranelift-|wings-(?:codegen|ir|compile|source|syntax|sema|lsp)\s)"),
    ):
        output = subprocess.check_output(["cargo", "tree", "-p", package, "--locked", "--prefix", "none"], cwd=root, text=True)
        log.write(output)
        if re.search(forbidden, output, re.MULTILINE):
            raise ValueError(f"dependency isolation violated: {package}")
    output = subprocess.check_output(["cargo", "tree", "-p", "wings-vm", "--locked", "-e", "features"], cwd=root, text=True)
    log.write(output)
    if re.search(r'corosensei feature "(?:default|unwind|asm-unwind)"', output):
        raise ValueError("corosensei must not enable unwinding")


def execute_task(root, task, packages, output):
    if task["kind"] == "rust-tests":
        if not workspace.run("debug", output / "harnesses", packages, task["targets"]):
            raise ValueError(f"Rust tests failed; see {output / 'harnesses/report.json'}")
        return
    with (output / "command.log").open("w") as log:
        if task["kind"] == "isolation":
            verify_isolation(root, log)
        else:
            for command in task["commands"]:
                run_command(command, root, log)


def run_plan(root, plan, directory, use_cache=True):
    with exclusive_run(directory):
        output = directory / "runs" / str(time.time_ns())
        output.mkdir(parents=True)
        report = {**plan, "status": "failed", "results": []}
        cache = directory / "cache"
        cache.mkdir(exist_ok=True)
        try:
            context = execution_context(plan)
            snapshot = source_snapshot(root)
            keys = {task["name"]: fingerprint(root, {**task, "packages": plan["packages"]}, context) for task in plan["tasks"]}
            for task in plan["tasks"]:
                name = task["name"]
                key = keys[name]
                evidence = cache / f"{key}.json"
                result = {"task": name, "fingerprint": key, "status": "failed"}
                report["results"].append(result)
                completed = {entry["task"] for entry in report["results"] if entry["status"] in {"passed", "cached"}}
                if not set(task["dependencies"]) <= completed:
                    raise ValueError(f"task dependencies were not satisfied: {name}")
                cached = None
                if use_cache and evidence.is_file():
                    try:
                        cached = json.loads(evidence.read_text())
                    except (OSError, ValueError):
                        pass
                if isinstance(cached, dict) and cached.get("fingerprint") == key and cached.get("status") == "passed":
                    result["status"] = "cached"
                    print(f"CACHED {name}", flush=True)
                    continue
                destination = output / name
                destination.mkdir()
                print(f"RUN {name}", flush=True)
                started = time.monotonic()
                try:
                    execute_task(root, task, plan["packages"], destination)
                except BaseException:
                    evidence.unlink(missing_ok=True)
                    raise
                result["elapsed_seconds"] = time.monotonic() - started
                if fingerprint(root, {**task, "packages": plan["packages"]}, context) != key:
                    result["status"] = "inconclusive"
                    report["status"] = "inconclusive"
                    raise ValueError("inputs changed during verification; rerun before relying on the result")
                result["status"] = "passed"
                if use_cache:
                    temporary = evidence.with_suffix(".tmp")
                    temporary.write_text(json.dumps(result, indent=2) + "\n")
                    temporary.replace(evidence)
            if source_snapshot(root) != snapshot or any(
                fingerprint(root, {**task, "packages": plan["packages"]}, context) != keys[task["name"]] for task in plan["tasks"]
            ):
                report["status"] = "inconclusive"
                raise ValueError("inputs changed during verification; rerun before relying on the result")
            report["status"] = "passed"
        except (OSError, ValueError, subprocess.SubprocessError, KeyboardInterrupt) as error:
            report["error"] = str(error) or "interrupted"
            print(f"FAILED: {report['error']}", file=sys.stderr)
        finally:
            (output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        print(f"{report['status']}: incremental checks only; report: {output / 'report.json'}", flush=True)
        return report["status"] == "passed"


def main():
    parser = argparse.ArgumentParser(description="Select and cache bounded incremental checks; never run release/performance gates")
    parser.add_argument("--base", help="include committed changes since the merge-base with this ref; default: working tree only")
    parser.add_argument("--all", action="store_true", help="select every incremental task, not the release suite")
    parser.add_argument("--plan", action="store_true", help="print selection without executing checks")
    parser.add_argument("--no-cache", action="store_true", help="execute selected checks without reusing or writing successful evidence")
    arguments = parser.parse_args()
    try:
        graph, directories = package_graph(ROOT)
        plan = make_plan(changed_files(ROOT, arguments.base), graph, directories, arguments.all)
        if arguments.plan:
            print(json.dumps(plan, indent=2))
            return 0
        return 0 if run_plan(ROOT, plan, ROOT / "target/incremental-validation", not arguments.no_cache) else 1
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        parser.exit(1, f"incremental verification: {error}\n")


if __name__ == "__main__":
    raise SystemExit(main())

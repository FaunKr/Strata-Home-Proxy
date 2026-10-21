#!/usr/bin/env python3
"""Builds a repo task of the code benchmark from a real commit, at image build.

  prepare_repo_task.py <task folder> <clone> <out>

The task folder (tasks/<name>) has repo.json: the feature's commit and the
test command. From the clone this writes <out>/repos/<name>/:

  checkout/   the tree before the commit as a single fresh commit, crates
              vendored, cargo offline, the test binaries prebuilt
  hidden/     the commit's test files, laid over the agent's work to score it
  task.json   the base commit, the command, the counted tests (those the
              commit added) and the tests that pass on the checkout

The vendored crates go to <out>/vendor. Before writing task.json it checks
the task itself: the commit's own change scores all counted tests and
breaks none, the untouched checkout scores fewer. Otherwise it fails, and
so does the image build.
"""

import json
import os
import shutil
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from runner import GIT_ENV, cargo_results  # noqa: E402

CARGO = os.environ.get("CARGO_BIN", "cargo")
GIT_ID = ["-c", "user.name=code-bench", "-c", "user.email=code-bench@localhost"]


def git(repo, *args, stdin=None):
    return subprocess.run(
        ["git", "-C", repo, *GIT_ID, *args],
        env=dict(os.environ, **GIT_ENV),
        input=stdin,
        capture_output=True,
        check=True,
    ).stdout


def cargo_test(folder, command):
    """Each test's outcome, as runner.cargo_results names them."""
    # One stream, as in the runner: the test lines follow their Running line.
    done = subprocess.run(
        [CARGO, *command],
        cwd=folder,
        env=dict(os.environ, CARGO_TERM_COLOR="never"),
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        errors="replace",
    )
    return cargo_results(done.stdout), done.stdout


def hidden_paths(clone, commit):
    """The files the commit added or changed under a tests folder."""
    names = git(clone, "diff", "--name-only", "--diff-filter=AM", f"{commit}^", commit).decode().split("\n")
    return sorted(n for n in names if n and "tests" in n.split("/")[:-1])


def lay_over(clone, commit, paths, folder):
    for path in paths:
        target = os.path.join(folder, path)
        os.makedirs(os.path.dirname(target), exist_ok=True)
        with open(target, "wb") as f:
            f.write(git(clone, "show", f"{commit}:{path}"))


def checkout(clone, commit, folder, vendor):
    """The tree before the commit as one fresh commit; returns its id."""
    os.makedirs(folder)
    tree = git(clone, "archive", "--format=tar", f"{commit}^")
    subprocess.run(["tar", "-x", "-C", folder], input=tree, check=True)
    # The crates from the image, never the internet.
    sources = subprocess.run(
        [CARGO, "vendor", "--locked", "--versioned-dirs", vendor],
        cwd=folder,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    config = os.path.join(folder, ".cargo", "config.toml")
    os.makedirs(os.path.dirname(config), exist_ok=True)
    with open(config, "a", encoding="utf-8") as f:
        f.write("\n# The code benchmark: crates from the image, no network.\n")
        f.write(sources)
        f.write("\n[net]\noffline = true\n")
    git(folder, "init", "-q", "-b", "main")
    git(folder, "add", "-A")
    git(folder, "commit", "-q", "-m", "The repo before the feature")
    return git(folder, "rev-parse", "HEAD").decode().strip()


def main(task_dir, clone, out):
    with open(os.path.join(task_dir, "repo.json"), encoding="utf-8") as f:
        spec = json.load(f)
    name = os.path.basename(os.path.normpath(task_dir))
    commit, command = spec["commit"], spec["command"]
    root = os.path.join(out, "repos", name)
    folder = os.path.join(root, "checkout")
    base = checkout(clone, commit, folder, os.path.join(out, "vendor"))

    # Builds the test binaries, so a run only builds what the agent changed.
    before, output = cargo_test(folder, command)
    if not before:
        sys.exit(f"{name}: the checkout's tests do not run:\n{output[-4000:]}")
    passing_before = sorted(k for k, v in before.items() if v == "ok")

    paths = hidden_paths(clone, commit)
    lay_over(clone, commit, paths, os.path.join(root, "hidden"))

    # The commit's own change: what full marks look like.
    reference = os.path.join(out, "reference")
    shutil.copytree(folder, reference, symlinks=True)
    try:
        change = git(clone, "diff", "--binary", f"{commit}^", commit)
        git(reference, "apply", "--whitespace=nowarn", "-", stdin=change)
        after, output = cargo_test(reference, command)
    finally:
        shutil.rmtree(reference)
    counted = sorted(k for k, v in after.items() if v == "ok" and k not in before)
    if not counted:
        sys.exit(f"{name}: the commit adds no test that passes:\n{output[-4000:]}")
    failing = sorted(k for k, v in after.items() if v != "ok" and k not in before)
    broken = sorted(k for k in passing_before if after.get(k) != "ok")
    if failing or broken:
        sys.exit(f"{name}: the commit itself fails {failing + broken}:\n{output[-4000:]}")

    # The untouched checkout with the commit's tests must not score full marks.
    untouched = os.path.join(out, "untouched")
    shutil.copytree(folder, untouched, symlinks=True)
    try:
        lay_over(clone, commit, paths, untouched)
        results, _ = cargo_test(untouched, command)
    finally:
        shutil.rmtree(untouched)
    passed = sum(1 for k in counted if results.get(k) == "ok")
    if passed >= len(counted):
        sys.exit(f"{name}: the untouched checkout already passes all {len(counted)} counted tests")

    with open(os.path.join(root, "task.json"), "w", encoding="utf-8") as f:
        json.dump(
            {"base": base, "command": command, "counted": counted, "passing_before": passing_before},
            f,
            indent=1,
        )
    print(f"{name}: {len(counted)} counted tests ({passed} pass untouched), "
          f"{len(passing_before)} passing before, {len(paths)} hidden files")


if __name__ == "__main__":
    if len(sys.argv) != 4:
        sys.exit(__doc__)
    main(*sys.argv[1:])

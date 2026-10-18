#!/usr/bin/env python3
"""PreToolUse guard for Bash: nothing reaches `main` except through a human.

Blocks (exit 2, reason on stderr):
- `gh pr merge` when the PR's base branch is `main`, or when the base cannot
  be determined (fails closed);
- `gh api` calls to a pull request or branch merge endpoint;
- `git push` that would update `main`: an explicit `main` destination,
  `--all`/`--mirror`, or a bare push/`HEAD` push from a branch that is `main`
  or tracks `main`.

Everything else exits 0 with no output, so the normal permission flow
applies. Wired in .claude/settings.json with `onFailure: "block"`, and any
unexpected error here also blocks, so a broken guard never lets a push through.
The GitHub ruleset on `main` is the second line of defence.
"""

import json
import re
import shlex
import subprocess
import sys

PROTECTED = "main"
SEPARATORS = {";", "&&", "||", "|", "&", "\n", "(", ")", "|&"}
SHELLS = {"sh", "bash", "zsh", "dash"}
# Leading words that run the rest of the line as a command.
WRAPPERS = {"env", "command", "exec", "nohup", "time", "sudo", "xargs"}


class Blocked(Exception):
    pass


def segments(command):
    lexer = shlex.shlex(command, posix=True, punctuation_chars=";&|()\n")
    lexer.whitespace = " \t\r"
    lexer.whitespace_split = True
    current = []
    for token in lexer:
        if token in SEPARATORS or set(token) <= set(";&|()\n"):
            if current:
                yield current
            current = []
        else:
            current.append(token)
    if current:
        yield current


def strip_prefix(words):
    """Drop env assignments and wrapper commands in front of the real one."""
    while words:
        head = words[0].rsplit("/", 1)[-1]
        if "=" in words[0] and not words[0].startswith("-"):
            words = words[1:]
        elif head in WRAPPERS:
            words = words[1:]
            while words and words[0].startswith("-"):
                words = words[1:]
        else:
            break
    return words


def run(args, cwd):
    result = subprocess.run(
        args, cwd=cwd, capture_output=True, text=True, timeout=20, check=False
    )
    return result.returncode, result.stdout.strip()


def ref_name(ref):
    ref = ref.lstrip("+")
    for prefix in ("refs/heads/", "heads/"):
        if ref.startswith(prefix):
            return ref[len(prefix) :]
    return ref


def check_git(words, cwd):
    # Skip git's global options; `-C dir` changes where branch lookups happen.
    i = 1
    while i < len(words) and words[i].startswith("-"):
        if words[i] in ("-C", "-c", "--git-dir", "--work-tree", "--namespace"):
            if words[i] == "-C" and i + 1 < len(words):
                cwd = words[i + 1]
            i += 2
        else:
            i += 1
    if i >= len(words) or words[i] != "push":
        return
    args = words[i + 1 :]

    positional = []
    j = 0
    while j < len(args):
        arg = args[j]
        if arg in ("--all", "--mirror", "--branches"):
            raise Blocked(f"`git push {arg}` could update '{PROTECTED}'.")
        if arg in ("-o", "--push-option", "--repo", "--receive-pack", "--exec"):
            j += 2
            continue
        if arg == "--":
            positional.extend(args[j + 1 :])
            break
        if not arg.startswith("-"):
            positional.append(arg)
        j += 1

    refspecs = positional[1:]
    _, branch = run(["git", "symbolic-ref", "--quiet", "--short", "HEAD"], cwd)
    if not refspecs:
        _, upstream = run(
            ["git", "rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"], cwd
        )
        if branch == PROTECTED or upstream.split("/", 1)[-1] == PROTECTED:
            raise Blocked(
                f"bare `git push` from '{branch}' (upstream '{upstream}') "
                f"would update '{PROTECTED}'. Name the branch explicitly."
            )
        return
    for spec in refspecs:
        src, _, dst = spec.partition(":")
        target = ref_name(dst if dst else src)
        if target in ("HEAD", "@"):
            target = branch
        if target == PROTECTED:
            raise Blocked(f"`git push … {spec}` would update '{PROTECTED}'.")


def check_gh(words, cwd):
    if len(words) >= 2 and words[1] == "api":
        endpoint = " ".join(words[2:])
        if "/merge" in endpoint:
            raise Blocked("merging through `gh api` is not allowed; use `gh pr merge`.")
        return
    if len(words) < 3 or words[1] != "pr" or words[2] != "merge":
        return
    args = words[3:]
    selector = []
    repo = []
    j = 0
    while j < len(args):
        arg = args[j]
        if arg in ("-R", "--repo") and j + 1 < len(args):
            repo = ["--repo", args[j + 1]]
            j += 2
            continue
        if arg.startswith("--repo="):
            repo = ["--repo", arg.split("=", 1)[1]]
        elif arg in ("-b", "--body", "-F", "--body-file", "-t", "--subject",
                     "-A", "--author-email", "--match-head-commit"):
            j += 2
            continue
        elif not arg.startswith("-") and not selector:
            selector = [arg]
        j += 1
    code, base = run(
        ["gh", "pr", "view", *selector, *repo, "--json", "baseRefName",
         "--jq", ".baseRefName"],
        cwd,
    )
    if code != 0 or not base:
        raise Blocked("could not determine the PR's base branch; refusing to merge.")
    if base == PROTECTED:
        raise Blocked(f"this PR targets '{PROTECTED}'; only the maintainer merges into it.")


def check(command, cwd, depth=0):
    for words in segments(command):
        words = strip_prefix(words)
        if not words:
            continue
        head = words[0].rsplit("/", 1)[-1]
        if head in SHELLS and "-c" in words and depth < 3:
            index = words.index("-c")
            if index + 1 < len(words):
                check(words[index + 1], cwd, depth + 1)
        elif head == "eval" and depth < 3:
            check(" ".join(words[1:]), cwd, depth + 1)
        else:
            # Any wrapper (xvfb-run, timeout, nice, …) may run git or gh
            # further along, so check from every occurrence.
            for index, word in enumerate(words):
                name = word.rsplit("/", 1)[-1]
                if name == "git":
                    check_git(words[index:], cwd)
                elif name == "gh":
                    check_gh(words[index:], cwd)


def main():
    data = json.load(sys.stdin)
    command = data.get("tool_input", {}).get("command", "")
    cwd = data.get("cwd") or "."
    try:
        try:
            check(command, cwd)
        except ValueError:
            # Unbalanced quotes (e.g. an apostrophe in a heredoc): shlex gives
            # up, so block only what could be a push or merge.
            if re.search(r"\bgit\b.*\bpush\b|\bgh\b.*\b(merge|api)\b", command, re.S):
                raise Blocked("could not parse this command; split out the push/merge.")
    except Blocked as reason:
        print(f"Blocked by .claude/hooks/guard-main.py: {reason}", file=sys.stderr)
        sys.exit(2)


if __name__ == "__main__":
    try:
        main()
    except SystemExit:
        raise
    except Exception as error:  # Fail closed: a broken guard blocks.
        print(f"guard-main.py failed ({error!r}); blocking to be safe.", file=sys.stderr)
        sys.exit(2)

#!/bin/sh
# body.sh: a `[body]` hook for `nv publish`. The change is on stdin and the summary line in
# NV_SUBJECT; the body goes to stdout. One `claude -p` call with no tools and a system prompt of
# a line, so that it costs what the change is and little more; what each call used is appended
# to $NV_BODY_LOG, if set, as "input output cache-read cache-made" tokens.
set -eu
prompt=$(printf 'Summary line of the commit: %s\n\nWrite the body of its message from the change below: what it does and, where it is not obvious from the diff, why or how it behaves (edge cases, choices made). Two to five short lines, wrapped at 72 columns. No summary line, no headings, no markdown, nothing that is not in the change.\n\n' "${NV_SUBJECT:-}")
answer=$( { printf '%s' "$prompt"; cat; } | claude -p --output-format json --tools "" --system-prompt "You write the bodies of git commit messages." )
printf '%s' "$answer" | python3 -c '
import json, os, sys
d = json.load(sys.stdin)
u = d.get("usage", {})
log = os.environ.get("NV_BODY_LOG")
if log:
    with open(log, "a") as f:
        keys = ("input_tokens", "output_tokens", "cache_read_input_tokens", "cache_creation_input_tokens")
        f.write(" ".join(str(u.get(k, 0)) for k in keys) + "\n")
print(d.get("result", "").strip())
'

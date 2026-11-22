#!/usr/bin/env python3
"""Print a counterexample of Sync.tla as a story: for each step, the action and what it changed,
with line values shown by content (`i` for what everyone started with, `a1` for the first line
`a` wrote) and a `*` where the content is that of an older write.

  idealis tlc -difftrace -config Small.cfg Sync.tla | python3 trace.py
"""
import re
import sys

text = sys.stdin.read()
print(text.splitlines()[0])
steps = re.split(r"\n(?=State \d+: )", text)

def short(s):
    # [c |-> <<"a", 1>>, n |-> <<"a", 3>>] -> a1*   (content a1, written by write a3)
    def value(m):
        c, n = m.group(1), m.group(2)
        cs = "".join(re.findall(r'"?(\w+)"?', c)).replace("i0", "i")
        ns = "".join(re.findall(r'"?(\w+)"?', n)).replace("i0", "i")
        return cs if cs == ns else cs + "*"
    s = re.sub(r"\[c \|-> (<<[^>]*>>), n \|-> (<<[^>]*>>)\]", value, s)
    s = s.replace('<<"none">>', "-")
    return s

keep = ("hist", "seen", "chain", "disk", "unsettled", "waiting", "standing", "kept")
for step in steps[1:]:
    head, *rest = step.split("\n")
    print(head)
    for line in rest:
        m = re.match(r"/\\ (\w+) = (.*)", line)
        if m and m.group(1) in keep:
            body = short(m.group(2))
            if m.group(1) == "waiting":
                body = re.sub(r"<<<<.*?>>, (\d+)>>", r"push on \1", body)
            print(f"    {m.group(1)} = {body[:400]}")
print(text.splitlines()[-1])

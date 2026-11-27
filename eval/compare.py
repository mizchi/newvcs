#!/usr/bin/env python3
"""compare.py: the medians of a results TSV, one column for each group of rows.

    eval/compare.py results.tsv [--by pattern,label] [--within scenario] [--rows field,field,...]

For the TSVs `agent-bench/run.py` and `history-bench/bench.py` write. Rows are grouped by the
fields given (by default every text field but the run's number), and each numeric field is the
median over the group, with the number of rows in the group under it. The first group is the one
the others are compared with, in per cent. With --within, a table for each value of that field
(each scenario, say), the groups compared within it.
"""
import argparse
import csv
import statistics
import sys


def number(v):
    try:
        return float(v)
    except ValueError:
        return None


def main():
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    p.add_argument("tsv")
    p.add_argument("--by", default=None)
    p.add_argument("--rows", default=None)
    p.add_argument("--within", default=None)
    p.add_argument("--spread", action="store_true", help="the quartiles under each median: what a difference has to be larger than")
    a = p.parse_args()
    rows = list(csv.DictReader(open(a.tsv), delimiter="\t"))
    if not rows:
        sys.exit("no rows")
    fields = list(rows[0])
    numeric = [f for f in fields if all(number(r[f]) is not None for r in rows) and f != "rep"]
    by = a.by.split(",") if a.by else [f for f in fields if f not in numeric and f != "rep"]
    show = a.rows.split(",") if a.rows else [f for f in numeric if f not in by]
    if a.within:
        by = [f for f in by if f != a.within]
        for value in dict.fromkeys(r[a.within] for r in rows):
            print(f"\n{a.within} {value}")
            table([r for r in rows if r[a.within] == value], by, show, a.spread)
    else:
        table(rows, by, show, a.spread)


def table(rows, by, show, spread=False):
    groups = {}
    for r in rows:
        groups.setdefault(tuple(r[f] for f in by), []).append(r)
    keys = list(groups)
    width = max(12, *(len(" ".join(k)) for k in keys))
    print(f"{'':<22}" + "".join(f"{' '.join(k):>{width + 2}}" for k in keys))
    print(f"{'runs':<22}" + "".join(f"{len(groups[k]):>{width + 2}}" for k in keys))
    for f in show:
        medians = [statistics.median(number(r[f]) for r in groups[k]) for k in keys]
        cells = []
        for i, m in enumerate(medians):
            cell = f"{m:.4g}"
            if i and medians[0]:
                cell += f" ({(m - medians[0]) / medians[0] * 100:+.0f}%)"
            cells.append(f"{cell:>{width + 2}}")
        print(f"{f:<22}" + "".join(cells))
        if spread:
            quartiles = []
            for k in keys:
                values = [number(r[f]) for r in groups[k]]
                q = statistics.quantiles(values, n=4) if len(values) > 1 else [values[0]] * 3
                quartiles.append(f"{f'{q[0]:.4g}..{q[2]:.4g}':>{width + 2}}")
            print(f"{'':<22}" + "".join(quartiles))

if __name__ == "__main__":
    main()

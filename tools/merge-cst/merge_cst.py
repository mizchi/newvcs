#!/usr/bin/env python3
"""merge_cst.py: a Git merge driver that merges by the syntax tree, for Python and Markdown.

    git config merge.cst.driver 'python3 tools/merge-cst/merge_cst.py %O %A %B %P %X %Y %L'
    echo '*.py merge=cst' >> .gitattributes        # nv git config ... for an nv workspace

Each version is parsed with tree-sitter and cut into units: a Python module into its top-level
statements, a Markdown document into its sections, and a section into its heading, blocks and
the sections under it, a list into its items. A unit is known by a key (the name a definition
or a class defines, a heading's text, an item's or a statement's own text), and the three
versions are merged unit by unit:

- what one side changed and the other did not is taken from the side that changed it;
- what both removed, or one removed and the other left as it was, is removed;
- what both inserted at one place is kept, ours first, theirs after;
- a unit both changed is merged within, where it is of a kind that has units (a section, a
  list); otherwise it is a conflict.

Where anything is a conflict, or a version does not parse, or is of a language it does not
know, it does what Git would do without it: `git merge-file` (both versions between markers,
named %X and %Y), exit 1. Where Git is not there either, it leaves %A as it was and exits 1, for
the caller's own merge (`nv sync` merges as text where a driver leaves no markers).

It assumes what it merges is checked afterwards: two units inserted at one place, or one side's
change to a unit the other moved, can be wrong together though each was right. Where the tests
run on what a sync made (`auto` in nv.toml) and a commit is made only of a state they pass on,
what this driver gets wrong is caught there.
"""
import os
import re
import shutil
import subprocess
import sys

EXIT_CLEAN, EXIT_CONFLICT = 0, 1


class Conflict(Exception):
    pass


# Lean, with no grammar: a file is a list of commands, each beginning at the start of a line
# with one of these words (or a doc comment or an attribute that goes with the next), and known
# by its kind and the name it declares.
LEAN_COMMANDS = (
    "theorem", "lemma", "def", "abbrev", "instance", "structure", "inductive", "class", "example",
    "axiom", "opaque", "namespace", "section", "end", "open", "variable", "universe", "set_option",
    "import", "noncomputable", "private", "protected", "partial", "unsafe", "macro", "syntax",
    "notation", "infix", "infixl", "infixr", "prefix", "postfix", "attribute", "deriving", "mutual",
    "termination_by", "#eval", "#check", "#print",
)
LEAN_MODIFIERS = {"noncomputable", "private", "protected", "partial", "unsafe"}
LEAN_NAMED = {"theorem", "lemma", "def", "abbrev", "instance", "structure", "inductive", "class", "axiom", "opaque"}


class LeanParser:
    """Stands for a tree-sitter parser where there is no grammar: `units` is all it is used for."""


def lean_starts(line):
    if line.startswith(("/--", "@[")):
        return True
    word = line.split(None, 1)[0] if line.strip() else ""
    return word in LEAN_COMMANDS


def lean_units(src):
    text = src.decode("utf-8", "surrogateescape")
    lines = text.splitlines(keepends=True)
    out, current, pending = [], [], []
    # A doc comment or attribute, and blank lines, wait for the command they go with.

    def flush():
        if current:
            body = "".join(current)
            out.append(body)
            current.clear()

    in_doc = False
    for line in lines:
        if in_doc:
            pending.append(line)
            if "-/" in line:
                in_doc = False
            continue
        if line.startswith("/--") or line.startswith("@["):
            flush()
            pending.append(line)
            in_doc = line.startswith("/--") and "-/" not in line
            continue
        if not line.strip() or line.startswith("--"):
            # Blank lines and comments go with what is before them, or wait with a doc comment.
            (pending if pending else current).append(line)
            continue
        if lean_starts(line):
            flush()
            current.extend(pending)
            pending.clear()
            current.append(line)
        else:
            current.extend(pending)
            pending.clear()
            current.append(line)
    current.extend(pending)
    flush()
    units, seen = [], {}
    for body in out:
        key = lean_key(body)
        seen[key] = seen.get(key, 0) + 1
        if seen[key] > 1:
            key = f"{key}#{seen[key]}"
        units.append(Unit(key, body, "lean-command"))
    return units


def lean_key(body):
    for line in body.splitlines():
        if not line.strip() or line.startswith(("--", "/--", "@[", " ", "\t")) or line.rstrip().endswith("-/"):
            continue
        words = line.split()
        while words and words[0] in LEAN_MODIFIERS:
            words = words[1:]
        if len(words) >= 2 and words[0] in LEAN_NAMED:
            return f"{words[0]}:{words[1]}"
        return "text:" + body.strip()
    return "text:" + body.strip()


def parser_for(path):
    if os.path.splitext(path)[1].lower() == ".lean":
        return LeanParser(), "lean"
    try:
        from tree_sitter import Language, Parser
    except ImportError:
        return None, None
    ext = os.path.splitext(path)[1].lower()
    try:
        if ext == ".py":
            import tree_sitter_python as grammar

            return Parser(Language(grammar.language())), "python"
        if ext in (".md", ".markdown"):
            import tree_sitter_markdown as grammar

            return Parser(Language(grammar.language())), "markdown"
    except ImportError:
        return None, None
    return None, None


class Unit:
    """A span of the source, the gap before it included, with a key and, where it has them,
    units of its own."""

    def __init__(self, key, text, kind, parts=None, head="", tail=""):
        self.key, self.text, self.kind, self.parts, self.head, self.tail = key, text, kind, parts, head, tail

    def render(self):
        if self.parts is None:
            return self.text
        return self.head + "".join(p.render() for p in self.parts) + self.tail


def node_text(src, node):
    return src[node.start_byte : node.end_byte].decode("utf-8", "surrogateescape")


def python_key(src, node):
    kind = node.type
    inner = node
    if kind == "decorated_definition":
        inner = node.child_by_field_name("definition") or node
    if inner.type in ("function_definition", "class_definition"):
        name = inner.child_by_field_name("name")
        return f"{inner.type}:{node_text(src, name)}"
    if kind == "expression_statement" and node.named_child_count == 1 and node.named_children[0].type == "assignment":
        left = node.named_children[0].child_by_field_name("left")
        if left is not None and left.type == "identifier":
            return f"assign:{node_text(src, left)}"
    return f"{kind}:{node_text(src, node).strip()}"


def markdown_key(src, node):
    if node.type == "section":
        heading = next((c for c in node.children if c.type in ("atx_heading", "setext_heading")), None)
        return "section:" + (node_text(src, heading).strip() if heading else "")
    if node.type == "list":
        return "list"
    return f"{node.type}:{node_text(src, node).strip()}"


# Units that are one of many alike, which two sides may each replace with their own.
COLLECTION_ITEMS = {"list_item"}

# Which Markdown nodes are merged within: their children are units.
MARKDOWN_WITHIN = {"document", "section", "list"}


def units(src, node, language, start, end):
    """The children of `node` as units, each with the gap before it, between `start` and `end`."""
    out, at, seen = [], start, {}
    key_of = python_key if language == "python" else markdown_key
    for child in node.children:
        if child.end_byte <= at:
            continue
        key = key_of(src, child)
        # Keys are unique among siblings: a second of the same is known by its place among them.
        seen[key] = seen.get(key, 0) + 1
        if seen[key] > 1:
            key = f"{key}#{seen[key]}"
        text_start = at
        child_end = child.end_byte
        if language == "markdown" and child.type in MARKDOWN_WITHIN:
            parts = units(src, child, language, child.start_byte, child.end_byte)
            head = src[text_start : child.start_byte].decode("utf-8", "surrogateescape")
            unit = Unit(key, src[text_start : child.end_byte].decode("utf-8", "surrogateescape"), child.type, parts, head=head)
        else:
            end_at = child.end_byte
            if child.type in COLLECTION_ITEMS:
                # Blank lines after an item are the list's, not the item's: an item put in after
                # it would bring them along twice.
                while end_at - 2 >= text_start and src[end_at - 1 : end_at] == b"\n" and src[end_at - 2 : end_at - 1] == b"\n":
                    end_at -= 1
            unit = Unit(key, src[text_start:end_at].decode("utf-8", "surrogateescape"), child.type)
            child_end = end_at
        out.append(unit)
        at = child_end if not (language == "markdown" and child.type in MARKDOWN_WITHIN) else child.end_byte
    if at < end:
        out.append(Unit("<tail>", src[at:end].decode("utf-8", "surrogateescape"), "<tail>"))
    return out


# What a list item reads as when it holds a place for items still to come.
PLACEHOLDERS = {"tbd", "tba", "todo", "none", "none yet", "n/a", "na", "...", "\u2026", "-", "wip", "nothing yet", "coming soon", "empty"}


def placeholder(unit):
    """Whether a list item only holds a place: `(none yet)`, `TBD`, `n/a`, `...`, or one word or a
    few wholly in brackets or emphasis. Two sides may each put their own items in place of one; an
    item that says something is changed by whoever replaces it (fourth pass: a likeness of texts
    was slow, and wrong below its threshold; fifth pass: `TODO: fix the auth bug` and
    `(urgent) rotate the keys (today)` were taken for placeholders, and an edit was lost)."""
    text = unit.render().strip()
    if "\n" in text:
        return False
    text = re.sub(r"^([-*+]|[0-9]+[.)])\s+(\[[ xX]\]\s+)?", "", text).strip()
    # Brackets and emphasis around it, one pair in another as `*(empty)*`, are taken off: what
    # is left has to be one of the words that hold a place, not a few words of anything.
    for _ in range(3):
        for left, right in (("**", "**"), ("__", "__"), ("(", ")"), ("[", "]"), ("<", ">"), ("*", "*"), ("_", "_")):
            inner = text[len(left) : len(text) - len(right)]
            if len(text) > len(left) + len(right) and text.startswith(left) and text.endswith(right) and left not in inner and right not in inner:
                text = inner.strip()
                break
        else:
            break
    # `...` is one of them as it is: its dots are not a trailing stop (sixth pass).
    return text.lower() in PLACEHOLDERS or text.lower().rstrip(".:!") in PLACEHOLDERS


def merge_units(base, ours, theirs):
    """Three-way merge of sibling units, by key."""
    b = {u.key: u for u in base}
    o = {u.key: u for u in ours}
    t = {u.key: u for u in theirs}

    def same(x, y):
        return x.render() == y.render()

    def merged(key):
        x, y, z = b.get(key), o.get(key), t.get(key)
        if x is not None:
            if y is None and z is None:
                return None
            if y is None:
                if same(z, x):
                    return None
                raise Conflict(f"ours removed {key}, theirs changed it")
            if z is None:
                if same(y, x):
                    return None
                raise Conflict(f"theirs removed {key}, ours changed it")
            if same(y, x):
                return z
            if same(z, x) or same(y, z):
                return y
            return within(x, y, z)
        # New on one side or both.
        if y is not None and z is not None:
            return y if same(y, z) else within(None, y, z)
        return y if y is not None else z

    def within(x, y, z):
        if y.parts is None or z.parts is None or y.kind != z.kind or (x is not None and x.parts is None):
            raise Conflict(f"both changed {y.key}")
        parts = merge_units(x.parts if x is not None else [], y.parts, z.parts)
        return Unit(y.key, None, y.kind, parts, head=y.head, tail=y.tail)

    # A unit known by its own text that both sides took out and put something else in the place
    # of is a unit both changed, unless it is one item of many (a list's): a conflict.
    base_keys = [u.key for u in base]

    def anchors(side):
        # For each unit of a side, the last unit of the base before it there: one pass, not one
        # look back per unit (thirteenth round: 16,000 units put in at one place took 16 s).
        out, last = [], None
        for u in side:
            out.append(last)
            if u.key in b:
                last = u.key
        return out

    ours_anchors, theirs_anchors = anchors(ours), anchors(theirs)

    def replaced(side, keys, side_anchors):
        put = {}
        for i, u in enumerate(side):
            if u.key not in b:
                put.setdefault(side_anchors[i], []).append(u.render())
        out, before = {}, None
        for k in base_keys:
            if k in keys:
                before = k
                continue
            if before in put:
                out[k] = put[before]
        return out

    by_ours, by_theirs = replaced(ours, o, ours_anchors), replaced(theirs, t, theirs_anchors)
    for k, unit in b.items():
        # Blank space between units goes with the unit after it, and moves as they are put in.
        if k in o or k in t or not unit.render().strip():
            continue
        # Taken out on both sides, it is known by its text: one side that put something else in
        # its place changed that text, and the other took it out or changed it otherwise (third
        # pass: a list item, a heading or a function's name that one side edited came back when
        # the other had removed it, and two edits of one list item were both kept).
        put_ours, put_theirs = by_ours.get(k), by_theirs.get(k)
        if put_ours == put_theirs:
            continue
        # A list item that only holds a place may be put in place of by both, each with its own.
        if unit.kind in COLLECTION_ITEMS and placeholder(unit):
            continue
        raise Conflict(f"both replaced {k}, or one removed what the other changed")

    # Every unit in the order of the base: a unit of the base where it was, one put in after the
    # unit of the base it follows on its side (its anchor), ours before theirs where both put
    # something after the same one, each side's in its own order. A unit one side moved goes back
    # to where the base had it.
    place = {k: i for i, k in enumerate(base_keys)}

    def after_anchor(side_anchors, i, rank):
        a = side_anchors[i]
        return (place[a] + 1 if a is not None else 0, rank, i)

    # A unit ours removed is gone only if theirs left it as it was: one theirs changed is a conflict
    # (thirteenth round: a list ours removed went, with the items theirs put in it).
    for k in base_keys:
        if k not in o:
            merged(k)
    placed = []
    for i, u in enumerate(ours):
        m = merged(u.key)
        if m is not None:
            placed.append(((place[u.key], 2, i) if u.key in b else after_anchor(ours_anchors, i, 0), m))
    for i, u in enumerate(theirs):
        if u.key in o or u.key in b:
            # Theirs of a unit ours has too, or one ours removed: merged above, or removed.
            continue
        m = merged(u.key)
        if m is not None:
            placed.append((after_anchor(theirs_anchors, i, 1), m))
    # What follows the last unit stays last.
    placed.sort(key=lambda p: (p[1].key == "<tail>", p[0]))
    return [m for _, m in placed]


def plain_key(key):
    """A unit's key without the place among alike siblings that `units` adds to a second one."""
    head, sep, tail = key.rpartition("#")
    return head if sep and tail.isdigit() else key


def keys_of(us):
    """The keys and texts of units and of the units within them, in order, blank space and tails
    left out. The text, not only the key: a unit can run into the next and still be read as one of
    the same name (second pass: `a = 1` and `@d` became `a = 1@d`, and `f` lost its decorator)."""
    for u in us:
        if u.key == "<tail>" or not u.render().strip():
            continue
        yield plain_key(u.key), u.render().strip() if u.parts is None else None
        if u.parts:
            yield from keys_of(u.parts)


def runs_together(us):
    """Whether a unit begins where the one before ended, with no line end between them."""
    texts = [u.render() for u in us]
    return any(a and b and not a.endswith("\n") and not b.startswith(("\n", "\r\n")) for a, b in zip(texts, texts[1:]))


def merge_source(parser, language, base, ours, theirs):
    # Where one side changed nothing, the other side is the merge, as Git has it (thirteenth round:
    # a move one side made was undone where the other had not changed the file).
    if ours == base or ours == theirs:
        return theirs
    if theirs == base:
        return ours
    if language == "lean":
        parts = [lean_units(v) for v in (base, ours, theirs)]
        merged = merge_units(*parts)
        # With no parser to read it back, a merge is clean only where no unit runs into the next.
        if runs_together(merged):
            raise Conflict("a unit of the merge runs into the next")
        return "".join(u.render() for u in merged).encode("utf-8", "surrogateescape")
    trees = [parser.parse(v) for v in (base, ours, theirs)]
    if any(tree.root_node.has_error for tree in trees):
        raise Conflict("a version does not parse")
    parts = [units(v, tree.root_node, language, 0, len(v)) for v, tree in zip((base, ours, theirs), trees)]
    merged = merge_units(*parts)
    result = "".join(u.render() for u in merged).encode("utf-8", "surrogateescape")
    # What was merged is what is written only if it reads back as those units, each whole: a unit
    # put after one that had no line ending of its own runs into it (thirteenth round: `# note`
    # and `b = 2` became one comment, exit 0). Otherwise it is not this driver's to decide.
    tree = parser.parse(result)
    if tree.root_node.has_error:
        raise Conflict("the merge does not parse")
    if list(keys_of(units(result, tree.root_node, language, 0, len(result)))) != list(keys_of(merged)):
        raise Conflict("the merge does not read back as the units merged")
    return result


def fall_back(a, o, b, labels, size):
    git = shutil.which("git")
    if git is None:
        return EXIT_CONFLICT
    with open(a, "rb") as f:
        ours = f.read()
    out = subprocess.run(
        [git, "merge-file", "-p", f"--marker-size={size}", "-L", labels[0], "-L", "base", "-L", labels[1], a, o, b],
        capture_output=True,
    )
    if out.returncode < 0 or out.returncode > 127:
        return EXIT_CONFLICT
    with open(a, "wb") as f:
        f.write(out.stdout if out.stdout or not ours else ours)
    return EXIT_CLEAN if out.returncode == 0 else EXIT_CONFLICT


def main(argv):
    if len(argv) < 4:
        print(__doc__.split("\n")[0], file=sys.stderr)
        return 2
    o, a, b, path = argv[:4]
    labels = (argv[4] if len(argv) > 4 else "ours", argv[5] if len(argv) > 5 else "theirs")
    size = argv[6] if len(argv) > 6 else "7"
    parser, language = parser_for(path)
    if parser is not None:
        try:
            versions = []
            for f in (o, a, b):
                with open(f, "rb") as h:
                    versions.append(h.read())
            result = merge_source(parser, language, *versions)
            with open(a, "wb") as h:
                h.write(result)
            return EXIT_CLEAN
        except Conflict as why:
            if os.environ.get("MERGE_CST_VERBOSE"):
                print(f"merge_cst: {path}: {why}; merging as text", file=sys.stderr)
    return fall_back(a, o, b, labels, size)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

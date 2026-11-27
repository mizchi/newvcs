from .errors import TemplateError



MISSING = object()
FILTERS = {"upper": str.upper, "lower": str.lower, "title": str.title, "strip": str.strip}


def _lookup(context, name):
    value = context
    for step in name.split("."):
        if isinstance(value, dict):
            value = value.get(step, MISSING)
        else:
            value = getattr(value, step, MISSING)
        if value is MISSING:
            return MISSING
    return value


def _field(text, context):
    name, *filters = text.split("|")
    name = name.strip()
    if not name:
        raise TemplateError("empty name")
    value = _lookup(context, name)
    for f in filters:
        if f.startswith("default:"):
            if value is MISSING or value is None:
                value = f[len("default:"):]
            continue
        if f not in FILTERS:
            raise TemplateError(f"unknown filter {f!r}")
        if value is MISSING:
            raise TemplateError(f"missing {name!r}")
        value = FILTERS[f](str(value))
    if value is MISSING:
        raise TemplateError(f"missing {name!r}")
    return str(value)


def render(template, context):
    out, i = [], 0
    while i < len(template):
        c = template[i]
        if template.startswith("{{", i):
            out.append("{")
            i += 2
        elif template.startswith("}}", i):
            out.append("}")
            i += 2
        elif c == "{":
            end = template.find("}", i)
            if end == -1:
                raise TemplateError("unclosed {")
            out.append(_field(template[i + 1:end], context))
            i = end + 1
        elif c == "}":
            raise TemplateError("lone }")
        else:
            out.append(c)
            i += 1
    return "".join(out)

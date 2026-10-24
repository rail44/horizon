"""Minimal reader for SCIP indexes (binary protobuf, or the JSON of `scip print --json`).

Only the fields the database stores are decoded. Field numbers and enum
values follow Sourcegraph's scip.proto (Apache-2.0,
https://github.com/sourcegraph/scip/blob/e01e97efac2f6b8c266b4d04825f1f1eab7b8f6c/scip.proto):

  Index:             documents = 2, external_symbols = 3
  Document:          relative_path = 1, occurrences = 2, symbols = 3
  Occurrence:        range = 1 (packed int32, deprecated), symbol = 2,
                     symbol_roles = 3, single_line_range = 8, multi_line_range = 9
  SingleLineRange:   line = 1, start_character = 2, end_character = 3
  MultiLineRange:    start_line = 1, start_character = 2, end_line = 3, end_character = 4
  SymbolInformation: symbol = 1, kind = 5, display_name = 6
  SymbolRole:        Definition = 0x1, Import = 0x2

Unknown fields are skipped by wire type, so newer indexes still decode.
"""

import json

KINDS = {
    0: "UnspecifiedKind", 66: "AbstractMethod", 72: "Accessor", 1: "Array", 2: "Assertion",
    3: "AssociatedType", 4: "Attribute", 5: "Axiom", 6: "Boolean", 7: "Class", 86: "Concept",
    8: "Constant", 9: "Constructor", 62: "Contract", 10: "DataFamily", 73: "Delegate", 11: "Enum",
    12: "EnumMember", 63: "Error", 13: "Event", 84: "Extension", 14: "Fact", 15: "Field", 16: "File",
    17: "Function", 18: "Getter", 19: "Grammar", 20: "Instance", 21: "Interface", 22: "Key",
    23: "Lang", 24: "Lemma", 64: "Library", 25: "Macro", 26: "Method", 74: "MethodAlias",
    27: "MethodReceiver", 67: "MethodSpecification", 28: "Message", 85: "Mixin", 65: "Modifier",
    29: "Module", 30: "Namespace", 31: "Null", 32: "Number", 33: "Object", 34: "Operator",
    35: "Package", 36: "PackageObject", 37: "Parameter", 38: "ParameterLabel", 39: "Pattern",
    40: "Predicate", 41: "Property", 42: "Protocol", 68: "ProtocolMethod", 69: "PureVirtualMethod",
    43: "Quasiquoter", 44: "SelfParameter", 45: "Setter", 46: "Signature", 75: "SingletonClass",
    76: "SingletonMethod", 77: "StaticDataMember", 78: "StaticEvent", 79: "StaticField",
    80: "StaticMethod", 81: "StaticProperty", 82: "StaticVariable", 48: "String", 49: "Struct",
    47: "Subscript", 50: "Tactic", 51: "Theorem", 52: "ThisParameter", 53: "Trait",
    70: "TraitMethod", 54: "Type", 55: "TypeAlias", 56: "TypeClass", 71: "TypeClassMethod",
    57: "TypeFamily", 58: "TypeParameter", 59: "Union", 60: "Value", 61: "Variable",
}
DEFINITION, IMPORT = 0x1, 0x2


def varint(data, pos):
    result = shift = 0
    while True:
        byte = data[pos]
        pos += 1
        result |= (byte & 0x7F) << shift
        if byte < 0x80:
            return result, pos
        shift += 7


def fields(data):
    """Yield (field number, wire type, value) for one message; value is int or memoryview."""
    pos, end = 0, len(data)
    while pos < end:
        key, pos = varint(data, pos)
        number, wire = key >> 3, key & 7
        if wire == 0:
            value, pos = varint(data, pos)
        elif wire == 2:
            size, pos = varint(data, pos)
            value = data[pos : pos + size]
            pos += size
        elif wire == 1:
            value, pos = None, pos + 8
        elif wire == 5:
            value, pos = None, pos + 4
        else:
            raise ValueError(f"Unsupported protobuf wire type {wire}")
        yield number, wire, value


def int32(value):
    return value - (1 << 64) if value >= 1 << 63 else value


def packed(value, wire):
    if wire == 0:
        return [int32(value)]
    out, pos = [], 0
    while pos < len(value):
        item, pos = varint(value, pos)
        out.append(int32(item))
    return out


def text(value):
    return bytes(value).decode("utf-8", "replace")


def ints(message, names):
    values = dict.fromkeys(names, 0)
    for number, wire, value in fields(message):
        if 1 <= number <= len(names) and wire == 0:
            values[names[number - 1]] = int32(value)
    return [values[name] for name in names]


def symbol_information(message):
    symbol, kind, display = "", 0, ""
    for number, wire, value in fields(message):
        if number == 1 and wire == 2:
            symbol = text(value)
        elif number == 5 and wire == 0:
            kind = value
        elif number == 6 and wire == 2:
            display = text(value)
    return {"symbol": symbol, "kind": KINDS.get(kind, str(kind)), "display_name": display}


def normalize_range(values):
    if len(values) == 3:
        return values[0], values[1], values[0], values[2]
    if len(values) == 4:
        return tuple(values)
    raise ValueError(f"Invalid SCIP range {values}")


def occurrence(message):
    legacy, typed, symbol, roles = [], None, "", 0
    for number, wire, value in fields(message):
        if number == 1:
            legacy.extend(packed(value, wire))
        elif number == 2 and wire == 2:
            symbol = text(value)
        elif number == 3 and wire == 0:
            roles = int32(value)
        elif number == 8 and wire == 2:
            line, start, end = ints(value, ["line", "start", "end"])
            typed = (line, start, line, end)
        elif number == 9 and wire == 2:
            typed = tuple(ints(value, ["sl", "sc", "el", "ec"]))
    return {"symbol": symbol, "roles": roles, "range": typed or normalize_range(legacy)}


def read_binary(data):
    data = memoryview(data)
    documents, external = [], []
    for number, wire, value in fields(data):
        if number == 2 and wire == 2:
            path, occurrences, symbols = "", [], []
            for n, w, v in fields(value):
                if n == 1 and w == 2:
                    path = text(v)
                elif n == 2 and w == 2:
                    occurrences.append(occurrence(v))
                elif n == 3 and w == 2:
                    symbols.append(symbol_information(v))
            documents.append({"path": path, "occurrences": occurrences, "symbols": symbols})
        elif number == 3 and wire == 2:
            external.append(symbol_information(value))
    return documents, external


def json_kind(value):
    if isinstance(value, int):
        return KINDS.get(value, str(value))
    return value or "UnspecifiedKind"


def json_symbol(item):
    return {"symbol": item.get("symbol", ""), "kind": json_kind(item.get("kind", 0)),
            "display_name": item.get("displayName", "")}


def json_range(item):
    if "singleLineRange" in item:
        r = item["singleLineRange"]
        return r.get("line", 0), r.get("startCharacter", 0), r.get("line", 0), r.get("endCharacter", 0)
    if "multiLineRange" in item:
        r = item["multiLineRange"]
        return r.get("startLine", 0), r.get("startCharacter", 0), r.get("endLine", 0), r.get("endCharacter", 0)
    return normalize_range(item["range"])


def read_json(raw):
    """Read the protobuf JSON mapping (camelCase names), as printed by `scip print --json`."""
    index = json.loads(raw)
    documents = [
        {
            "path": doc["relativePath"],
            "occurrences": [
                {"symbol": o.get("symbol", ""), "roles": o.get("symbolRoles", 0), "range": json_range(o)}
                for o in doc.get("occurrences", [])
            ],
            "symbols": [json_symbol(s) for s in doc.get("symbols", [])],
        }
        for doc in index.get("documents", [])
    ]
    return documents, [json_symbol(s) for s in index.get("externalSymbols", [])]


def read(path):
    raw = path.read_bytes()
    return read_json(raw) if raw.lstrip()[:1] == b"{" else read_binary(raw)

#!/usr/bin/env python3
"""Append DuckDB's loadable-extension metadata footer to a shared library.

Equivalent to `scripts/append_extension_metadata.py` of duckdb/extension-ci-tools
(commit 2de802913caf5c9bba0ef267658b20f3cc0486c3, 2026-10-23), written anew
rather than vendored because that repository declares no license. The layout
it reproduces:

- a WebAssembly custom-section header (so a Wasm binary stays valid), 22 bytes;
- eight 32-byte NUL-padded ASCII fields, written in the order
  FIELD8..FIELD6 (unused), FIELD5 abi_type, FIELD4 extension_version,
  FIELD3 duckdb_version, FIELD2 platform, FIELD1 the magic "4";
- 256 zero bytes reserved for a signature.

Usage: append_metadata.py LIBRARY OUTPUT --name N --platform P
       --duckdb-version V --extension-version E [--abi-type T]
"""

import argparse
import os
import shutil


def custom_section_header():
    # 0 = custom section id; 0x93 0x04 = LEB128 531, the length of what follows
    # (1 + 16 name bytes + 2 + 8 * 32 + 256); 16 = name length; then the name
    # and 0x80 0x04 = LEB128 512, the payload length.
    return bytes([0, 0x93, 0x04, 16]) + b"duckdb_signature" + bytes([0x80, 0x04])


def field(value):
    data = value.encode("ascii")
    if len(data) > 32:
        raise ValueError(f"metadata field longer than 32 bytes: {value!r}")
    return data + b"\x00" * (32 - len(data))


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("library")
    parser.add_argument("output")
    parser.add_argument("--name", required=True)
    parser.add_argument("--platform", required=True)
    parser.add_argument("--duckdb-version", required=True)
    parser.add_argument("--extension-version", required=True)
    parser.add_argument("--abi-type", default="C_STRUCT_UNSTABLE")
    args = parser.parse_args()

    if os.path.basename(args.output) != f"{args.name}.duckdb_extension":
        raise SystemExit(f"output must be named {args.name}.duckdb_extension")
    tmp = args.output + ".tmp"
    shutil.copyfile(args.library, tmp)
    with open(tmp, "ab") as out:
        out.write(custom_section_header())
        for value in ["", "", "", args.abi_type, args.extension_version, args.duckdb_version, args.platform, "4"]:
            out.write(field(value))
        out.write(b"\x00" * 256)
    os.replace(tmp, args.output)


if __name__ == "__main__":
    main()

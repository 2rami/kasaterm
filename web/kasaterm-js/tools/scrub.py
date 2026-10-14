#!/usr/bin/env python3
"""Scrub a recording before it goes into the public repo, then refuse it if anything personal is
left.

    python3 tools/scrub.py corpus/x.kcast.gz [--replace OLD=NEW ...]

Replacements are byte-for-byte and must keep the length (and the display width), so every escape
sequence, column and wrap in the recording stays where it was — the differential test only needs
the stream to be self-consistent, not to be the original. The account name, host name, home
directory and e-mail of the machine that recorded are always replaced; afterwards the stream is
searched for those and for anything token-shaped, and the file is left untouched if a hit remains.
"""
import argparse
import base64
import getpass
import gzip
import json
import os
import re
import socket
import sys


def personal():
    user = getpass.getuser()
    host = socket.gethostname().split(".")[0]
    pairs = [(os.path.expanduser("~"), "/Users/" + "x" * (len(os.path.expanduser("~")) - 7))]
    # The host name ("<name>-MacBookPro") carries a person's name; keep its shape.
    pairs.append((host, re.sub(r"[A-Za-z0-9]", "x", host)))
    pairs.append((user, "x" * len(user)))
    return pairs


FORBIDDEN = [
    rb"sk-ant-[A-Za-z0-9_-]{8,}", rb"sk-[A-Za-z0-9]{20,}", rb"gh[pousr]_[A-Za-z0-9]{20,}",
    rb"xox[abpr]-[A-Za-z0-9-]{10,}", rb"AKIA[0-9A-Z]{16}", rb"eyJ[A-Za-z0-9_-]{20,}\.",
    rb"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}",
]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("path")
    ap.add_argument("--replace", action="append", default=[], help="OLD=NEW, same byte length")
    args = ap.parse_args()
    pairs = personal()
    for kv in args.replace:
        old, new = kv.split("=", 1)
        pairs.append((old, new))
    enc = [(o.encode(), n.encode()) for o, n in pairs]
    for o, n in enc:
        if len(o) != len(n):
            sys.exit(f"replacement changes length: {o!r} -> {n!r}")

    opener = gzip.open if args.path.endswith(".gz") else open
    with opener(args.path, "rt", encoding="utf-8") as f:
        lines = f.read().splitlines()
    header, events = json.loads(lines[0]), [json.loads(l) for l in lines[1:]]
    # Join the stream so a name split across two reads is still found, then cut it back.
    sizes = [len(base64.b64decode(e[2])) if e[1] == "o" else 0 for e in events]
    stream = b"".join(base64.b64decode(e[2]) for e in events if e[1] == "o")
    for o, n in sorted(enc, key=lambda p: -len(p[0])):
        if o == getpass.getuser().encode():
            stream = re.sub(rb"(?<![A-Za-z0-9])" + re.escape(o) + rb"(?![A-Za-z0-9])", n, stream)
        else:
            stream = stream.replace(o, n)
    # Session and request ids: not secrets, but they name one person's sessions.
    stream = re.sub(rb"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", b"00000000-0000-0000-0000-000000000000", stream)
    header = json.loads(json.dumps(header).replace(os.path.expanduser("~"), "~"))

    leftovers = [o for o, _ in enc if o in stream and o != getpass.getuser().encode()]
    leftovers += [m.group(0) for p in FORBIDDEN for m in re.finditer(p, stream)]
    user = getpass.getuser().encode()
    leftovers += [m.group(0) for m in re.finditer(rb"(?<![A-Za-z0-9])" + re.escape(user) + rb"(?![A-Za-z0-9])", stream)]
    if leftovers:
        sys.exit(f"{args.path}: personal data left, not written: {sorted(set(leftovers))[:10]}")

    pos = 0
    for e, size in zip(events, sizes):
        if e[1] == "o":
            e[2] = base64.b64encode(stream[pos:pos + size]).decode()
            pos += size
    with opener(args.path, "wt", encoding="utf-8") as f:
        f.write(json.dumps(header, ensure_ascii=False) + "\n")
        for e in events:
            f.write(json.dumps(e) + "\n")
    print(f"{args.path}: clean")


if __name__ == "__main__":
    main()

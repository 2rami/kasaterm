#!/usr/bin/env python3
"""Copy only the feedback receiver credentials into a private service file."""
import argparse
import os
from pathlib import Path
import shlex
import tempfile


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("source", type=Path)
    parser.add_argument("destination", type=Path)
    parser.add_argument("--token-key", default="KASATERM_FEEDBACK_DISCORD_TOKEN")
    parser.add_argument("--user-key", default="KASATERM_FEEDBACK_DISCORD_USER")
    args = parser.parse_args()
    if args.destination.exists():
        parser.error("destination already exists")
    wanted = {args.token_key, args.user_key}
    values = {}
    for line in args.source.read_text().splitlines():
        name, separator, value = line.strip().removeprefix("export ").partition("=")
        if separator and name.strip() in wanted:
            parts = shlex.split(value, comments=True)
            if len(parts) == 1:
                values[name.strip()] = parts[0]
    token, user = values.get(args.token_key, ""), values.get(args.user_key, "")
    if not token or not all(c.isascii() and (c.isalnum() or c in "._-") for c in token):
        parser.error("valid bot credential is missing")
    if not (16 <= len(user) <= 22 and user.isascii() and user.isdigit()):
        parser.error("valid recipient is missing")
    args.destination.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=".feedback-", dir=args.destination.parent)
    try:
        with os.fdopen(fd, "w") as output:
            output.write(f"KASATERM_FEEDBACK_DISCORD_TOKEN={token}\nKASATERM_FEEDBACK_DISCORD_USER={user}\n")
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, args.destination)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)
    print("Feedback service credentials configured with private permissions.")


if __name__ == "__main__":
    main()

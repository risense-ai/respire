"""Snapshot a reviewed regular ar archive of uniquely named native ELF objects."""
import hashlib
import os
import re
import stat
import struct
import sys

MAX_BYTES = 512 * 1024 * 1024
MAX_ENTRIES = 16384


def check(condition, message):
    if not condition:
        raise ValueError(message)


def inspect(archive, size, machine):
    check(archive.read(8) == b"!<arch>\n", "expected regular ar archive")
    names = set()
    symbols = False
    for index in range(MAX_ENTRIES):
        if archive.tell() == size:
            break
        header = archive.read(60)
        check(len(header) == 60 and header[-2:] == b"`\n", "invalid member header")
        name = header[:16].rstrip(b" ")
        length = header[48:58].rstrip(b" ")
        check(length.isdigit(), "invalid member length")
        length = int(length)
        start = archive.tell()
        check(start + length <= size, "truncated member")
        if name in (b"/", b"/SYM64/"):
            check(index == 0 and not symbols, "ambiguous symbol table")
            symbols = True
        else:
            # Accept direct, portable names only. Extended name tables need separate review.
            check(re.fullmatch(rb"[a-zA-Z0-9_][a-zA-Z0-9_.+-]*/", name)
                  and name not in names, "ambiguous or unsupported member name")
            names.add(name)
            check(length >= 64, "short ELF member")
            elf = archive.read(64)
            check(elf[:7] == b"\x7fELF\x02\x01\x01" and
                  struct.unpack_from("<HHI", elf, 16) == (1, machine, 1)
                  and struct.unpack_from("<H", elf, 52)[0] == 64,
                  "invalid or wrong-target ELF header")
        archive.seek(start + length)
        if length % 2:
            check(archive.read(1) == b"\n", "invalid member padding")
    check(archive.tell() == size and names, "too many entries or no objects")


def snapshot(source, destination, pin, arch):
    flags = os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW
    with os.fdopen(os.open(source, flags), "rb") as original:
        info = os.fstat(original.fileno())
        check(stat.S_ISREG(info.st_mode) and 8 < info.st_size <= MAX_BYTES,
              "invalid archive size or type")
        with open(destination, "x+b") as copy:
            remaining = info.st_size
            digest = hashlib.sha256()
            while remaining:
                block = original.read(min(remaining, 1024 * 1024))
                check(block, "archive shrank")
                copy.write(block)
                digest.update(block)
                remaining -= len(block)
            check(not original.read(1) and digest.hexdigest() == pin, "archive digest mismatch")
            copy.seek(0)
            inspect(copy, info.st_size, {"linux/amd64": 62, "linux/arm64": 183}[arch])
            os.fchmod(copy.fileno(), 0o400)


if __name__ == "__main__":
    try:
        snapshot(*sys.argv[1:])
    except (ValueError, OSError) as error:
        sys.exit(str(error))

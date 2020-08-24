#!/bin/python3

import contextlib
import os
import pathlib
import subprocess
import sys


fstype = "ext2"
image_size = 1000000


@contextlib.contextmanager
def mounted(path, dest=pathlib.Path("fs")):
    if not dest.is_dir():
        dest.mkdir()
    subprocess.check_call(["mount", "-t", fstype, "-o", "loop", path, str(dest)])
    oldcwd = os.getcwd()
    os.chdir(dest)
    try:
        yield
    finally:
        os.chdir(oldcwd)
        subprocess.run(["umount", str(dest)])


def prepare(out):
    # allocate
    with open(out, "wb") as f:
        f.write(b"\0" * image_size)
    # mkfs
    subprocess.check_call([f"mkfs.{fstype}", out])
    # write files
    with mounted(out):
        with open("a", "wb") as f:
            f.write(b"1" * 10000)


def changes(mountpoint):
    with mounted(mountpoint):
        with open("b", "wb") as f:
            f.write(b"2" * 20000)
            os.fsync(f)
        os.rename("b", "a")


def verify(mountpoint):
    with mounted(mountpoint):
        path = pathlib.Path("a")
        try:
            if not path.exists():
                print("BAD: does not exist")
                sys.exit(1)
            else:
                data = path.read_bytes()
                if data == b"":
                    print("BAD: empty file")
                    sys.exit(1)
                elif data == b"1" * 10000:
                    print("GOOD: old content")
                    sys.exit(11)
                elif data == b"2" * 20000:
                    print("GOOD: new content")
                    sys.exit(12)
                else:
                    print("BAD: unexpected content")
                    sys.exit(1)
        except (OSError, IOError) as ex:
            print(f"ERROR: {ex}")


if __name__ == "__main__":
    argv = sys.argv[1:]
    if argv:
        cmd = argv[0]
        if cmd == "prepare":
            prepare(*argv[1:])
        elif cmd == "changes":
            changes(*argv[1:])
        elif cmd == "verify":
            verify(*argv[1:])
        else:
            print(f"Unknown cmd: {cmd}")
            sys.exit(1)
    else:
        subprocess.run(["recordfs", "run-suite", "--sudo", sys.argv[0]])

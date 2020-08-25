#!/usr/bin/env python3

import contextlib
import os
import pathlib
import sqlite3
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
    # write db
    with mounted(out):
        db = sqlite3.connect("db", isolation_level="EXCLUSIVE")
        cur = db.cursor()
        cur.execute("PRAGMA journal_mode = MEMORY")
        cur.execute("CREATE TABLE rows (data TEXT)")
        cur.executemany("INSERT INTO rows(data) VALUES (?)", [("1111",)] * 20000)
        db.commit()
        db.close()


def changes(mountpoint):
    with mounted(mountpoint):
        db = sqlite3.connect("db", isolation_level="EXCLUSIVE")
        cur = db.cursor()
        cur.execute("PRAGMA journal_mode = MEMORY")
        cur.execute("DELETE FROM rows")
        cur.executemany("INSERT INTO rows(data) VALUES (?)", [("203",)] * 30000)
        db.commit()
        db.close()


def verify(mountpoint):
    with mounted(mountpoint):
        db = sqlite3.connect("db", isolation_level="EXCLUSIVE")
        try:
            cur = db.execute("SELECT data FROM rows")
            count = 0
            for row in cur:
                count += int(row[0])
            if count == 1111 * 20000:
                print("GOOD: old content")
                sys.exit(11)
            elif count == 203 * 30000:
                print("GOOD: new content")
                sys.exit(12)
            else:
                print("BAD: unexpected content")
                sys.exit(1)
        except Exception as ex:
            print(f"ERROR: {ex}")
        finally:
            db.close()


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
        subprocess.run(["outagefs", "run-suite", "--sudo", sys.argv[0]])

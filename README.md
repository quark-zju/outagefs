recordfs
========

recordfs is a fuse filesystem that exposes a single fixed-sized file.

Writes and syncs on that file are recorded, and can be replayed with arbitrary
sets of writes dropped.

This emulates power failure or hard reboot cases. It is useful to check certain
properties of a application running on a filesystem.


Example: "atomic rename" on ext4
--------------------------------

It's common to create a file and rename it to overwrite an existing file, and
expect the file to either have the new content, or the old content. How does
that work practically on ext4? Let's find out.

### Setup

First, prepare a base image of ext4 with a file `b` in it having some content:

```bash
# or, try `-s 1m` and see if it makes a difference
truncate -s 3m base
# or, try 'ext2'
mkfs.ext4 base
mkdir ext4root
sudo mount -o loop -t ext4 base ext4root
sudo sh -c 'seq 4000 > ext4root/b'
sudo umount ext4root
```

### Record

Then, use recordfs to record the write + rename operation:

```bash
# try adding 'sync' before 'mv' if 'ext2' is used
recordfs mount --record --sudo --exec 'mount -o loop -t ext4 $1 ext4root; seq 2 6000 > ext4root/a; mv ext4root/{a,b}; umount ext4root'
```

The above command uses `base` as the base image, mounts it as a single file with
recording turned on, and passes that single file as `$1` to the shell script. The
shell script mounts the file as ext4 and makes changes to the ext4 filesystem.
Writing to the mounted ext4 filesystem gets translated to low-level write and
sync operations to the `$1` file. The `--record` flag tells `recordfs` to write
the changes back to disk as `changes`.

Let's check that recordfs does record some changes:

```bash
recordfs show
```

### Verify

The property we want to verify is "b should have either new or old content".
Let's express that in a script and name it `verify.py`:

```python
import pathlib
path = pathlib.Path("./ext4root/b")

def seq(start, end):
    return b"".join([b"%d\n" % i for i in range(start, end + 1)])

try:
    if not path.exists():
        print("BAD: does not exist")
    else:
        data = path.read_bytes()
        if data == b"":
            print("BAD: empty file")
        elif data == seq(1, 4000):
            print("GOOD: old content")
        elif data == seq(2, 6000):
            print("GOOD: new content")
        else:
            print("BAD: unexpected content")
except Exception as ex:
    print(f"ERROR: {ex}")
```

Verify the end state is good:

```bash
recordfs mount --sudo --exec 'mount -o loop -t ext4 $1 ext4root && python3 verify.py; umount ext4root'
# should print 'GOOD: new content'
```

It's also good if all writes are discarded:

```bash
recordfs mount --filter 0 --sudo --exec 'mount -o loop -t ext4 $1 ext4root && python3 verify.py; umount ext4root'
# should print 'GOOD: old content'
```

### Generating Tests

More interesting tests will be when some writes are discarded while others
aren't.  In theory it's possible to look at `recordfs show` result and find out
what to discard, and figure out bits as a "filter" (`1`: take, `0` or not
mentioned: discard), and test it like:

```bash
recordfs mount --filter 1000000001000000011 --sudo --exec 'mount -o loop -t ext4 $1 ext4root && python3 verify.py; umount ext4root'
```

It is time consuming to figure out interesting test cases manually.
`recordfs` provides a subcommand to generate test cases:

```bash
recordfs gen-tests
```

This will print strings in the `offset:bits` form, suitable for `--filter`.
`gen-tests` respects `Sync` operations. If a `Sync` is not discarded, none of
the `Write`s before it would be discarded. It will also try to make the number
of test cases bounded so tests can complete.

Now, let's just use the generated tests and run the verify script on them:

```bash
for f in $(recordfs gen-tests); do
    recordfs mount --filter $f --sudo --exec 'mount -o loop -t ext4 $1 ext4root && python3 verify.py; umount ext4root'
done
```

Tips
----

### More Challenging Tests

The tests above might be not challenging enough. For example, individual writes
are atomic and Sync are expected to work as expected. Hardware might have
different properties. For example, having hardward-specific 2KB block size,
or does not always respect Sync, or might corrupt data during writes.
To make it easier to exercise such behaviors, `recordfs` has a `mutate`
sub-command to rewrite changes:

```bash
recordfs mutate --split-write --zero-fill --drop-sync
```

The `changes` file will be updated with the rewritten result.  Note that the
internal filesystem state can break more easily. It's likely to see some tests
erroring out at the `mount` command. It's also easier to trigger some errors
like `EUCLEAN` or hangs.


### Convenient Way to Run Tests

It is verbose and error-prone to setup, record, and run tests manually.
The `run-script` subcommand can be use to make it easier:

```bash
recordfs run-suite --sudo suite-examples/rename-no-fsync-ext2.py
```

The above command will create a temporary directory, call the script with
`prepare` to create the `base` image, then `changes` to make changes to record,
and eventually `verify` to verify test cases. After testing, the temporary
directory is deleted.

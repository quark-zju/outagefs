use crate::journal::Change;
use crate::vendor::fuse::FileAttr;
use crate::vendor::fuse::FileType;
use crate::vendor::fuse::Filesystem;
use crate::vendor::fuse::ReplyAttr;
use crate::vendor::fuse::ReplyData;
use crate::vendor::fuse::ReplyEmpty;
use crate::vendor::fuse::ReplyEntry;
use crate::vendor::fuse::ReplyStatfs;
use crate::vendor::fuse::ReplyWrite;
use crate::vendor::fuse::Request;
use std::ffi::OsStr;
use std::time::Duration;
use std::time::UNIX_EPOCH;

const BLOCK_SIZE: usize = 512;

/// Fuse state for "recordfs" - a single file filesystem recording write and
/// flush operations.
pub struct FuseRecordFilesystem<'a> {
    /// The filesystem is exposed as a single file. This is its content.
    data: Vec<u8>,

    /// Modifications to the filesystem.
    changes: &'a mut Vec<Change>,
}

impl<'a> FuseRecordFilesystem<'a> {
    fn block_count(&self) -> usize {
        (self.data.len() + BLOCK_SIZE - 1) / BLOCK_SIZE
    }

    fn attr(&self) -> FileAttr {
        FileAttr {
            ino: 1,
            size: self.data.len() as u64,
            blocks: self.block_count() as _,
            atime: UNIX_EPOCH,
            mtime: UNIX_EPOCH,
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
            kind: FileType::RegularFile,
            perm: 0o666,
            nlink: 1,
            uid: 0,
            gid: 0,
            rdev: 1,
            flags: 0,
        }
    }

    pub fn new(data: Vec<u8>, changes: &'a mut Vec<Change>) -> Self {
        Self { data, changes }
    }
}

impl<'a> Filesystem for FuseRecordFilesystem<'a> {
    fn getattr(&mut self, _req: &Request, _ino: u64, reply: ReplyAttr) {
        reply.attr(&Duration::from_secs(60), &self.attr());
    }

    fn lookup(&mut self, _req: &Request, _parent: u64, _name: &OsStr, reply: ReplyEntry) {
        reply.entry(&Duration::from_secs(60), &self.attr(), 0);
    }

    fn read(&mut self, _: &Request, _ino: u64, _fh: u64, offset: i64, size: u32, reply: ReplyData) {
        let offset = offset as usize;
        let size = size as usize;
        let end = (offset + size).min(self.data.len());
        reply.data(&self.data[offset..end]);
    }

    fn write(
        &mut self,
        _req: &Request,
        _ino: u64,
        _fh: u64,
        offset: i64,
        data: &[u8],
        _flags: u32,
        reply: ReplyWrite,
    ) {
        let offset = offset as usize;
        self.data[offset..offset + data.len()].copy_from_slice(data);
        self.changes.push(Change::Write {
            offset,
            data: data.to_vec(),
        });
        reply.written(data.len() as u32);
    }

    fn fsync(&mut self, _req: &Request, _ino: u64, _fh: u64, _datasync: bool, reply: ReplyEmpty) {
        if let Some(Change::Sync) = self.changes.last() {
            // No need to record Sync if the last change was Sync.
        } else {
            self.changes.push(Change::Sync);
        }
        reply.ok();
    }

    /// Get file system statistics.
    fn statfs(&mut self, _req: &Request, _ino: u64, reply: ReplyStatfs) {
        let blocks = self.block_count();
        let namelen = 255;
        reply.statfs(blocks as _, 0, 0, 0, 0, BLOCK_SIZE as _, namelen, 0);
    }
}

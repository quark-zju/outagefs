use crate::errors::Context;
use crate::vendor::fuse;
use log::debug;
use serde::Deserialize;
use serde::Serialize;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::Path;
use std::rc::Rc;
use std::str::FromStr;

/// Represent data and a list of changes to it.
#[derive(Debug, Clone)]
pub struct Journal {
    /// Initial data.
    pub initial_data: Rc<Vec<u8>>,

    /// Changes applied to the initial data.
    pub changes: Vec<Change>,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub enum Change {
    /// A "write" operation.
    Write {
        offset: usize,
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
    },

    /// A "fsync" operation.
    Sync,
}

/// Describe what changes to take and what to skip.
#[derive(Debug)]
pub struct ChangeFilter {
    should_take: Vec<bool>,
}

impl Journal {
    /// Create `Journal` using specified initial data.
    pub fn new(data: impl Into<Vec<u8>>) -> Self {
        Self {
            initial_data: Rc::new(data.into()),
            changes: Vec::new(),
        }
    }

    /// Return data with changes applied.
    pub fn data(&self, filter: Option<&ChangeFilter>) -> Vec<u8> {
        // Apply chanes
        let mut data = Vec::clone(&self.initial_data);
        for (i, change) in self.changes.iter().enumerate() {
            if let Some(filter) = filter {
                if filter.should_take.get(i) != Some(&true) {
                    continue;
                }
            }
            if let Change::Write { offset, data: b } = &change {
                data[*offset..*offset + b.len()].copy_from_slice(&b);
            }
        }
        data
    }

    /// Dump state to a directory.
    pub fn dump(&self, base_path: &Path, changes_path: &Path) -> io::Result<()> {
        if fs::read(base_path).ok().as_ref() != Some(&*self.initial_data) {
            fs::write(base_path, &*self.initial_data).context(base_path.display())?;
        }
        if !self.changes.is_empty() || changes_path.exists() {
            fs::write(changes_path, varbincode::serialize(&self.changes).unwrap())
                .context(changes_path.display())?;
        }
        Ok(())
    }

    /// Load state from a directory.
    pub fn load(base_path: &Path, changes_path: &Path) -> io::Result<Self> {
        let init = fs::read(base_path).context(&base_path.display())?;
        let changes: Vec<Change> = if changes_path.exists() {
            let data = fs::read(changes_path)?;
            varbincode::deserialize(&data[..])
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid changes data"))?
        } else {
            Vec::new()
        };
        Ok(Self {
            initial_data: Rc::new(init),
            changes,
        })
    }

    /// Mount to the destination path as a single, fixed-sized file.
    ///
    /// Changes to that file are recorded in this journal.
    ///
    /// When the returned value gets dropped, umount the filesystem.
    pub fn mount(
        &mut self,
        dest: &Path,
        opts: &[String],
        filter: Option<&ChangeFilter>,
    ) -> io::Result<fuse::BackgroundSession> {
        let data = self.data(filter);
        let fs = crate::fs::FuseOutageFilesystem::new(data, &mut self.changes);
        // Add '-o allow_root' automatically.
        let uid = unsafe { libc::getuid() };
        let fixed_opts = if opts.contains(&"allow_other".to_string()) || uid == 0 {
            vec![]
        } else {
            vec!["-o".to_string(), "allow_root".to_string()]
        };
        let opts: Vec<&OsStr> = fixed_opts
            .iter()
            .chain(opts.iter())
            .map(|s| OsStr::new(s))
            .collect();
        debug!("fuse mount options: {:?}", &opts);
        return unsafe { fuse::spawn_mount(fs, dest, &opts) };
    }
}

impl FromStr for ChangeFilter {
    type Err = io::Error;

    fn from_str(s: &str) -> io::Result<Self> {
        let mut result = Vec::new();
        let push_bitvec = |result: &mut Vec<bool>, bitvec: &str| -> io::Result<()> {
            for ch in bitvec.chars() {
                match ch {
                    '1' => result.push(true),
                    '0' => result.push(false),
                    _ => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("unexpected char: {}", ch),
                        ))
                    }
                }
            }
            Ok(())
        };

        if s.contains(":") {
            let mut split = s.splitn(2, ":");
            let start_from = split.next().unwrap();
            let bitvec = split.next().unwrap();
            let start_from = start_from
                .parse()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
            for _ in 0..start_from {
                result.push(true);
            }
            push_bitvec(&mut result, &bitvec)?;
        } else {
            push_bitvec(&mut result, &s)?;
        }
        Ok(Self {
            should_take: result,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn test_journal_changes() {
        let mut journal = Journal::new(vec![9, 5, 7]);
        assert_eq!(journal.data(None), vec![9, 5, 7]);
        journal.changes.push(Change::Write {
            offset: 1,
            data: vec![4, 6],
        });
        assert_eq!(journal.data(None), vec![9, 4, 6]);
        journal.changes.push(Change::Write {
            offset: 0,
            data: vec![8, 3],
        });
        assert_eq!(journal.data(None), vec![8, 3, 6]);
    }

    #[test]
    fn test_journal_change_filter() {
        let mut journal = Journal::new(vec![9, 5, 7]);
        journal.changes.push(Change::Write {
            offset: 1,
            data: vec![4, 6],
        });
        journal.changes.push(Change::Write {
            offset: 0,
            data: vec![8, 3],
        });
        let p = |s: &str| -> Option<ChangeFilter> {
            let filter: ChangeFilter = s.parse().unwrap();
            Some(filter)
        };
        assert_eq!(journal.data(p("11").as_ref()), vec![8, 3, 6]);
        assert_eq!(journal.data(p("1:1").as_ref()), vec![8, 3, 6]);
        assert_eq!(journal.data(p("10").as_ref()), vec![9, 4, 6]);
        assert_eq!(journal.data(p("1:0").as_ref()), vec![9, 4, 6]);
        assert_eq!(journal.data(p("01").as_ref()), vec![8, 3, 7]);
        assert_eq!(journal.data(p("00").as_ref()), vec![9, 5, 7]);
        assert_eq!(journal.data(p("2:0").as_ref()), vec![8, 3, 6]);
    }

    #[test]
    fn test_mount() {
        let dir = tempdir().unwrap();

        let mut journal = Journal::new(vec![9, 5, 7]);
        let path = dir.path().join("a");
        fs::write(&path, "").unwrap();

        {
            let _session = journal.mount(&path, &[], None).unwrap();
            assert_eq!(fs::read(&path).unwrap(), vec![9, 5, 7]);
            overwrite(&path, vec![3, 2, 1]);
            // drop _session - umount
        }
        assert!(fs::read(&path).unwrap().is_empty());
        assert_eq!(journal.data(None), vec![3, 2, 1]);

        {
            let _session = journal.mount(&path, &[], None).unwrap();
            assert_eq!(fs::read(&path).unwrap(), vec![3, 2, 1]);
            overwrite(&path, vec![0, 0, 0]);
            // drop _session - umount
        }
        assert_eq!(journal.data(None), vec![0, 0, 0]);

        journal.changes.clear();
        assert_eq!(journal.data(None), vec![9, 5, 7]);
    }

    fn overwrite(path: &Path, data: Vec<u8>) {
        // write without going through the 'create' path.
        let mut file = fs::OpenOptions::new().write(true).open(path).unwrap();
        file.write_all(&data).unwrap();
    }

    #[test]
    fn test_dump_load() {
        let dir = tempdir().unwrap();

        let base_path = dir.path().join("base");
        let changes_path = dir.path().join("changes");
        let mut journal = Journal::new(vec![9, 5, 7]);
        journal.changes.push(Change::Write {
            offset: 1,
            data: vec![4, 6],
        });
        journal.changes.push(Change::Write {
            offset: 0,
            data: vec![8, 3],
        });

        journal.dump(&base_path, &changes_path).unwrap();

        let journal2 = Journal::load(&base_path, &changes_path).unwrap();
        assert_eq!(journal2.changes, journal.changes);
        assert_eq!(journal2.data(None), journal.data(None));
    }
}

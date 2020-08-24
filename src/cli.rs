use crate::errors::Context;
use crate::journal::Change;
use crate::journal::ChangeFilter;
use crate::journal::Journal;
use log::info;
use rand::Rng;
use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::Command;
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
struct PathOpt {
    /// Path to the "base" image
    #[structopt(short, long)]
    #[structopt(default_value = "./base")]
    base: PathBuf,

    /// Path to the "changes" file
    #[structopt(short, long)]
    #[structopt(default_value = "./changes")]
    changes: PathBuf,
}

#[derive(Debug, StructOpt)]
struct FilterOpt {
    /// Filter out certain changes
    ///
    /// For example, "24:01011" means take the first 24 writes,
    /// then skip the 25th (0), take 26th (1), skip 27th (0),
    /// take 28th and 29th write operations.
    #[structopt(short, long)]
    #[structopt(default_value = "")]
    filter: String,
}

#[derive(Debug, StructOpt)]
struct MutateOpt {
    /// Discard Sync operations
    #[structopt(long)]
    drop_sync: bool,

    /// Split large writes into 2048-byte ones
    #[structopt(long)]
    split_write: bool,

    /// Insert Write operations with zeros
    #[structopt(long)]
    zero_fill: bool,
}

#[derive(Debug, StructOpt)]
enum Opt {
    /// Mounts image and record changes
    ///
    /// Without --exec, the process will wait for ENTER in stdin before unmounting.
    /// With --exec, the process will unmount after executing the command.
    Mount {
        #[structopt(flatten)]
        paths: PathOpt,

        #[structopt(flatten)]
        filter: FilterOpt,

        /// FUSE mount options
        #[structopt(long)]
        fuse_args: Vec<String>,

        /// Whether to record changes back to disk.
        #[structopt(short, long)]
        record: bool,

        /// Shell command to run with the mount path as $1
        #[structopt(short, long)]
        exec: Option<String>,

        /// Whether to use 'sudo' to run the command
        #[structopt(long)]
        sudo: bool,

        /// Mount destination
        #[structopt(short, long)]
        #[structopt(default_value = "./mountpoint")]
        dest: PathBuf,
    },

    /// Merges changes into base image
    Merge {
        #[structopt(flatten)]
        paths: PathOpt,

        #[structopt(flatten)]
        filter: FilterOpt,
    },

    /// Mutate the changes
    Mutate {
        #[structopt(flatten)]
        paths: PathOpt,

        #[structopt(flatten)]
        mutate: MutateOpt,
    },

    /// Shows details of a "changes" file
    Show {
        #[structopt(flatten)]
        paths: PathOpt,

        /// Show detailed bytes
        #[structopt(short, long)]
        verbose: bool,
    },

    /// Generate "filter"s for testing
    GenTests {
        #[structopt(flatten)]
        paths: PathOpt,

        /// Log(Maximum test cases generated between 2 Syncs) / Log(2)
        #[structopt(short, long)]
        #[structopt(default_value = "8")]
        max_cases_log2: usize,
    },
}

fn load_journal(opt: &PathOpt) -> io::Result<Journal> {
    info!(
        "reading journal at {} with changes {}",
        opt.base.display(),
        opt.changes.display()
    );
    Journal::load(&opt.base, &opt.changes)
}

fn save_journal(journal: &Journal, opt: &PathOpt) -> io::Result<()> {
    info!(
        "writing journal to {} with changes {}",
        opt.base.display(),
        opt.changes.display()
    );
    journal.dump(&opt.base, &opt.changes)?;
    Ok(())
}

fn mutate_journal(journal: &mut Journal, opt: &MutateOpt) {
    let mut new_changes = Vec::new();
    for change in &journal.changes {
        match change {
            Change::Sync => {
                if !opt.drop_sync {
                    new_changes.push(Change::Sync);
                }
            }
            Change::Write { offset, data } => {
                if opt.zero_fill && data.iter().any(|b| *b != 0) {
                    new_changes.push(Change::Write {
                        offset: *offset,
                        data: vec![0; data.len()],
                    });
                }
                if opt.split_write && data.len() > 2048 {
                    let mut data_offset = 0;
                    while let Some(sub) =
                        data.get(data_offset..(data_offset + 2048).min(data.len()))
                    {
                        if sub.is_empty() {
                            break;
                        }
                        new_changes.push(Change::Write {
                            offset: offset + data_offset,
                            data: sub.to_vec(),
                        });
                        data_offset += sub.len();
                    }
                } else {
                    new_changes.push(change.clone());
                }
            }
        }
    }
    journal.changes = new_changes;
}

fn parse_filter(opt: &FilterOpt) -> io::Result<Option<ChangeFilter>> {
    if opt.filter.is_empty() {
        Ok(None)
    } else {
        opt.filter.parse().map(Some)
    }
}

fn show_changes(changes: &[Change], verbose: bool) {
    if changes.is_empty() {
        info!("No changes");
    }
    for (i, change) in changes.iter().enumerate() {
        print!("{:6} ", i);
        match change {
            Change::Sync => println!("Sync"),
            Change::Write { offset, data } => {
                if verbose {
                    println!("Write at {} with {:?}", offset, data);
                } else {
                    let is_zero = data.iter().all(|b| *b == 0);
                    println!(
                        "Write at {} with {} bytes{}",
                        offset,
                        data.len(),
                        if is_zero { " of zeros" } else { "" }
                    );
                }
            }
        }
    }
}

fn gen_tests(mut changes: Vec<Change>, max_width: usize) {
    // Ensure the last change is Sync.
    if let Some(Change::Write { .. }) = changes.last() {
        changes.push(Change::Sync);
    }
    // Figure out locations of "Sync"s.
    let mut sync_indexes = Vec::new();
    for (i, change) in changes.iter().enumerate() {
        if let Change::Sync = change {
            sync_indexes.push(i);
        }
    }
    // For each "Sync", generate test cases.
    for (i, sync_index) in sync_indexes.iter().enumerate() {
        // start_index .. sync_index
        let start_index = if i == 0 { 0 } else { sync_indexes[i - 1] + 1 };
        let width = sync_index - start_index;
        if width == 0 {
            // Ignore - no writes.
        } else if width <= max_width {
            info!(
                "# All cases for {} writes before #{} Sync",
                width, sync_index,
            );
            for bits in 0..(1 << width) {
                println!("{}:{:0width$b}", start_index, bits, width = width);
            }
        } else {
            let n = 1 << max_width;
            info!(
                "# Random {} cases for {} writes before #{} Sync",
                n, width, sync_index,
            );
            let mut bits = vec![false; width];
            let mut rng = rand::thread_rng();
            let mut visited: HashSet<String> = HashSet::new();
            while visited.len() < n {
                // Do a few bit flips.
                for _ in 0..((width + max_width - 1) / max_width) {
                    let idx = rng.gen_range(0, width);
                    bits[idx] = !bits[idx];
                }
                let bits_str: String = bits
                    .iter()
                    .map(|&b| if b { "1" } else { "0" })
                    .collect::<Vec<&str>>()
                    .concat();
                if visited.insert(bits_str.clone()) {
                    println!("{}:{}", start_index, bits_str);
                }
            }
        }
    }
}

fn wait_stdin() {
    let stdin = io::stdin();
    let mut s = String::new();
    let _ = stdin.read_line(&mut s);
}

pub(crate) fn main() -> io::Result<()> {
    let opt = Opt::from_args();
    match opt {
        Opt::Mount {
            paths,
            fuse_args,
            dest,
            filter,
            exec,
            sudo,
            record,
        } => {
            let mut journal = load_journal(&paths)?;
            let filter = parse_filter(&filter)?;
            // Create the file if it does not exist.
            let _ = fs::OpenOptions::new().write(true).create(true).open(&dest);
            let session = journal
                .mount(&dest, &fuse_args, filter.as_ref())
                .context(format!("mounting recordfs to {}", dest.display()))?;
            info!("mounted: {}", dest.display());
            match exec {
                Some(cmd) => {
                    let mut args = vec![
                        "/bin/sh".into(),
                        "-c".into(),
                        cmd.clone(),
                        "--".into(),
                        dest.display().to_string(),
                    ];
                    if sudo {
                        args.insert(0, "/bin/sudo".into());
                    }
                    info!("running: {}", shell_words::join(&args[..]));

                    let mut child = Command::new(&args[0])
                        .args(&args[1..])
                        .spawn()
                        .context("spawning")?;
                    let status = child.wait().context("waiting child")?;
                    if let Some(code) = status.code() {
                        info!("child exited with {}", code);
                    }
                }
                None => {
                    info!("press ENTER to write changes and unmount");
                    wait_stdin();
                }
            }
            drop(session);
            info!("unmounted: {}", dest.display());
            if record {
                journal.dump(&paths.base, &paths.changes)?;
                info!("changes written: {}", paths.changes.display());
            }
        }
        Opt::Merge { paths, filter } => {
            let journal = load_journal(&paths)?;
            let filter = parse_filter(&filter)?;
            let data = journal.data(filter.as_ref());
            let journal = Journal::new(data);
            save_journal(&journal, &paths)?;
        }
        Opt::Mutate { paths, mutate } => {
            let mut journal = load_journal(&paths)?;
            mutate_journal(&mut journal, &mutate);
            save_journal(&journal, &paths)?;
        }
        Opt::Show { paths, verbose } => {
            let journal = load_journal(&paths)?;
            show_changes(&journal.changes, verbose);
        }
        Opt::GenTests {
            paths,
            max_cases_log2,
        } => {
            let journal = load_journal(&paths)?;
            gen_tests(journal.changes, max_cases_log2);
        }
    }
    Ok(())
}

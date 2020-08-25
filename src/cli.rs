use crate::errors::Context;
use crate::journal::Change;
use crate::journal::ChangeFilter;
use crate::journal::Journal;
use log::info;
use rand::Rng;
use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::ExitStatus;
use structopt::StructOpt;
use tempfile::tempdir;

#[derive(Debug, Clone, StructOpt)]
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

#[derive(Debug, Default, StructOpt)]
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

#[derive(Debug, Clone, StructOpt)]
struct RunOpt {
    /// Whether to use 'sudo' to run the command
    #[structopt(long)]
    sudo: bool,
}

#[derive(Debug, Clone, StructOpt)]
struct GenTestsOpt {
    /// Log(Maximum test cases generated between 2 Syncs) / Log(2)
    #[structopt(short, long)]
    #[structopt(default_value = "8")]
    max_cases_log2: usize,
}

#[derive(Debug, StructOpt)]
struct MountOpt {
    #[structopt(flatten)]
    paths: PathOpt,

    #[structopt(flatten)]
    filter: FilterOpt,

    #[structopt(flatten)]
    run: RunOpt,

    /// FUSE mount options
    #[structopt(long)]
    fuse_args: Vec<String>,

    /// Whether to record changes back to disk.
    #[structopt(short, long)]
    record: bool,

    /// Shell command to run with the mount path as $1
    #[structopt(short, long)]
    exec: Option<String>,

    /// Mount destination
    #[structopt(short, long)]
    #[structopt(default_value = "./mountpoint")]
    dest: PathBuf,
}

#[derive(Debug, StructOpt)]
enum Opt {
    /// Mounts image and record changes
    ///
    /// Without --exec, the process will wait for ENTER in stdin before unmounting.
    /// With --exec, the process will unmount after executing the command.
    Mount {
        #[structopt(flatten)]
        opts: MountOpt,
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

        #[structopt(flatten)]
        test: GenTestsOpt,
    },

    /// Run a test suite script
    ///
    /// The script will receive `argv[1]` telling it what to do:
    ///
    /// * prepare: Prepare the initial filesystem. Output to `argv[2]`.
    ///
    /// * changes: Make changes that will be recorded. Input is `argv[2]`.
    ///
    /// * verify: Check properties. Input is `argv[2]`. Return value in 10..20
    /// are considered as "successful", and are used to "bisect" test cases.
    ///
    /// If the script returns non-zero exit code, and is not in the 10..20
    /// range, then verification stops and prints the test case.
    ///
    /// The input and output files are created in a temporary directory
    /// that will be deleted unless `--keep` is set.
    RunSuite {
        /// Script to run
        script_path: PathBuf,

        /// Whether to keep the temporary directory
        #[structopt(short, long)]
        keep: bool,

        #[structopt(flatten)]
        run: RunOpt,

        #[structopt(flatten)]
        test: GenTestsOpt,
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

fn gen_tests(mut changes: Vec<Change>, opt: &GenTestsOpt) -> Vec<String> {
    let max_width: usize = opt.max_cases_log2;
    let mut result = Vec::new();

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
                result.push(format!("{}:{:0width$b}", start_index, bits, width = width));
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
                let bit_flip_count = rng.gen_range(1, width *2 / max_width);
                for _ in 0..bit_flip_count {
                    let idx = rng.gen_range(0, width);
                    bits[idx] = !bits[idx];
                }
                let bits_str: String = bits
                    .iter()
                    .map(|&b| if b { "1" } else { "0" })
                    .collect::<Vec<&str>>()
                    .concat();
                if visited.insert(bits_str.clone()) {
                    result.push(format!("{}:{}", start_index, bits_str));
                }
            }
        }
    }

    result
}

fn wait_stdin() {
    let stdin = io::stdin();
    let mut s = String::new();
    let _ = stdin.read_line(&mut s);
}

fn execute(mut args: Vec<String>, run: &RunOpt) -> io::Result<ExitStatus> {
    if run.sudo {
        args.insert(0, "/bin/sudo".to_string());
    }
    info!("running: {}", shell_words::join(&args[..]));
    Command::new(&args[0])
        .args(&args[1..])
        .status()
        .context("run script")
}

fn mount(opts: MountOpt) -> io::Result<i32> {
    let MountOpt {
        paths,
        fuse_args,
        dest,
        filter,
        exec,
        run,
        record,
    } = opts;

    let mut result = 0;
    let mut journal = load_journal(&paths)?;
    let filter = parse_filter(&filter)?;
    // Create the file if it does not exist.
    let _ = fs::OpenOptions::new().write(true).create(true).open(&dest);
    let session = journal
        .mount(&dest, &fuse_args, filter.as_ref())
        .context(format!("mounting outagefs to {}", dest.display()))?;
    info!("mounted: {}", dest.display());
    match exec {
        Some(cmd) => {
            let sh_args = vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                cmd.clone(),
                "--".to_string(),
                dest.display().to_string(),
            ];
            let status = execute(sh_args, &run)?;
            if let Some(code) = status.code() {
                result = code;
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
        save_journal(&journal, &paths)?;
        info!("changes written: {}", paths.changes.display());
    }
    Ok(result)
}

fn run_script(script_path: &str, run: &RunOpt, test: &GenTestsOpt) -> io::Result<i32> {
    // Prepare
    let paths = PathOpt {
        base: "base".into(),
        changes: "changes".into(),
    };
    execute(
        vec![
            script_path.to_string(),
            "prepare".into(),
            paths.base.display().to_string(),
        ],
        &run,
    )
    .context("executing prepare script")?;

    // Record changes
    let dest = Path::new("mountpoint").to_path_buf();
    mount(MountOpt {
        paths: paths.clone(),
        filter: FilterOpt::default(),
        fuse_args: Vec::new(),
        run: run.clone(),
        record: true,
        exec: Some(shell_words::join(vec![
            script_path.to_string(),
            "changes".to_string(),
            dest.display().to_string(),
        ])),
        dest: dest.clone(),
    })
    .context("runing mount subcommand to record changes")?;

    // Tests
    let journal = load_journal(&paths)?;
    let tests = gen_tests(journal.changes, test);
    let total = tests.len();
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    #[repr(u8)]
    enum Tested {
        Unknown,
        Pass(usize),
    }
    let mut tested = vec![Tested::Unknown; tests.len()];
    let mut tested_count = 0;
    let mut next_test_index = 0;
    while tested_count < tests.len() {
        let i = next_test_index;
        tested_count += 1;
        assert_eq!(tested[i], Tested::Unknown);
        eprintln!("[{} of {}] Test Case #{}", tested_count, total, i);
        let code = mount(MountOpt {
            paths: paths.clone(),
            filter: FilterOpt {
                filter: tests[i].clone(),
            },
            fuse_args: Vec::new(),
            run: run.clone(),
            record: false,
            exec: Some(shell_words::join(vec![
                script_path.to_string(),
                "verify".into(),
                dest.display().to_string(),
            ])),
            dest: dest.clone(),
        })
        .context(format!("runing mount subcommand to verify {}", &tests[i]))?;
        info!("verify script returned {}", code);
        if code >= 10 && code < 20 {
            tested[i] = Tested::Pass((code - 10) as _);
        } else if code == 0 {
            tested[i] = Tested::Pass(0);
        } else {
            eprintln!("verify script returned {} for filter {}", code, &tests[i]);
            return Ok(code);
        }

        if tested_count >= tests.len() {
            break;
        }

        // Find the next "interesting" test.
        next_test_index = if i == 0 {
            tests.len() - 1
        } else {
            // Find a bisect range.
            let mut best_range_start = 0;
            let mut best_range_distance = 0;
            let mut last_pass_start = 0;
            let mut last_pass_variant = 0;
            for j in 0..tests.len() {
                match tested[j] {
                    Tested::Unknown => continue,
                    Tested::Pass(v) => {
                        if v != last_pass_variant && j - last_pass_start > best_range_distance {
                            best_range_distance = j - last_pass_start;
                            best_range_start = last_pass_start;
                        }
                        last_pass_start = j;
                        last_pass_variant = v;
                    }
                }
            }
            let best_range_end = best_range_start + best_range_distance;
            let best_range_mid = (best_range_end + best_range_start) / 2;
            if best_range_distance > 1 {
                info!(
                    "bisect {}..{}: {}",
                    best_range_start, best_range_end, best_range_mid
                );
                best_range_mid
            } else {
                let mut j = (i + 1) % tests.len();
                let mut count = 0;
                while tested[j] != Tested::Unknown {
                    j += 1;
                    count += 1;
                    assert!(count <= tests.len());
                    if j >= tests.len() {
                        j = 0;
                    }
                }
                info!("picking next untested case: {}", j);
                j
            }
        };
    }
    eprintln!("{} test cases verified", tested_count);
    Ok(0)
}

pub(crate) fn main() -> io::Result<()> {
    let opt = Opt::from_args();
    match opt {
        Opt::Mount { opts } => {
            mount(opts)?;
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
        Opt::GenTests { paths, test } => {
            let journal = load_journal(&paths)?;
            for s in gen_tests(journal.changes, &test) {
                println!("{}", s);
            }
        }
        Opt::RunSuite {
            script_path,
            keep,
            run,
            test,
        } => {
            let script_path = script_path.canonicalize()?.display().to_string();
            let tmpdir = tempdir()?;
            let dir = &tmpdir.path();
            info!("chdir: {}", dir.display());
            std::env::set_current_dir(dir)?;
            let _code = run_script(&script_path, &run, &test)?;
            if keep {
                eprintln!("keep tmpdir: {}", tmpdir.into_path().display());
            }
        }
    }
    Ok(())
}

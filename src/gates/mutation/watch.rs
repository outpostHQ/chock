//! Watches the progress journals mutest writes and fails the run when a test passes its deadline.
//! Only a matching end record closes a deadline; no other output extends one.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::progress::{FILE_LIMIT, LINE_LIMIT, Progress, hex_id};

const FILES_LIMIT: usize = 128;
// O_NOFOLLOW refuses a symlink planted at a journal's path and O_NONBLOCK a FIFO swapped in for it.
// Linux numbers them per architecture, and a wrong number drops the guard without an error.
#[cfg(any(
    target_arch = "aarch64",
    target_arch = "arm",
    target_arch = "m68k",
    target_arch = "powerpc",
    target_arch = "powerpc64"
))]
const NOFOLLOW: i32 = 0o100_000;
#[cfg(not(any(
    target_arch = "aarch64",
    target_arch = "arm",
    target_arch = "m68k",
    target_arch = "powerpc",
    target_arch = "powerpc64"
)))]
const NOFOLLOW: i32 = 0o400_000;
#[cfg(any(target_arch = "mips", target_arch = "mips64"))]
const NONBLOCK: i32 = 0o200;
#[cfg(any(target_arch = "sparc", target_arch = "sparc64"))]
const NONBLOCK: i32 = 0o40_000;
#[cfg(not(any(
    target_arch = "mips",
    target_arch = "mips64",
    target_arch = "sparc",
    target_arch = "sparc64"
)))]
const NONBLOCK: i32 = 0o4_000;
const OPEN_FLAGS: i32 = NOFOLLOW | NONBLOCK;

pub(crate) struct Watch {
    directory: PathBuf,
    nonce: String,
    identity: (u64, u64),
    started: Instant,
    files: BTreeMap<String, Journal>,
}

struct Journal {
    file: File,
    identity: (u64, u64),
    read: u64,
    partial: Vec<u8>,
    progress: Progress,
    correlated: bool,
}

fn create_private(directory: &Path) -> Result<(), String> {
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(directory)
        .map_err(|error| format!("cannot create private mutation progress directory: {error}"))
}

fn identity(metadata: &std::fs::Metadata) -> (u64, u64) {
    (metadata.dev(), metadata.ino())
}

fn random_nonce() -> Result<String, String> {
    let mut bytes = [0; 16];
    File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|error| format!("cannot allocate mutation run identity: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

impl Watch {
    pub(crate) fn create(root: &Path) -> Result<Self, String> {
        let nonce = random_nonce()?;
        let parent = root.join("target/test-scratch");
        std::fs::create_dir_all(&parent).map_err(|error| error.to_string())?;
        let parent = parent.canonicalize().map_err(|error| error.to_string())?;
        if !parent.starts_with(root) {
            return Err("mutation progress directory escapes the project".to_string());
        }
        let directory = parent.join(format!("mutest-progress-{nonce}"));
        create_private(&directory)?;
        let metadata = directory
            .symlink_metadata()
            .map_err(|error| error.to_string())?;
        Ok(Self {
            directory,
            nonce,
            identity: identity(&metadata),
            started: Instant::now(),
            files: BTreeMap::new(),
        })
    }

    pub(crate) fn environment(&self) -> Result<[(&str, &str); 2], String> {
        let path = self
            .directory
            .to_str()
            .ok_or("mutation progress path is not UTF-8")?;
        Ok([
            ("MUTEST_PROGRESS_DIR", path),
            ("MUTEST_PROGRESS_NONCE", &self.nonce),
        ])
    }

    pub(crate) fn poll(&mut self) -> Result<(), String> {
        self.inspect_directory()?;
        let mut seen = Vec::new();
        for entry in std::fs::read_dir(&self.directory).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "non-UTF-8 mutation progress filename")?;
            let (pid, instance) = journal_name(&name, &self.nonce)?;
            if seen.len() >= FILES_LIMIT {
                return Err("too many mutation progress files".to_string());
            }
            seen.push(name.clone());
            if !self.files.contains_key(&name) {
                let journal = Journal::open(&entry.path(), &self.nonce)?;
                self.files.insert(name.clone(), journal);
            }
            let journal = self
                .files
                .get_mut(&name)
                .ok_or("mutation progress file disappeared")?;
            journal.update(&entry.path(), self.started)?;
            if let Some(header) = journal.progress.header() {
                if header.instance_id != instance || header.pid != pid {
                    return Err(
                        "mutation progress filename does not match its instance".to_string()
                    );
                }
                if !journal.correlated {
                    correlate(header.pid, header.start_ticks, &header.exe)?;
                    journal.correlated = true;
                }
            }
            journal.progress.check(self.started.elapsed())?;
        }
        if self.files.keys().any(|name| !seen.contains(name)) {
            return Err("mutation progress file was removed during execution".to_string());
        }
        Ok(())
    }

    fn inspect_directory(&self) -> Result<(), String> {
        let metadata = self
            .directory
            .symlink_metadata()
            .map_err(|error| error.to_string())?;
        if !metadata.file_type().is_dir()
            || identity(&metadata) != self.identity
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.permissions().mode() & 0o777 != 0o700
        {
            return Err("private mutation progress directory changed".to_string());
        }
        Ok(())
    }

    pub(crate) fn complete(&mut self) -> Result<(), String> {
        self.poll()?;
        if self.files.is_empty() {
            return Err(
                "mutest wrote no progress records, so no mutation was evaluated".to_string(),
            );
        }
        for journal in self.files.values() {
            if !journal.partial.is_empty() {
                return Err("mutest ended with a partial progress record".to_string());
            }
            journal.progress.complete()?;
        }
        Ok(())
    }

    pub(crate) fn accept(&self) -> Result<(), String> {
        self.inspect_directory()?;
        std::fs::remove_dir_all(&self.directory)
            .map_err(|error| format!("cannot remove completed mutation progress: {error}"))
    }

    pub(crate) fn retained(&self) -> String {
        format!(
            "; mutation progress retained at {}",
            self.directory.display()
        )
    }
}

impl crate::exec::Watchdog for Watch {
    fn poll(&mut self) -> Result<(), String> {
        Watch::poll(self)
    }
    fn complete(&mut self) -> Result<(), String> {
        Watch::complete(self)
    }
}

impl Journal {
    fn open(path: &Path, nonce: &str) -> Result<Self, String> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(OPEN_FLAGS)
            .open(path)
            .map_err(|error| format!("cannot open mutation progress: {error}"))?;
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file()
            || metadata.permissions().mode() & 0o777 != 0o600
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.nlink() != 1
        {
            return Err("mutation progress must be an owner-only regular file".to_string());
        }
        Ok(Self {
            file,
            identity: identity(&metadata),
            read: 0,
            partial: Vec::new(),
            progress: Progress::new(nonce.to_string()),
            correlated: false,
        })
    }

    fn update(&mut self, path: &Path, started: Instant) -> Result<(), String> {
        let metadata = path.symlink_metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file()
            || identity(&metadata) != self.identity
            || metadata.len() < self.read
            || metadata.len() > FILE_LIMIT
            || metadata.permissions().mode() & 0o777 != 0o600
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.nlink() != 1
        {
            return Err(
                "mutation progress was replaced, truncated, or exceeded its bound".to_string(),
            );
        }
        let remaining = metadata.len() - self.read;
        let mut bytes = Vec::new();
        self.file
            .by_ref()
            .take(remaining)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        let now = started.elapsed();
        self.read += bytes.len() as u64;
        for byte in bytes {
            if byte == b'\n' {
                let line = std::str::from_utf8(&self.partial)
                    .map_err(|_| "mutation progress is not UTF-8")?;
                self.progress.ingest(line, now)?;
                self.partial.clear();
            } else {
                self.partial.push(byte);
                if self.partial.len() >= LINE_LIMIT {
                    return Err("mutation progress record exceeds its byte limit".to_string());
                }
            }
        }
        Ok(())
    }
}

fn journal_name<'a>(name: &'a str, nonce: &str) -> Result<(u32, &'a str), String> {
    let (pid, instance) = name
        .strip_prefix(nonce)
        .and_then(|rest| rest.strip_prefix('-'))
        .and_then(|rest| rest.strip_suffix(".jsonl"))
        .and_then(|rest| rest.split_once('-'))
        .ok_or("unexpected file in mutation progress directory")?;
    let pid = pid
        .parse::<u32>()
        .ok()
        .filter(|pid| *pid > 0)
        .ok_or("invalid progress process ID")?;
    if !hex_id(instance) {
        return Err("invalid mutation progress instance ID".to_string());
    }
    Ok((pid, instance))
}

fn correlate(pid: u32, ticks: u64, exe: &str) -> Result<(), String> {
    let root = PathBuf::from(format!("/proc/{pid}"));
    let Some(actual_ticks) = process_ticks(std::fs::read_to_string(root.join("stat")))? else {
        return Ok(());
    };
    if actual_ticks != ticks {
        return Err("mutation progress process identity is stale".to_string());
    }
    checked_executable(std::fs::read_link(root.join("exe")), Path::new(exe))
}

fn checked_executable(actual: std::io::Result<PathBuf>, expected: &Path) -> Result<(), String> {
    match actual {
        Ok(actual) if actual == expected => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err("mutation progress executable identity changed".to_string()),
    }
}

fn process_ticks(stat: std::io::Result<String>) -> Result<Option<u64>, String> {
    match stat {
        Ok(text) => text
            .rsplit_once(')')
            .and_then(|(_, fields)| fields.split_whitespace().nth(19))
            .and_then(|ticks| ticks.parse().ok())
            .map(Some)
            .ok_or_else(|| "malformed process status".to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("cannot read mutation process status: {error}")),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "invalid fixture state is a failing test"
)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::io::Write;
    use std::time::Duration;

    const INSTANCE: &str = "0123456789abcdef0123456789abcdef";

    fn setup() -> (crate::testdir::Scratch, Watch, PathBuf) {
        let root = crate::testdir::make("mutation-watch");
        let watch = Watch::create(&root).unwrap();
        let file = watch.directory.join(format!(
            "{}-{}-{INSTANCE}.jsonl",
            watch.nonce,
            std::process::id()
        ));
        (root, watch, file)
    }

    fn append(watch: &Watch, file: &Path, seq: u64, event: &str, fields: Value) {
        let mut row = json!({"schema":"mutest-progress","version":1,"event":event,
            "nonce":watch.nonce,"instance_id":INSTANCE,"seq":seq,"elapsed_ms":0});
        row.as_object_mut()
            .unwrap()
            .extend(serde_json::from_value::<serde_json::Map<String, Value>>(fields).unwrap());
        let mut out = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(file)
            .unwrap();
        writeln!(out, "{row}").unwrap();
    }

    fn header(watch: &Watch, file: &Path) {
        let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
        let ticks: u64 = stat
            .rsplit_once(')')
            .unwrap()
            .1
            .split_whitespace()
            .nth(19)
            .unwrap()
            .parse()
            .unwrap();
        append(
            watch,
            file,
            0,
            "header",
            json!({"pid":std::process::id(),
            "process_start":{"kind":"linux-proc-starttime","ticks":ticks},
            "exe":std::fs::read_link("/proc/self/exe").unwrap().to_str().unwrap(),
            "record_limit_bytes":16384,"file_limit_bytes":67108864}),
        );
    }

    #[test]
    fn watchdog_dispatch_reads_appends_and_correlates_the_header() {
        let (_root, mut watch, file) = setup();
        header(&watch, &file);
        let journal = Journal::open(&file, &watch.nonce).unwrap();
        assert!(!journal.correlated);
        assert_eq!(crate::exec::Watchdog::poll(&mut watch), Ok(()));
        let name = file.file_name().unwrap().to_str().unwrap();
        let observed = &watch.files[name];
        assert!(observed.correlated);
        let observed_header = observed.progress.header().unwrap();
        assert_eq!(observed_header.pid, std::process::id());
        assert_eq!(observed_header.instance_id, INSTANCE);
        assert_eq!(crate::exec::Watchdog::poll(&mut watch), Ok(()));
        append(&watch, &file, 1, "phase", json!({"phase":"evaluation"}));
        append(
            &watch,
            &file,
            2,
            "terminal",
            json!({"status":"completed","exit_code":0}),
        );
        assert_eq!(crate::exec::Watchdog::complete(&mut watch), Ok(()));
        assert_eq!(crate::exec::Watchdog::complete(&mut watch), Ok(()));
    }

    #[test]
    fn watchdog_dispatch_rejects_a_stale_header_before_completion() {
        let (_root, mut watch, file) = setup();
        append(
            &watch,
            &file,
            0,
            "header",
            json!({"pid":std::process::id(),
                "process_start":{"kind":"linux-proc-starttime","ticks":0},
                "exe":std::fs::read_link("/proc/self/exe").unwrap().to_str().unwrap(),
                "record_limit_bytes":16384,"file_limit_bytes":67108864}),
        );
        append(
            &watch,
            &file,
            1,
            "terminal",
            json!({"status":"completed","exit_code":0}),
        );
        let watchdog: &mut dyn crate::exec::Watchdog = &mut watch;
        let expected = Err("mutation progress process identity is stale".into());
        assert_eq!(watchdog.poll(), expected);
        assert_eq!(watchdog.complete(), expected);
    }

    #[test]
    fn replacing_a_private_directory_preserves_the_unaccepted_replacement() {
        let (root, watch, _) = setup();
        let metadata = watch.directory.symlink_metadata().unwrap();
        assert_eq!(watch.identity, (metadata.dev(), metadata.ino()));
        assert_eq!(
            watch.environment(),
            Ok([
                ("MUTEST_PROGRESS_DIR", watch.directory.to_str().unwrap()),
                ("MUTEST_PROGRESS_NONCE", watch.nonce.as_str()),
            ])
        );
        assert_eq!(
            watch.retained(),
            format!(
                "; mutation progress retained at {}",
                watch.directory.display()
            )
        );
        std::fs::rename(&watch.directory, root.join("original-progress")).unwrap();
        create_private(&watch.directory).unwrap();
        let marker = watch.directory.join("replacement-marker");
        std::fs::write(&marker, b"not accepted evidence").unwrap();
        let expected = Err("private mutation progress directory changed".into());
        assert_eq!(watch.inspect_directory(), expected);
        assert_eq!(watch.accept(), expected);
        assert_eq!(std::fs::read(marker).unwrap(), b"not accepted evidence");
    }

    #[test]
    fn non_utf8_paths_and_filenames_are_reported_without_panicking() {
        use std::os::unix::ffi::OsStringExt;
        let root = crate::testdir::make("watch-non-utf8");
        let name = std::ffi::OsString::from_vec(vec![b'p', 0xff]);
        let invalid_root = root.join(&name);
        std::fs::create_dir(&invalid_root).unwrap();
        let mut watch = Watch::create(&invalid_root).unwrap();
        assert_eq!(
            watch.environment(),
            Err("mutation progress path is not UTF-8".into())
        );
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(watch.directory.join(name))
            .unwrap();
        assert_eq!(
            watch.poll(),
            Err("non-UTF-8 mutation progress filename".into())
        );
    }

    #[test]
    fn non_utf8_records_cannot_become_progress() {
        let (_root, mut watch, file) = setup();
        header(&watch, &file);
        watch.poll().unwrap();
        OpenOptions::new()
            .append(true)
            .open(&file)
            .unwrap()
            .write_all(&[0xff, b'\n'])
            .unwrap();
        assert_eq!(watch.poll(), Err("mutation progress is not UTF-8".into()));
    }

    #[test]
    fn observed_journals_reject_truncation_permissions_and_additional_links() {
        for change in ["truncate", "permissions", "hard-link"] {
            let (root, mut watch, file) = setup();
            header(&watch, &file);
            assert_eq!(watch.poll(), Ok(()));
            match change {
                "truncate" => OpenOptions::new()
                    .write(true)
                    .open(&file)
                    .unwrap()
                    .set_len(0)
                    .unwrap(),
                "permissions" => {
                    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640))
                        .unwrap();
                }
                _ => std::fs::hard_link(&file, root.join("linked-progress")).unwrap(),
            }
            assert_eq!(
                watch.poll(),
                Err("mutation progress was replaced, truncated, or exceeded its bound".into()),
                "{change}"
            );
        }
    }

    #[test]
    fn journal_size_limit_accepts_the_boundary_at_an_existing_read_offset() {
        use std::io::{Seek, SeekFrom};
        let (_root, watch, file) = setup();
        header(&watch, &file);
        let mut journal = Journal::open(&file, &watch.nonce).unwrap();
        journal.update(&file, Instant::now()).unwrap();
        let writer = OpenOptions::new().write(true).open(&file).unwrap();
        writer.set_len(FILE_LIMIT).unwrap();
        // Model an already consumed prefix without allocating or reading 64 MiB.
        journal.file.seek(SeekFrom::End(0)).unwrap();
        journal.read = FILE_LIMIT;
        assert_eq!(journal.update(&file, Instant::now()), Ok(()));
        writer.set_len(FILE_LIMIT + 1).unwrap();
        assert_eq!(
            journal.update(&file, Instant::now()),
            Err("mutation progress was replaced, truncated, or exceeded its bound".into())
        );
    }

    #[test]
    fn a_partial_record_can_reach_but_not_cross_its_last_accepted_byte() {
        let (_root, watch, file) = setup();
        let mut writer = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&file)
            .unwrap();
        let partial = vec![b' '; LINE_LIMIT - 1];
        writer.write_all(&partial).unwrap();
        let mut journal = Journal::open(&file, &watch.nonce).unwrap();
        assert_eq!(journal.update(&file, Instant::now()), Ok(()));
        assert_eq!(journal.partial, partial);
        writer.write_all(b" ").unwrap();
        assert_eq!(
            journal.update(&file, Instant::now()),
            Err("mutation progress record exceeds its byte limit".into())
        );
    }

    #[test]
    fn journal_names_enforce_pid_and_lowercase_identity_boundaries() {
        assert_eq!(
            journal_name(&format!("run-1-{INSTANCE}.jsonl"), "run"),
            Ok((1, INSTANCE))
        );
        assert_eq!(
            journal_name(&format!("run-{}-{INSTANCE}.jsonl", u32::MAX), "run"),
            Ok((u32::MAX, INSTANCE))
        );
        for pid in ["0", "-1", "4294967296", ""] {
            assert_eq!(
                journal_name(&format!("run-{pid}-{INSTANCE}.jsonl"), "run"),
                Err("invalid progress process ID".into())
            );
        }
        for instance in [
            INSTANCE[..31].to_owned(),
            format!("{INSTANCE}0"),
            INSTANCE.to_ascii_uppercase(),
            "g123456789abcdef0123456789abcdef".into(),
        ] {
            assert_eq!(
                journal_name(&format!("run-1-{instance}.jsonl"), "run"),
                Err("invalid mutation progress instance ID".into())
            );
        }
    }

    #[test]
    fn current_completed_records_are_accepted_and_removed_after_use() {
        let (_root, mut watch, file) = setup();
        header(&watch, &file);
        append(&watch, &file, 1, "phase", json!({"phase":"evaluation"}));
        append(
            &watch,
            &file,
            2,
            "terminal",
            json!({"status":"completed","exit_code":2}),
        );
        assert_eq!(watch.complete(), Ok(()));
        watch.accept().unwrap();
        let path = watch.directory.clone();
        drop(watch);
        assert!(!path.exists());
    }

    #[test]
    fn missing_partial_and_incomplete_records_never_establish_completion() {
        let (_root, mut watch, file) = setup();
        assert!(
            watch
                .complete()
                .unwrap_err()
                .contains("no progress records")
        );
        header(&watch, &file);
        watch.poll().unwrap();
        assert_eq!(
            watch.complete(),
            Err("missing completed terminal event".into())
        );
        OpenOptions::new()
            .append(true)
            .open(&file)
            .unwrap()
            .write_all(b"{")
            .unwrap();
        assert_eq!(
            watch.complete(),
            Err("mutest ended with a partial progress record".into())
        );
        let path = watch.directory.clone();
        drop(watch);
        assert!(path.exists());
    }

    #[test]
    fn symlinks_and_special_progress_files_are_refused_without_blocking() {
        let (_root, mut watch, file) = setup();
        let target = watch.directory.join("other");
        std::fs::write(&target, b"not a journal").unwrap();
        std::os::unix::fs::symlink(&target, &file).unwrap();
        assert!(Journal::open(&file, &watch.nonce).is_err());
        assert!(watch.poll().is_err());
    }

    #[test]
    fn observed_files_cannot_be_removed_or_replaced() {
        let (_root, mut watch, file) = setup();
        header(&watch, &file);
        watch.poll().unwrap();
        std::fs::remove_file(&file).unwrap();
        assert_eq!(
            watch.poll(),
            Err("mutation progress file was removed during execution".into())
        );
        header(&watch, &file);
        assert!(watch.poll().unwrap_err().contains("replaced"));
    }

    #[test]
    fn progress_permissions_and_size_limits_are_checked() {
        let (_root, mut watch, file) = setup();
        header(&watch, &file);
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(watch.poll().unwrap_err().contains("owner-only"));
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut journal = Journal::open(&file, &watch.nonce).unwrap();
        OpenOptions::new()
            .write(true)
            .open(&file)
            .unwrap()
            .set_len(FILE_LIMIT + 1)
            .unwrap();
        assert!(
            journal
                .update(&file, Instant::now())
                .unwrap_err()
                .contains("bound")
        );
    }

    #[test]
    fn executable_identity_distinguishes_a_gone_process_from_unreadable_metadata() {
        let expected = Path::new("/bin/fixture");
        assert_eq!(checked_executable(Ok(expected.into()), expected), Ok(()));
        assert_eq!(
            checked_executable(Ok(PathBuf::from("/other")), expected),
            Err("mutation progress executable identity changed".into())
        );
        assert_eq!(
            checked_executable(Err(std::io::ErrorKind::NotFound.into()), expected),
            Ok(())
        );
        assert_eq!(
            checked_executable(Err(std::io::ErrorKind::PermissionDenied.into()), expected),
            Err("mutation progress executable identity changed".into())
        );
    }

    #[test]
    fn stale_process_identity_is_not_current_progress() {
        let exe = std::fs::read_link("/proc/self/exe").unwrap();
        assert_eq!(
            correlate(std::process::id(), 0, exe.to_str().unwrap()),
            Err("mutation progress process identity is stale".into())
        );
        assert!(journal_name("bad.jsonl", "0123456789abcdef0123456789abcdef").is_err());
    }

    #[test]
    fn changed_private_directory_and_header_names_are_refused() {
        let (_root, mut watch, file) = setup();
        header(&watch, &file);
        let renamed = watch
            .directory
            .join(format!("{}-1-{INSTANCE}.jsonl", watch.nonce));
        std::fs::rename(&file, &renamed).unwrap();
        assert!(
            watch
                .poll()
                .unwrap_err()
                .contains("filename does not match")
        );
        std::fs::set_permissions(&watch.directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            watch.poll(),
            Err("private mutation progress directory changed".into())
        );
    }

    #[test]
    fn an_existing_private_directory_is_not_reused() {
        let (_root, watch, _) = setup();
        assert!(
            create_private(&watch.directory)
                .unwrap_err()
                .starts_with("cannot create private mutation progress directory:")
        );
        assert!(watch.inspect_directory().is_ok());
    }

    #[test]
    fn progress_roots_cannot_escape_through_a_symlink() {
        let root = crate::testdir::make("watch-root-escape");
        let outside = crate::testdir::make("watch-outside");
        std::fs::create_dir(root.join("target")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("target/test-scratch")).unwrap();
        let result = Watch::create(&root).err();
        assert_eq!(
            result.as_deref(),
            Some("mutation progress directory escapes the project")
        );
    }

    #[test]
    fn too_many_journals_and_overlong_records_are_refused() {
        let (_root, mut watch, _) = setup();
        for i in 0..=FILES_LIMIT {
            let file = watch
                .directory
                .join(format!("{}-1-{i:032x}.jsonl", watch.nonce));
            OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(file)
                .unwrap();
        }
        assert_eq!(watch.poll(), Err("too many mutation progress files".into()));
        let (_other, watch, file) = setup();
        let mut out = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&file)
            .unwrap();
        out.write_all(&vec![b'x'; LINE_LIMIT]).unwrap();
        let mut journal = Journal::open(&file, &watch.nonce).unwrap();
        assert_eq!(
            journal.update(&file, Instant::now()),
            Err("mutation progress record exceeds its byte limit".into())
        );
        assert!(journal_name(&format!("{}-1-bad.jsonl", watch.nonce), &watch.nonce).is_err());
    }

    #[test]
    fn unreadable_and_malformed_process_status_cannot_claim_a_dead_child() {
        assert_eq!(
            process_ticks(Ok("not a process stat".into())),
            Err("malformed process status".into())
        );
        let error = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "fixture refusal");
        assert_eq!(
            process_ticks(Err(error)),
            Err("cannot read mutation process status: fixture refusal".into())
        );
    }

    /// `waitid` with `NOWAIT` makes the child a zombie deterministically, with no polling.
    #[test]
    fn an_exited_unreaped_process_can_finish_its_journal() {
        use rustix::process::{Pid, WaitId, WaitIdOptions};
        let mut child = std::process::Command::new("sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        let unreaped = WaitIdOptions::EXITED | WaitIdOptions::NOWAIT;
        let exited = rustix::process::waitid(WaitId::Pid(Pid::from_child(&child)), unreaped);
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", child.id()));
        let result = exited
            .map_err(|error| error.to_string())
            .and(stat.map_err(|error| error.to_string()))
            .and_then(|stat| process_ticks(Ok(stat)))
            .and_then(|ticks| ticks.ok_or_else(|| "fixture child lost its status".to_string()))
            .and_then(|ticks| correlate(child.id(), ticks, "/fixture/exited"));
        let cleanup = child.kill();
        let status = child.wait();
        cleanup.unwrap();
        assert!(status.unwrap().success());
        assert_eq!(result, Ok(()));
    }

    #[test]
    fn malformed_executable_identity_is_refused_but_an_exited_process_can_be_read() {
        let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
        let ticks = stat
            .rsplit_once(')')
            .unwrap()
            .1
            .split_whitespace()
            .nth(19)
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(
            correlate(std::process::id(), ticks, "/wrong/executable"),
            Err("mutation progress executable identity changed".into())
        );
        assert_eq!(correlate(u32::MAX, 1, "/gone"), Ok(()));
    }

    #[test]
    fn active_test_deadlines_do_not_wait_for_the_outer_build_limit() {
        let (_root, mut watch, file) = setup();
        header(&watch, &file);
        append(&watch, &file, 1, "phase", json!({"phase":"evaluation"}));
        append(
            &watch,
            &file,
            2,
            "test_start",
            json!({"phase":"evaluation","invocation_id":7,
            "test_name":"stalled test","mutation_ids":[3],"strategy":"isolated","execution_timeout_ms":100,
            "startup_timeout_ms":5000,"cleanup_timeout_ms":10000,"report_timeout_ms":1000,"join_timeout_ms":1000}),
        );
        watch.poll().unwrap();
        watch.started = Instant::now() - Duration::from_secs(20);
        assert!(watch.poll().unwrap_err().contains("invocation 7 exceeded"));
    }
}

use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

/// Byte bounds per record and per journal file, which a header must declare and the watch enforces.
pub(super) const LINE_LIMIT: usize = 16_384;
pub(super) const FILE_LIMIT: u64 = 67_108_864;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Header {
    pub(crate) pid: u32,
    pub(crate) start_ticks: u64,
    pub(crate) exe: String,
    pub(crate) instance_id: String,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Reference,
    Evaluation,
    Simulation,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Strategy {
    Isolated,
    InProcess,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessStart {
    kind: String,
    ticks: u64,
}

struct Row {
    schema: String,
    version: u32,
    nonce: String,
    instance_id: String,
    seq: u64,
    elapsed_ms: u64,
    event: Event,
}

impl Row {
    #[inline(never)]
    fn parse(line: &str) -> Result<Self, String> {
        require(
            line.trim_start().starts_with('{'),
            "expected a progress object",
        )?;
        let mut wire: WireRow = serde_json::from_str(line).map_err(|error| error.to_string())?;
        let event = wire.take_event()?;
        if let Some(field) = wire.unused_field() {
            return Err(format!("unknown field `{field}`"));
        }
        Ok(Self {
            schema: wire.schema,
            version: wire.version,
            nonce: wire.nonce,
            instance_id: wire.instance_id,
            seq: wire.seq,
            elapsed_ms: wire.elapsed_ms,
            event,
        })
    }
}

// A present null must not become an absent field, including on the wrong event variant.
#[derive(Default)]
enum Field<T> {
    #[default]
    Missing,
    Present(T),
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Field<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        T::deserialize(deserializer).map(Self::Present)
    }
}

impl<T> Field<T> {
    fn take(&mut self, name: &str) -> Result<T, String> {
        match std::mem::take(self) {
            Self::Present(value) => Ok(value),
            Self::Missing => Err(missing_field(name)),
        }
    }

    fn is_present(&self) -> bool {
        matches!(self, Self::Present(_))
    }
}

#[cold]
#[inline(never)]
fn missing_field(name: &str) -> String {
    format!("missing field `{name}`")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRow {
    schema: String,
    version: u32,
    nonce: String,
    instance_id: String,
    seq: u64,
    elapsed_ms: u64,
    event: String,
    #[serde(default)]
    pid: Field<u32>,
    #[serde(default)]
    process_start: Field<ProcessStart>,
    #[serde(default)]
    exe: Field<String>,
    #[serde(default)]
    record_limit_bytes: Field<u64>,
    #[serde(default)]
    file_limit_bytes: Field<u64>,
    #[serde(default)]
    phase: Field<Phase>,
    #[serde(default)]
    invocation_id: Field<u64>,
    #[serde(default)]
    test_name: Field<String>,
    #[serde(default)]
    mutation_ids: Field<Vec<u64>>,
    #[serde(default)]
    strategy: Field<Strategy>,
    #[serde(default)]
    execution_timeout_ms: Field<Option<u64>>,
    #[serde(default)]
    startup_timeout_ms: Field<u64>,
    #[serde(default)]
    cleanup_timeout_ms: Field<u64>,
    #[serde(default)]
    report_timeout_ms: Field<u64>,
    #[serde(default)]
    join_timeout_ms: Field<u64>,
    #[serde(default)]
    result: Field<String>,
    #[serde(default)]
    cleanup: Field<String>,
    #[serde(default)]
    status: Field<String>,
    #[serde(default)]
    exit_code: Field<i64>,
}

impl WireRow {
    #[inline(never)]
    fn take_event(&mut self) -> Result<Event, String> {
        Ok(match self.event.as_str() {
            "header" => Event::Header(HeaderEvent {
                pid: self.pid.take("pid")?,
                process_start: self.process_start.take("process_start")?,
                exe: self.exe.take("exe")?,
                record_limit_bytes: self.record_limit_bytes.take("record_limit_bytes")?,
                file_limit_bytes: self.file_limit_bytes.take("file_limit_bytes")?,
            }),
            "phase" => Event::Phase {
                phase: self.phase.take("phase")?,
            },
            "test_start" => Event::TestStart(InvocationStart {
                phase: self.phase.take("phase")?,
                invocation_id: self.invocation_id.take("invocation_id")?,
                test_name: self.test_name.take("test_name")?,
                mutation_ids: self.mutation_ids.take("mutation_ids")?,
                strategy: self.strategy.take("strategy")?,
                execution_timeout_ms: self.execution_timeout_ms.take("execution_timeout_ms")?,
                startup_timeout_ms: self.startup_timeout_ms.take("startup_timeout_ms")?,
                cleanup_timeout_ms: self.cleanup_timeout_ms.take("cleanup_timeout_ms")?,
                report_timeout_ms: self.report_timeout_ms.take("report_timeout_ms")?,
                join_timeout_ms: self.join_timeout_ms.take("join_timeout_ms")?,
            }),
            "test_end" => Event::TestEnd {
                invocation_id: self.invocation_id.take("invocation_id")?,
                result: self.result.take("result")?,
                cleanup: self.cleanup.take("cleanup")?,
            },
            "terminal" => Event::Terminal {
                status: self.status.take("status")?,
                exit_code: self.exit_code.take("exit_code")?,
            },
            other => return Err(format!("unknown variant `{other}`")),
        })
    }

    fn unused_field(&self) -> Option<&'static str> {
        [
            ("pid", self.pid.is_present()),
            ("process_start", self.process_start.is_present()),
            ("exe", self.exe.is_present()),
            ("record_limit_bytes", self.record_limit_bytes.is_present()),
            ("file_limit_bytes", self.file_limit_bytes.is_present()),
            ("phase", self.phase.is_present()),
            ("invocation_id", self.invocation_id.is_present()),
            ("test_name", self.test_name.is_present()),
            ("mutation_ids", self.mutation_ids.is_present()),
            ("strategy", self.strategy.is_present()),
            (
                "execution_timeout_ms",
                self.execution_timeout_ms.is_present(),
            ),
            ("startup_timeout_ms", self.startup_timeout_ms.is_present()),
            ("cleanup_timeout_ms", self.cleanup_timeout_ms.is_present()),
            ("report_timeout_ms", self.report_timeout_ms.is_present()),
            ("join_timeout_ms", self.join_timeout_ms.is_present()),
            ("result", self.result.is_present()),
            ("cleanup", self.cleanup.is_present()),
            ("status", self.status.is_present()),
            ("exit_code", self.exit_code.is_present()),
        ]
        .into_iter()
        .find_map(|(name, present)| present.then_some(name))
    }
}

enum Event {
    Header(HeaderEvent),
    Phase {
        phase: Phase,
    },
    TestStart(InvocationStart),
    TestEnd {
        invocation_id: u64,
        result: String,
        cleanup: String,
    },
    Terminal {
        status: String,
        exit_code: i64,
    },
}

struct HeaderEvent {
    pid: u32,
    process_start: ProcessStart,
    exe: String,
    record_limit_bytes: u64,
    file_limit_bytes: u64,
}

struct InvocationStart {
    phase: Phase,
    invocation_id: u64,
    test_name: String,
    mutation_ids: Vec<u64>,
    strategy: Strategy,
    execution_timeout_ms: Option<u64>,
    startup_timeout_ms: u64,
    cleanup_timeout_ms: u64,
    report_timeout_ms: u64,
    join_timeout_ms: u64,
}

impl InvocationStart {
    fn deadline(
        &self,
        anchor: Duration,
        elapsed_ms: u64,
    ) -> Result<Option<(u64, Duration)>, String> {
        let stages = [
            self.startup_timeout_ms,
            self.cleanup_timeout_ms,
            self.report_timeout_ms,
            self.join_timeout_ms,
        ];
        let expected = if self.strategy == Strategy::Isolated {
            [5_000, 10_000, 1_000, 1_000]
        } else {
            [0; 4]
        };
        require(stages == expected, "invalid invocation stage budgets")?;
        self.execution_timeout_ms
            .map(|timeout| {
                stages
                    .into_iter()
                    .chain([1_000, elapsed_ms])
                    .try_fold(timeout, u64::checked_add)
                    .and_then(|ms| {
                        anchor
                            .checked_add(Duration::from_millis(ms))
                            .map(|end| (ms, end))
                    })
                    .ok_or_else(|| "invocation deadline overflow".to_owned())
            })
            .transpose()
    }
}

struct Active {
    test_name: String,
    mutation_ids: Vec<u64>,
    deadline: Option<(u64, Duration)>,
}

impl Active {
    fn failure(&self, id: u64, reason: &str) -> String {
        format!(
            "invocation {id} {reason}; test={:?}; mutation_ids={:?}",
            self.test_name, self.mutation_ids
        )
    }
}

pub(crate) struct Progress {
    nonce: String,
    header: Option<Header>,
    anchor: Duration,
    observed: Duration,
    next_seq: u64,
    elapsed_ms: u64,
    phase: Option<Phase>,
    seen: BTreeSet<u64>,
    active: BTreeMap<u64, Active>,
    terminal: bool,
    error: Option<String>,
}

impl Progress {
    pub(crate) fn new(nonce: String) -> Self {
        Self {
            nonce,
            header: None,
            anchor: Duration::ZERO,
            observed: Duration::ZERO,
            next_seq: 0,
            elapsed_ms: 0,
            phase: None,
            seen: BTreeSet::new(),
            active: BTreeMap::new(),
            terminal: false,
            error: None,
        }
    }

    pub(crate) fn header(&self) -> Option<&Header> {
        self.header.as_ref()
    }

    pub(crate) fn ingest(&mut self, line: &str, observed: Duration) -> Result<(), String> {
        let result = self.advance(line, observed);
        if let Err(error) = &result {
            self.error = Some(error.clone());
        }
        result
    }

    pub(crate) fn check(&self, now: Duration) -> Result<(), String> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        require(now >= self.observed, "observation time moved backwards")?;
        let expired = self
            .active
            .iter()
            .filter_map(|(id, active)| active.deadline.map(|(_, end)| (end, *id, active)))
            .min_by_key(|(end, id, _)| (*end, *id));
        if let Some((deadline, id, active)) = expired
            && now >= deadline
        {
            return Err(active.failure(id, "exceeded its deadline"));
        }
        Ok(())
    }

    pub(crate) fn complete(&self) -> Result<(), String> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        require(self.terminal, "missing completed terminal event")
    }

    fn advance(&mut self, line: &str, observed: Duration) -> Result<(), String> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        require(
            observed >= self.observed,
            "observation time moved backwards",
        )?;
        require(!self.terminal, "event after terminal")?;
        let row = Row::parse(line).map_err(|error| format!("invalid progress row: {error}"))?;
        self.validate_row(&row)?;
        self.refine_anchor(observed, row.elapsed_ms)?;
        match row.event {
            Event::Header(header) => self.bind_header(header, row.instance_id, observed)?,
            Event::Phase { phase } => self.change_phase(phase)?,
            Event::TestStart(start) => self.begin(start, row.elapsed_ms)?,
            Event::TestEnd {
                invocation_id,
                result,
                cleanup,
            } => self.finish(invocation_id, &result, &cleanup, row.elapsed_ms)?,
            Event::Terminal { status, exit_code } => self.terminate(&status, exit_code)?,
        }
        self.next_seq = row.seq.checked_add(1).ok_or("progress sequence overflow")?;
        self.elapsed_ms = row.elapsed_ms;
        self.observed = observed;
        Ok(())
    }

    fn refine_anchor(&mut self, observed: Duration, elapsed_ms: u64) -> Result<(), String> {
        let anchor = self
            .anchor
            .min(observed.saturating_sub(Duration::from_millis(elapsed_ms)));
        if anchor == self.anchor {
            return Ok(());
        }
        self.anchor = anchor;
        for active in self.active.values_mut() {
            if let Some((millis, end)) = &mut active.deadline {
                *end = anchor
                    .checked_add(Duration::from_millis(*millis))
                    .ok_or("invocation deadline overflow")?;
            }
        }
        Ok(())
    }

    fn validate_row(&self, row: &Row) -> Result<(), String> {
        require(
            row.schema == "mutest-progress" && row.version == 1,
            "unsupported progress schema or version",
        )?;
        require(
            hex_id(&row.nonce) && row.nonce == self.nonce && hex_id(&row.instance_id),
            "invalid progress nonce or instance identity",
        )?;
        require(
            row.seq == self.next_seq && row.elapsed_ms >= self.elapsed_ms,
            "nonmonotonic progress sequence or elapsed time",
        )?;
        if let Some(header) = &self.header {
            require(
                row.instance_id == header.instance_id,
                "progress instance identity changed",
            )
        } else {
            require(
                matches!(row.event, Event::Header(_)) && row.elapsed_ms == 0,
                "first progress row must be a zero-time header",
            )
        }
    }

    fn bind_header(
        &mut self,
        header: HeaderEvent,
        instance_id: String,
        observed: Duration,
    ) -> Result<(), String> {
        require(self.header.is_none(), "duplicate progress header")?;
        require(
            header.pid > 0 && header.process_start.kind == "linux-proc-starttime",
            "invalid progress process identity",
        )?;
        require(
            header.exe.starts_with('/')
                && header.exe[1..]
                    .split('/')
                    .all(|p| !matches!(p, "" | "." | ".."))
                && !header.exe.contains('\0'),
            "executable path is not canonical-looking",
        )?;
        require(
            u64::try_from(LINE_LIMIT) == Ok(header.record_limit_bytes)
                && header.file_limit_bytes == FILE_LIMIT,
            "invalid progress size limits",
        )?;
        self.header = Some(Header {
            pid: header.pid,
            start_ticks: header.process_start.ticks,
            exe: header.exe,
            instance_id,
        });
        self.anchor = observed;
        Ok(())
    }

    fn change_phase(&mut self, phase: Phase) -> Result<(), String> {
        require(
            self.phase == Some(phase) || self.active.is_empty(),
            "phase changed with active invocations",
        )?;
        self.phase = Some(phase);
        Ok(())
    }

    fn begin(&mut self, start: InvocationStart, elapsed_ms: u64) -> Result<(), String> {
        require(
            self.phase == Some(start.phase) && !start.test_name.is_empty(),
            "invalid test phase or name",
        )?;
        require(
            !self.seen.contains(&start.invocation_id),
            "duplicate invocation ID",
        )?;
        require(
            start.phase != Phase::Reference || start.execution_timeout_ms.is_none(),
            "reference execution timeout must be null",
        )?;
        require(
            start
                .mutation_ids
                .iter()
                .all(|id| u32::try_from(*id).is_ok()),
            "invalid mutation identity",
        )?;
        require(
            start.phase != Phase::Reference || start.mutation_ids.is_empty(),
            "reference test names mutations",
        )?;
        let deadline = start.deadline(self.anchor, elapsed_ms)?;
        self.seen.insert(start.invocation_id);
        self.active.insert(
            start.invocation_id,
            Active {
                test_name: start.test_name,
                mutation_ids: start.mutation_ids,
                deadline,
            },
        );
        Ok(())
    }

    fn finish(
        &mut self,
        id: u64,
        result: &str,
        cleanup: &str,
        elapsed_ms: u64,
    ) -> Result<(), String> {
        require(
            matches!(
                result,
                "ok" | "failed" | "crashed" | "timed_out" | "ignored"
            ),
            "unknown invocation result",
        )?;
        require(
            matches!(cleanup, "complete" | "pending"),
            "unknown invocation cleanup status",
        )?;
        let active = self
            .active
            .get(&id)
            .ok_or("test_end without an active invocation")?;
        require(cleanup == "complete", "incomplete invocation or cleanup")?;
        if active.deadline.is_some_and(|(end, _)| elapsed_ms >= end) {
            return Err(active.failure(id, "ended after its deadline"));
        }
        self.active.remove(&id);
        Ok(())
    }

    fn terminate(&mut self, status: &str, exit_code: i64) -> Result<(), String> {
        require(
            matches!(status, "completed" | "incomplete"),
            "unknown terminal status",
        )?;
        require(
            status == "completed" && self.active.is_empty(),
            "incomplete terminal or active invocation",
        )?;
        require(
            matches!(exit_code, 0 | 2 | 3),
            "terminal reports an abnormal exit",
        )?;
        self.terminal = true;
        Ok(())
    }
}

#[inline(never)]
fn require(condition: bool, message: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message.to_owned())
    }
}

/// A 32-digit lowercase hex id, spelled as the run's nonce is, so one identity has one spelling.
pub(super) fn hex_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "a failed fixture decode is the test failing"
)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    const NONCE: &str = "0123456789abcdef0123456789abcdef";
    const INSTANCE: &str = "abcdef0123456789abcdef0123456789";

    fn row(event: &str, seq: u64, elapsed: u64, extra: Value) -> Value {
        let mut value = json!({"schema":"mutest-progress", "version":1, "event":event,
            "nonce":NONCE, "instance_id":INSTANCE, "seq":seq, "elapsed_ms":elapsed});
        value
            .as_object_mut()
            .unwrap()
            .extend(serde_json::from_value::<serde_json::Map<String, Value>>(extra).unwrap());
        value
    }

    fn ingest(
        progress: &mut Progress,
        row: impl std::borrow::Borrow<Value>,
        observed: u64,
    ) -> Result<(), String> {
        progress.ingest(&row.borrow().to_string(), Duration::from_millis(observed))
    }

    fn ready(phase: &str) -> Progress {
        begun(phase, 0, 100)
    }

    /// A run past its header and into `phase`, which began `at` ms in, both read at `observed` ms.
    fn begun(phase: &str, at: u64, observed: u64) -> Progress {
        let mut progress = Progress::new(NONCE.to_owned());
        ingest(&mut progress, header(), observed).unwrap();
        let begins = row("phase", 1, at, json!({ "phase": phase }));
        ingest(&mut progress, begins, observed).unwrap();
        progress
    }

    /// What a fresh run makes of a header whose `field` holds `value`.
    fn opened_with(field: &str, value: Value) -> Result<(), String> {
        let mut next = header();
        next[field] = value;
        ingest(&mut Progress::new(NONCE.into()), next, 100)
    }

    fn header() -> Value {
        row(
            "header",
            0,
            0,
            json!({"pid":42,"process_start":{"kind":"linux-proc-starttime","ticks":123},
            "exe":"/tmp/harness", "record_limit_bytes":16384,"file_limit_bytes":67108864}),
        )
    }

    fn start(seq: u64, elapsed: u64, id: u64, phase: &str, timeout: Option<u64>) -> Value {
        row(
            "test_start",
            seq,
            elapsed,
            json!({"phase":phase,"invocation_id":id,"test_name":"a test",
            "mutation_ids":if phase == "reference" { vec![] } else { vec![1] },"strategy":"isolated","execution_timeout_ms":timeout,
            "startup_timeout_ms":5000,"cleanup_timeout_ms":10000,"report_timeout_ms":1000,"join_timeout_ms":1000}),
        )
    }

    fn end(seq: u64, elapsed: u64, id: u64, result: &str, cleanup: &str) -> Value {
        row(
            "test_end",
            seq,
            elapsed,
            json!({"invocation_id":id,"result":result,"cleanup":cleanup}),
        )
    }

    fn terminal(seq: u64, elapsed: u64, status: &str, code: i64) -> Value {
        row(
            "terminal",
            seq,
            elapsed,
            json!({"status":status,"exit_code":code}),
        )
    }

    #[test]
    fn completed_terminals_reject_abnormal_exit_codes() {
        for code in [i64::MIN, -1, 1, 4, i64::MAX] {
            let mut progress = ready("evaluation");
            let expected = Err("terminal reports an abnormal exit".into());
            assert_eq!(
                ingest(&mut progress, terminal(2, 0, "completed", code), 100),
                expected,
                "exit code {code}"
            );
            assert_eq!(progress.complete(), expected);
        }
    }

    #[test]
    fn completed_runs_accept_success_and_measured_failure_exit_codes() {
        for (code, result) in [(0, "ok"), (2, "failed"), (3, "timed_out")] {
            let mut progress = ready("evaluation");
            ingest(&mut progress, start(2, 10, 7, "evaluation", Some(100)), 110).unwrap();
            ingest(&mut progress, end(3, 20, 7, result, "complete"), 120).unwrap();
            ingest(&mut progress, terminal(4, 30, "completed", code), 130).unwrap();
            assert_eq!(progress.complete(), Ok(()));
            assert_eq!(
                progress.header(),
                Some(&Header {
                    pid: 42,
                    start_ticks: 123,
                    exe: "/tmp/harness".into(),
                    instance_id: INSTANCE.into()
                })
            );
        }
    }

    #[test]
    fn reference_tests_have_no_deadline_and_can_transition_to_evaluation() {
        let mut progress = ready("reference");
        ingest(&mut progress, start(2, 10, 1, "reference", None), 110).unwrap();
        assert_eq!(progress.check(Duration::from_secs(1_000)), Ok(()));
        ingest(
            &mut progress,
            end(3, 1_000_000, 1, "ok", "complete"),
            1_000_100,
        )
        .unwrap();
        ingest(
            &mut progress,
            row("phase", 4, 1_000_000, json!({"phase":"evaluation"})),
            1_000_100,
        )
        .unwrap();
        ingest(
            &mut progress,
            start(5, 1_000_001, 2, "evaluation", Some(10)),
            1_000_101,
        )
        .unwrap();
        ingest(
            &mut progress,
            end(6, 1_000_002, 2, "ok", "complete"),
            1_000_102,
        )
        .unwrap();
        ingest(
            &mut progress,
            terminal(7, 1_000_003, "completed", 0),
            1_000_103,
        )
        .unwrap();
        assert_eq!(progress.complete(), Ok(()));
    }

    #[test]
    fn unrelated_activity_does_not_extend_an_active_deadline() {
        let mut progress = ready("evaluation");
        ingest(&mut progress, start(2, 10, 1, "evaluation", Some(100)), 110).unwrap();
        ingest(
            &mut progress,
            start(3, 1_000, 2, "evaluation", Some(100)),
            1_100,
        )
        .unwrap();
        ingest(&mut progress, end(4, 17_999, 2, "ok", "complete"), 18_099).unwrap();
        assert_eq!(progress.check(Duration::from_millis(18_209)), Ok(()));
        assert_eq!(
            progress.check(Duration::from_millis(18_210)),
            Err("invocation 1 exceeded its deadline; test=\"a test\"; mutation_ids=[1]".into())
        );
        assert_eq!(
            ingest(&mut progress, end(5, 18_110, 1, "ok", "complete"), 18_210),
            Err("invocation 1 ended after its deadline; test=\"a test\"; mutation_ids=[1]".into())
        );
        assert_eq!(
            progress.complete(),
            Err("invocation 1 ended after its deadline; test=\"a test\"; mutation_ids=[1]".into())
        );
    }

    #[test]
    fn duplicate_fields_are_rejected_including_nested_process_fields() {
        let original = header().to_string();
        for duplicate in [
            original.replacen("\"seq\":0", "\"seq\":0,\"seq\":0", 1),
            original.replacen("\"ticks\":123", "\"ticks\":123,\"ticks\":123", 1),
            original.replacen(
                "\"event\":\"header\"",
                "\"event\":\"header\",\"event\":\"header\"",
                1,
            ),
        ] {
            let error = Progress::new(NONCE.into())
                .ingest(&duplicate, Duration::ZERO)
                .unwrap_err();
            assert!(error.contains("duplicate field"), "{error}");
        }
    }

    #[test]
    fn sequence_time_and_identity_must_remain_consistent() {
        for (field, value, expected) in [
            ("seq", json!(1), "nonmonotonic"),
            ("seq", json!(3), "nonmonotonic"),
            ("nonce", json!(INSTANCE), "nonce"),
            ("instance_id", json!(NONCE), "identity changed"),
            ("nonce", json!("ABCDEF0123456789abcdef0123456789"), "nonce"),
        ] {
            let mut progress = ready("evaluation");
            let mut next = start(2, 10, 1, "evaluation", Some(100));
            next[field] = value;
            let error = ingest(&mut progress, next, 110).unwrap_err();
            assert!(error.contains(expected), "{error}");
        }
        let mut progress = ready("evaluation");
        ingest(&mut progress, start(2, 10, 1, "evaluation", Some(100)), 110).unwrap();
        assert_eq!(
            ingest(&mut progress, end(3, 9, 1, "ok", "complete"), 110),
            Err("nonmonotonic progress sequence or elapsed time".into())
        );
    }

    #[test]
    fn unfinished_and_unknown_results_cannot_become_completed_runs() {
        for (result, cleanup, expected) in [
            ("ok", "pending", "incomplete invocation or cleanup"),
            ("ok", "incomplete", "unknown invocation cleanup status"),
            ("ok", "not_required", "unknown invocation cleanup status"),
            ("incomplete", "complete", "unknown invocation result"),
            ("unknown", "complete", "unknown invocation result"),
            ("ok", "unknown", "unknown invocation cleanup status"),
        ] {
            let mut progress = ready("evaluation");
            ingest(&mut progress, start(2, 10, 1, "evaluation", Some(100)), 110).unwrap();
            assert_eq!(
                ingest(&mut progress, end(3, 20, 1, result, cleanup), 120),
                Err(expected.into())
            );
            assert_eq!(
                ingest(&mut progress, terminal(4, 30, "completed", 0), 130),
                Err(expected.into())
            );
        }
        for (status, expected) in [
            ("unknown", "unknown terminal status"),
            ("incomplete", "incomplete terminal or active invocation"),
        ] {
            assert_eq!(
                ingest(&mut ready("evaluation"), terminal(2, 0, status, 0), 100),
                Err(expected.into())
            );
        }
        assert_eq!(
            ready("evaluation").complete(),
            Err("missing completed terminal event".into())
        );
    }

    #[test]
    fn duplicate_invocations_and_inconsistent_phases_are_rejected() {
        for next in [
            start(3, 10, 1, "evaluation", Some(100)),
            row("phase", 3, 10, json!({"phase":"simulation"})),
            start(3, 10, 2, "reference", None),
            terminal(3, 10, "completed", 0),
        ] {
            let mut progress = ready("evaluation");
            ingest(&mut progress, start(2, 10, 1, "evaluation", Some(100)), 110).unwrap();
            let error = ingest(&mut progress, next, 110).unwrap_err();
            assert!(
                error.contains("duplicate") || error.contains("phase") || error.contains("active"),
                "{error}"
            );
        }
        let mut progress = ready("evaluation");
        ingest(&mut progress, start(2, 0, 1, "evaluation", Some(100)), 100).unwrap();
        ingest(&mut progress, end(3, 0, 1, "ok", "complete"), 100).unwrap();
        assert_eq!(
            ingest(&mut progress, start(4, 0, 1, "evaluation", Some(100)), 100),
            Err("duplicate invocation ID".into())
        );
    }

    #[test]
    fn buffered_timely_completion_is_valid_after_wall_deadlines() {
        let mut progress = ready("simulation");
        ingest(
            &mut progress,
            start(2, 10, 1, "simulation", Some(100)),
            90_000,
        )
        .unwrap();
        ingest(
            &mut progress,
            row("phase", 3, 11, json!({"phase":"simulation"})),
            90_000,
        )
        .unwrap();
        ingest(&mut progress, end(4, 20, 1, "ignored", "complete"), 90_000).unwrap();
        ingest(&mut progress, terminal(5, 30, "completed", 0), 90_000).unwrap();
        assert_eq!(progress.check(Duration::from_millis(90_000)), Ok(()));
        assert_eq!(progress.complete(), Ok(()));
    }

    #[test]
    fn in_process_tests_require_zero_stages_and_complete_cleanup() {
        let mut progress = ready("evaluation");
        let mut next = start(2, 0, 1, "evaluation", Some(100));
        next["strategy"] = json!("in_process");
        for field in [
            "startup_timeout_ms",
            "cleanup_timeout_ms",
            "report_timeout_ms",
            "join_timeout_ms",
        ] {
            next[field] = json!(0);
        }
        ingest(&mut progress, next, 100).unwrap();
        assert_eq!(
            progress.check(Duration::from_millis(1_200)),
            Err("invocation 1 exceeded its deadline; test=\"a test\"; mutation_ids=[1]".into())
        );
        ingest(&mut progress, end(3, 20, 1, "ok", "complete"), 120).unwrap();
        ingest(&mut progress, terminal(4, 30, "completed", 0), 130).unwrap();
        assert_eq!(progress.complete(), Ok(()));
    }

    #[test]
    fn schemas_and_versions_must_match_the_wire_contract() {
        for (field, value) in [("schema", json!("other")), ("version", json!(2))] {
            let why = "unsupported progress schema or version";
            assert_eq!(opened_with(field, value), Err(why.into()));
        }
    }

    #[test]
    fn the_first_row_must_be_a_zero_time_header() {
        let why = Err("first progress row must be a zero-time header".into());
        assert_eq!(opened_with("elapsed_ms", json!(1)), why);
        let phase = row("phase", 0, 0, json!({"phase":"reference"}));
        assert_eq!(ingest(&mut Progress::new(NONCE.into()), phase, 100), why);
    }

    #[test]
    fn headers_require_positive_process_ids_and_linux_start_times() {
        for (field, value) in [
            ("pid", json!(0)),
            ("process_start", json!({"kind":"unknown","ticks":123})),
        ] {
            let why = "invalid progress process identity";
            assert_eq!(opened_with(field, value), Err(why.into()));
        }
    }

    #[test]
    fn executable_paths_must_look_canonical() {
        for path in [
            "relative",
            "/",
            "/tmp//harness",
            "/tmp/./harness",
            "/tmp/../harness",
            "/tmp/harness/",
            "/tmp/ha\0rness",
        ] {
            let why = "executable path is not canonical-looking";
            assert_eq!(opened_with("exe", json!(path)), Err(why.into()));
        }
    }

    #[test]
    fn size_limits_must_match_the_protocol_limits() {
        for field in ["record_limit_bytes", "file_limit_bytes"] {
            let why = "invalid progress size limits";
            assert_eq!(opened_with(field, json!(1)), Err(why.into()));
        }
    }

    #[test]
    fn observations_cannot_move_backwards() {
        let mut progress = ready("evaluation");
        assert_eq!(
            progress.check(Duration::from_millis(99)),
            Err("observation time moved backwards".into())
        );
        assert_eq!(
            ingest(&mut progress, terminal(2, 0, "completed", 0), 99),
            Err("observation time moved backwards".into())
        );
    }

    #[test]
    fn checks_preserve_the_original_ingest_failure() {
        let mut progress = ready("evaluation");
        let error = ingest(&mut progress, terminal(2, 0, "unknown", 0), 100);
        assert_eq!(error, Err("unknown terminal status".into()));
        assert_eq!(progress.check(Duration::from_millis(101)), error);
    }

    #[test]
    fn reference_tests_cannot_declare_execution_timeouts() {
        assert_eq!(
            ingest(
                &mut ready("reference"),
                start(2, 0, 1, "reference", Some(10)),
                100
            ),
            Err("reference execution timeout must be null".into())
        );
    }

    #[test]
    fn reference_tests_cannot_name_mutations() {
        let mut next = start(2, 0, 1, "reference", None);
        next["mutation_ids"] = json!([1]);
        assert_eq!(
            ingest(&mut ready("reference"), next, 100),
            Err("reference test names mutations".into())
        );
    }

    #[test]
    fn flattened_events_reject_duplicate_payload_fields_and_unknown_fields() {
        let original = header().to_string();
        for text in [
            original.replacen("\"pid\":42", "\"pid\":42,\"pid\":42", 1),
            original.replacen("\"ticks\":123", "\"extra\":1,\"ticks\":123", 1),
            original.replacen("\"pid\":42", "\"extra\":1,\"pid\":42", 1),
        ] {
            let error = Progress::new(NONCE.into())
                .ingest(&text, Duration::ZERO)
                .unwrap_err();
            assert!(
                error.contains("duplicate field") || error.contains("unknown field"),
                "{error}"
            );
        }
        for next in [
            start(2, 0, 1, "evaluation", None),
            end(2, 0, 1, "ok", "complete"),
            row("phase", 2, 0, json!({"phase":"evaluation"})),
            terminal(2, 0, "completed", 0),
        ] {
            let mut unknown = next.clone();
            unknown["extra"] = json!(1);
            let error = ingest(&mut ready("evaluation"), unknown, 100).unwrap_err();
            assert!(error.contains("unknown field"), "{error}");
            let field = if next.get("invocation_id").is_some() {
                "invocation_id"
            } else if next.get("phase").is_some() {
                "phase"
            } else {
                "status"
            };
            let text = format!("{{\"{field}\":{},{}", next[field], &next.to_string()[1..]);
            let error = ready("evaluation")
                .ingest(&text, Duration::from_millis(100))
                .unwrap_err();
            assert!(error.contains("duplicate field"), "{error}");
        }
    }

    #[test]
    fn wire_rows_require_every_field_and_reject_nonnullable_nulls() {
        for next in [
            header(),
            row("phase", 1, 0, json!({"phase":"evaluation"})),
            start(2, 0, 1, "evaluation", None),
            end(3, 0, 1, "ok", "complete"),
            terminal(4, 0, "completed", 0),
        ] {
            for field in next.as_object().unwrap().keys() {
                let mut missing = next.clone();
                missing.as_object_mut().unwrap().remove(field);
                let error = Row::parse(&missing.to_string()).err().unwrap();
                assert!(
                    error.contains(&format!("missing field `{field}`")),
                    "{error}"
                );
                let mut null = next.clone();
                null[field] = Value::Null;
                assert_eq!(
                    Row::parse(&null.to_string()).is_ok(),
                    field == "execution_timeout_ms",
                    "nullable field {field}"
                );
                let text = format!("{{\"{field}\":{},{}", next[field], &next.to_string()[1..]);
                let error = Row::parse(&text).err().unwrap();
                assert!(error.contains("duplicate field"), "{field}: {error}");
            }
        }
    }

    #[test]
    fn wire_rows_reject_fields_from_other_variants_including_present_nulls() {
        let rows = [
            header(),
            row("phase", 1, 0, json!({"phase":"evaluation"})),
            start(2, 0, 1, "evaluation", None),
            end(3, 0, 1, "ok", "complete"),
            terminal(4, 0, "completed", 0),
        ];
        let mut fields = serde_json::Map::new();
        for next in &rows {
            fields.extend(next.as_object().unwrap().clone());
        }
        for next in rows {
            for (field, value) in &fields {
                if next.get(field).is_some() {
                    continue;
                }
                let mut extra = next.clone();
                extra[field] = value.clone();
                let error = Row::parse(&extra.to_string()).err().unwrap();
                assert!(
                    error.contains(&format!("unknown field `{field}`")),
                    "{error}"
                );
                extra[field] = Value::Null;
                assert!(
                    Row::parse(&extra.to_string()).is_err(),
                    "unused null {field}"
                );
            }
        }
    }

    #[test]
    fn an_unknown_event_never_becomes_a_valid_progress_row() {
        let mut unknown = header();
        unknown["event"] = json!("unknown-event");
        assert_eq!(
            Row::parse(&unknown.to_string()).err().unwrap(),
            "unknown variant `unknown-event`"
        );
    }

    #[test]
    fn wire_rows_require_objects_and_accept_the_discriminator_last() {
        let positional = json!([
            "mutest-progress", 1, NONCE, INSTANCE, 0, 0, "header", 42,
            {"kind":"linux-proc-starttime","ticks":123}, "/tmp/harness", 16384, 67108864
        ]);
        assert_eq!(
            Row::parse(&positional.to_string()).err().unwrap(),
            "expected a progress object"
        );
        for mut next in [
            header(),
            row("phase", 1, 0, json!({"phase":"evaluation"})),
            start(2, 0, 1, "evaluation", None),
            end(3, 0, 1, "ok", "complete"),
            terminal(4, 0, "completed", 0),
        ] {
            let event = next.as_object_mut().unwrap().remove("event").unwrap();
            let mut text = next.to_string();
            text.pop();
            text.push_str(&format!(",\"event\":{event}}}"));
            assert!(Row::parse(&text).is_ok(), "{text}");
        }
    }

    #[test]
    fn nullable_nonreference_timeouts_leave_deadlines_to_the_outer_budget() {
        for phase in ["evaluation", "simulation"] {
            let mut progress = ready(phase);
            ingest(&mut progress, start(2, 0, 1, phase, None), 100).unwrap();
            assert_eq!(progress.check(Duration::from_secs(1_000)), Ok(()));
            ingest(
                &mut progress,
                end(3, 1_000_000, 1, "ok", "complete"),
                1_000_100,
            )
            .unwrap();
            ingest(
                &mut progress,
                terminal(4, 1_000_000, "completed", 0),
                1_000_100,
            )
            .unwrap();
            assert_eq!(progress.complete(), Ok(()));
        }
    }

    #[test]
    fn delayed_headers_do_not_double_count_elapsed_time() {
        let mut progress = begun("evaluation", 60_000, 60_000);
        ingest(
            &mut progress,
            start(2, 60_000, 1, "evaluation", Some(100)),
            60_000,
        )
        .unwrap();
        assert_eq!(progress.check(Duration::from_millis(78_099)), Ok(()));
        assert_eq!(
            progress.check(Duration::from_millis(78_100)),
            Err("invocation 1 exceeded its deadline; test=\"a test\"; mutation_ids=[1]".into())
        );
    }

    #[test]
    fn anchor_refinement_only_shortens_existing_deadlines() {
        let mut progress = begun("evaluation", 0, 60_000);
        ingest(
            &mut progress,
            start(2, 0, 1, "evaluation", Some(60_000)),
            60_000,
        )
        .unwrap();
        ingest(
            &mut progress,
            start(3, 60_000, 2, "evaluation", Some(100)),
            61_000,
        )
        .unwrap();
        assert_eq!(progress.check(Duration::from_millis(78_999)), Ok(()));
        assert_eq!(
            progress.check(Duration::from_millis(79_000)),
            Err("invocation 1 exceeded its deadline; test=\"a test\"; mutation_ids=[1]".into())
        );
    }

    #[test]
    fn anchor_refinement_preserves_explicit_outer_budget_invocations() {
        let mut progress = begun("reference", 0, 1000);
        ingest(&mut progress, start(2, 0, 1, "reference", None), 1000).unwrap();
        ingest(
            &mut progress,
            row("phase", 3, 1000, json!({"phase":"reference"})),
            1000,
        )
        .unwrap();
        assert_eq!(progress.check(Duration::from_secs(10)), Ok(()));
        ingest(&mut progress, end(4, 1000, 1, "ok", "complete"), 1000).unwrap();
        ingest(&mut progress, terminal(5, 1000, "completed", 0), 1000).unwrap();
        assert_eq!(progress.complete(), Ok(()));
    }

    #[test]
    fn stage_budgets_and_required_nullable_timeouts_are_checked() {
        for (field, value) in [
            ("startup_timeout_ms", json!(0)),
            ("execution_timeout_ms", json!(-1)),
            ("mutation_ids", json!([-1])),
            ("mutation_ids", json!([4294967296_u64])),
            ("test_name", json!("")),
            ("strategy", json!("unknown")),
        ] {
            let mut next = start(2, 0, 1, "evaluation", Some(100));
            next[field] = value;
            let error = ingest(&mut ready("evaluation"), next, 100).unwrap_err();
            assert!(
                error.contains("invalid") || error.contains("execution timeout"),
                "{error}"
            );
        }
        let mut next = start(2, 0, 1, "reference", None);
        next.as_object_mut().unwrap().remove("execution_timeout_ms");
        let error = ingest(&mut ready("reference"), next, 100).unwrap_err();
        assert!(
            error.contains("missing field `execution_timeout_ms`"),
            "{error}"
        );
    }
}

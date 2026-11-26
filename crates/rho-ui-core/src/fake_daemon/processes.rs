//! What the fake daemon's background processes wrote, which
//! `rho/session/processes` reads. A process's state follows its background
//! work's: running, exited with its code, or terminated once stopped. A
//! process is listed, and its output read, while its work is listed: as
//! rho forgets a process once the model was given its result.

use std::{collections::BTreeMap, time::Instant};

use rho_protocol::{
    methods::{BackgroundItem, BackgroundStatus, BackgroundWork},
    ProcessChunk, ProcessOutput, ProcessState, ProcessStream, SessionProcess,
    SessionProcessesParams, SessionProcessesResponse,
};

/// One process: what its work last said, its output, and its times.
#[derive(Debug)]
struct Record {
    command: String,
    state: ProcessState,
    exit_code: Option<i32>,
    started: Instant,
    /// When its work ended, which stops its clock.
    ended: Option<Instant>,
    last_output: Option<Instant>,
    chunks: Vec<ProcessChunk>,
}

/// A session's processes, by ID.
#[derive(Debug, Default)]
pub(super) struct FakeProcesses(BTreeMap<String, Record>);

impl FakeProcesses {
    /// Process `id` started now, running `command`, having written nothing.
    pub(super) fn start(&mut self, id: &str, command: &str) {
        self.0.insert(
            id.to_owned(),
            Record {
                command: command.to_owned(),
                state: ProcessState::Running,
                exit_code: None,
                started: Instant::now(),
                ended: None,
                last_output: None,
                chunks: Vec::new(),
            },
        );
    }

    /// Process `id` wrote `text` to `stream`, as one chunk.
    ///
    /// # Panics
    ///
    /// If there is no process `id`: the test has a bug.
    pub(super) fn write(&mut self, id: &str, stream: ProcessStream, text: &str) {
        let record = self
            .0
            .get_mut(id)
            .unwrap_or_else(|| panic!("the fake daemon has no process {id}"));
        record.chunks.push(ProcessChunk {
            cursor: cursor(record.chunks.len()),
            stream,
            text: text.to_owned(),
        });
        record.last_output = Some(Instant::now());
    }

    /// Takes each process's state from its work in `work`, the session's
    /// background list, and stops the clock of each that ended.
    pub(super) fn sync(&mut self, work: &[BackgroundWork]) {
        for work in work {
            let BackgroundItem::Process { id } = &work.item else {
                continue;
            };
            let Some(record) = self.0.get_mut(id) else {
                continue;
            };
            record.state = match work.status {
                BackgroundStatus::Completed => ProcessState::Exited,
                BackgroundStatus::Cancelled => ProcessState::Terminated,
                BackgroundStatus::Failed => ProcessState::FailedToStart,
                _ => ProcessState::Running,
            };
            record.exit_code = work.exit_code;
            if !work.status.is_running() {
                record.ended.get_or_insert_with(Instant::now);
            }
        }
    }

    /// Answers `rho/session/processes` for a session whose background work
    /// is `work`.
    pub(super) fn read(
        &self,
        work: &[BackgroundWork],
        params: &SessionProcessesParams,
    ) -> SessionProcessesResponse {
        let processes = work
            .iter()
            .filter_map(|work| match &work.item {
                BackgroundItem::Process { id } => Some(self.0.get(id)?.wire(id)),
                _ => None,
            })
            .collect();
        let listed = |id: &str| {
            work.iter().any(
                |work| matches!(&work.item, BackgroundItem::Process { id: listed } if listed == id),
            )
        };
        let output = params.output.as_ref().and_then(|asked| {
            let record = self
                .0
                .get(&asked.process)
                .filter(|_| listed(&asked.process))?;
            Some(ProcessOutput {
                process: record.wire(&asked.process),
                first_cursor: 0,
                next_cursor: cursor(record.chunks.len()),
                chunks: record
                    .chunks
                    .iter()
                    .filter(|chunk| chunk.cursor >= asked.from)
                    .cloned()
                    .collect(),
            })
        });
        SessionProcessesResponse { processes, output }
    }
}

impl Record {
    fn wire(&self, id: &str) -> SessionProcess {
        let now = self.ended.unwrap_or_else(Instant::now);
        SessionProcess {
            id: id.to_owned(),
            command: self.command.clone(),
            state: self.state,
            elapsed_seconds: now.duration_since(self.started).as_secs(),
            quiet_seconds: self
                .last_output
                .filter(|_| self.ended.is_none())
                .map(|at| at.elapsed().as_secs()),
            exit_code: self.exit_code,
        }
    }
}

/// The cursor of a process's chunk `ix`, its place in the output.
fn cursor(ix: usize) -> u64 {
    u64::try_from(ix).expect("fewer than 2^64 chunks")
}

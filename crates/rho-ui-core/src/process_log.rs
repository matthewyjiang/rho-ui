//! One background process's output as a client follows it: read with
//! [`crate::Daemon::processes`] from [`ProcessLog::request`]'s cursor, each
//! answer given to [`ProcessLog::apply`], which keeps the output as lines.
//!
//! The daemon hands output on as it was read, in chunks that need not end
//! a line, so a line can span chunks; a chunk on the other stream starts a
//! line of its own. A carriage return, as a progress bar writes, starts the
//! line over, as a terminal would show it. The session retains a bounded
//! amount of output: lines the daemon evicted are dropped here too.

use rho_protocol::{ProcessOutput, ProcessOutputParams, ProcessStream, SessionProcess};

/// A line of a process's output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputLine {
    stream: ProcessStream,
    text: String,
    /// The cursor of the newest chunk it holds text of.
    last_cursor: u64,
}

impl OutputLine {
    /// The stream it was written to.
    pub fn stream(&self) -> ProcessStream {
        self.stream
    }

    /// Its text, without the newline.
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// How [`ProcessLog::apply`] changed the lines, for a list that shows them:
/// first `dropped` lines left the front, then the line at `changed` (an
/// index after the drop) took more text, then `added` lines joined the end.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[must_use]
pub struct LogChange {
    /// How many lines left the front.
    pub dropped: usize,
    /// The line, after the drop, that took more text.
    pub changed: Option<usize>,
    /// How many lines joined the end.
    pub added: usize,
}

/// The output of one process, followed from the start of what the session
/// retains.
#[derive(Clone, Debug)]
pub struct ProcessLog {
    /// The process's ID.
    id: String,
    /// The process as the latest answer had it.
    process: Option<SessionProcess>,
    lines: Vec<OutputLine>,
    /// The last line awaits more text: its chunk ended without a newline.
    open: bool,
    /// The open line's text was returned over: its next text replaces it.
    returned: bool,
    /// The cursor to read on from.
    next: u64,
    /// Output older than what the session retains was dropped.
    evicted: bool,
}

impl ProcessLog {
    /// Follows process `id`, which has no output read yet.
    pub fn new(id: String) -> Self {
        Self {
            id,
            process: None,
            lines: Vec::new(),
            open: false,
            returned: false,
            next: 0,
            evicted: false,
        }
    }

    /// The process's ID.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// What to ask [`crate::Daemon::processes`] for: the output after what
    /// this holds.
    pub fn request(&self) -> ProcessOutputParams {
        ProcessOutputParams {
            process: self.id.clone(),
            from: self.next,
        }
    }

    /// The process as the latest answer had it; `None` before one came.
    pub fn process(&self) -> Option<&SessionProcess> {
        self.process.as_ref()
    }

    /// The output read so far, oldest line first.
    pub fn lines(&self) -> &[OutputLine] {
        &self.lines
    }

    /// Whether earlier output was dropped, by the session or here, as it
    /// retains only so much.
    pub fn evicted(&self) -> bool {
        self.evicted
    }

    /// Takes an answer for this process. Chunks already held are skipped,
    /// so an answer to an older request changes nothing it already gave.
    pub fn apply(&mut self, output: ProcessOutput) -> LogChange {
        let before = self.lines.len();
        let mut changed = false;
        // Output between what this holds and what the session retains was
        // evicted: what comes next continues no line held here.
        if output.first_cursor > self.next {
            self.open = false;
            self.returned = false;
        }
        for chunk in output.chunks {
            if chunk.cursor < self.next || chunk.text.is_empty() {
                continue;
            }
            self.next = chunk.cursor + 1;
            for piece in chunk.text.split_inclusive('\n') {
                let (text, ends) = match piece.strip_suffix('\n') {
                    // A CRLF ends the line; its CR returns over nothing.
                    Some(line) => (line.strip_suffix('\r').unwrap_or(line), true),
                    None => (piece, false),
                };
                let continues = self.open
                    && self
                        .lines
                        .last()
                        .is_some_and(|line| line.stream == chunk.stream);
                if continues {
                    // A line this answer did not start took more text.
                    changed |= self.lines.len() == before;
                } else {
                    self.returned = false;
                    self.lines.push(OutputLine {
                        stream: chunk.stream,
                        text: String::new(),
                        last_cursor: chunk.cursor,
                    });
                }
                let last = self.lines.len() - 1;
                let line = &mut self.lines[last];
                line.last_cursor = chunk.cursor;
                write(&mut line.text, &mut self.returned, text);
                self.open = !ends;
            }
        }
        self.next = self.next.max(output.next_cursor);
        // The session no longer retains these, nor anything before them.
        // Lines of this answer's chunks are never among them: the daemon
        // sends only what it retains.
        let dropped = self
            .lines
            .iter()
            .take_while(|line| line.last_cursor < output.first_cursor)
            .count();
        self.lines.drain(..dropped);
        self.evicted |= output.first_cursor > 0;
        self.process = Some(output.process);
        LogChange {
            dropped,
            changed: (changed && before > dropped).then(|| before - dropped - 1),
            added: self.lines.len() + dropped - before,
        }
    }
}

/// Writes `text` at the end of `line`. After a carriage return, which
/// `returned` carries from one write to the next, text starts the line
/// over; a return at the end waits for the text that replaces it.
fn write(line: &mut String, returned: &mut bool, text: &str) {
    for (ix, part) in text.split('\r').enumerate() {
        if ix > 0 {
            *returned = true;
        }
        if part.is_empty() {
            continue;
        }
        if std::mem::take(returned) {
            line.clear();
        }
        line.push_str(part);
    }
}

#[cfg(test)]
mod tests {
    use rho_protocol::{ProcessChunk, ProcessOutput, ProcessState, ProcessStream, SessionProcess};

    use super::{LogChange, ProcessLog};

    /// An answer of `chunks`, each `(cursor, stream, text)`, with the
    /// session retaining from `first` on.
    fn answer(first: u64, chunks: &[(u64, ProcessStream, &str)]) -> ProcessOutput {
        ProcessOutput {
            process: SessionProcess {
                id: "p1".to_owned(),
                command: "cargo build".to_owned(),
                state: ProcessState::Running,
                elapsed_seconds: 3,
                quiet_seconds: Some(0),
                exit_code: None,
            },
            first_cursor: first,
            next_cursor: chunks.last().map_or(first, |(cursor, ..)| cursor + 1),
            chunks: chunks
                .iter()
                .map(|&(cursor, stream, text)| ProcessChunk {
                    cursor,
                    stream,
                    text: text.to_owned(),
                })
                .collect(),
        }
    }

    fn texts(log: &ProcessLog) -> Vec<(ProcessStream, &str)> {
        log.lines()
            .iter()
            .map(|line| (line.stream(), line.text()))
            .collect()
    }

    /// Chunks join into lines however the daemon read them: a line spans
    /// chunks of one stream, another stream starts its own, CRLF ends a
    /// line, and a carriage return starts it over.
    #[test]
    fn chunks_become_lines_as_a_terminal_shows_them() {
        use ProcessStream::{Stderr, Stdout};

        // Chunks as (cursor, stream, text), and the lines they make.
        type Case = (
            &'static [(u64, ProcessStream, &'static str)],
            &'static [(ProcessStream, &'static str)],
        );
        let cases: &[Case] = &[
            (
                &[(0, Stdout, "one\ntw"), (1, Stdout, "o\nthree")],
                &[(Stdout, "one"), (Stdout, "two"), (Stdout, "three")],
            ),
            (
                &[
                    (0, Stdout, "build"),
                    (1, Stderr, "warning\n"),
                    (2, Stdout, "ok\n"),
                ],
                &[(Stdout, "build"), (Stderr, "warning"), (Stdout, "ok")],
            ),
            (
                &[(0, Stdout, "a\r\nb\r\n")],
                &[(Stdout, "a"), (Stdout, "b")],
            ),
            (
                &[
                    (0, Stdout, "10%\r"),
                    (1, Stdout, "50%\r"),
                    (2, Stdout, "done\n"),
                ],
                &[(Stdout, "done")],
            ),
            (&[(0, Stdout, "10%\r")], &[(Stdout, "10%")]),
        ];
        for (chunks, lines) in cases {
            let mut log = ProcessLog::new("p1".to_owned());
            let _ = log.apply(answer(0, chunks));
            assert_eq!(texts(&log), *lines, "chunks {chunks:?}");
        }
    }

    /// Following reads on from the last answer; an answer that repeats
    /// chunks held already adds nothing, and the change says which rows a
    /// list must touch: the open line that grew, and the lines added.
    #[test]
    fn following_reads_on_and_says_what_changed() {
        let mut log = ProcessLog::new("p1".to_owned());
        assert_eq!(log.request().from, 0);
        let first = log.apply(answer(0, &[(0, ProcessStream::Stdout, "a\nb")]));
        assert_eq!(
            first,
            LogChange {
                dropped: 0,
                changed: None,
                added: 2
            }
        );
        assert_eq!(log.request().from, 1);

        let more = log.apply(answer(0, &[(1, ProcessStream::Stdout, "c\nd\n")]));
        assert_eq!(texts(&log).last(), Some(&(ProcessStream::Stdout, "d")));
        assert_eq!(
            more,
            LogChange {
                dropped: 0,
                changed: Some(1),
                added: 1
            }
        );

        let again = log.apply(answer(0, &[(1, ProcessStream::Stdout, "c\nd\n")]));
        assert_eq!(again, LogChange::default(), "an older answer adds nothing");
        assert_eq!(log.lines().len(), 3);
    }

    /// Lines the session evicted leave, and the log says output was lost.
    #[test]
    fn evicted_output_leaves() {
        let mut log = ProcessLog::new("p1".to_owned());
        let _ = log.apply(answer(
            0,
            &[
                (0, ProcessStream::Stdout, "old\n"),
                (1, ProcessStream::Stdout, "kept\n"),
            ],
        ));
        assert!(!log.evicted());
        let change = log.apply(answer(1, &[(2, ProcessStream::Stdout, "new\n")]));
        assert_eq!(
            change,
            LogChange {
                dropped: 1,
                changed: None,
                added: 1
            }
        );
        assert_eq!(
            texts(&log),
            [
                (ProcessStream::Stdout, "kept"),
                (ProcessStream::Stdout, "new")
            ]
        );
        assert!(log.evicted());

        // A line left open ends where output it never read was evicted,
        // and leaves with it, rather than run on into what came after.
        let _ = log.apply(answer(1, &[(3, ProcessStream::Stdout, "half")]));
        let _ = log.apply(answer(9, &[(9, ProcessStream::Stdout, "later\n")]));
        assert_eq!(texts(&log), [(ProcessStream::Stdout, "later")]);
    }
}

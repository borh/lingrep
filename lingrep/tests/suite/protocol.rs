// Exempt test/bench/example code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::todo,
    clippy::unimplemented,
    clippy::unreachable,
    clippy::exit,
    clippy::unchecked_time_subtraction,
    clippy::unwrap_used
)]

use std::io::{self, Write};

use lingrep::{
    protocol::{OutputError, query::QueryEnvelope},
    write_jsonl,
};

#[test]
fn broken_pipe_is_typed_for_authoritative_jsonl() {
    let event = QueryEnvelope::new(lingrep::protocol::query::QueryEvent::Summary(
        lingrep::protocol::query::SummaryData {
            pattern: "p".into(),
            group: lingrep::protocol::query::SummaryGroupData::Surface {
                value: "猫".into()
            },
            count: 1,
            rank: 1,
        },
    ));
    let error = write_jsonl(&mut BrokenPipe, &event).unwrap_err();
    assert!(matches!(error, OutputError::Io(ref io) if io.kind() == io::ErrorKind::BrokenPipe));
}

struct BrokenPipe;

impl Write for BrokenPipe {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::ErrorKind::BrokenPipe.into())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

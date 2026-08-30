use std::io::{self, Read as _, Write as _};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::process::ExitCode;

use dayweave_scheduler_helper::{
    INTERNAL_EXIT_CODE, MAX_INPUT_BYTES, ProcessOutput, internal_failure_output,
    invalid_invocation_output, process_bytes,
};

fn main() -> ExitCode {
    std::panic::set_hook(Box::new(|_| {}));
    let output = catch_unwind(AssertUnwindSafe(run)).unwrap_or_else(|_| internal_failure_output());
    write_output(&output)
}

fn run() -> ProcessOutput {
    if std::env::args_os().len() != 1 {
        return invalid_invocation_output();
    }

    let mut input = Vec::new();
    let limit = u64::try_from(MAX_INPUT_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    if io::stdin()
        .lock()
        .take(limit)
        .read_to_end(&mut input)
        .is_err()
    {
        return internal_failure_output();
    }
    process_bytes(&input)
}

fn write_output(output: &ProcessOutput) -> ExitCode {
    let mut stdout = io::stdout().lock();
    if stdout.write_all(&output.stdout).is_err() || stdout.flush().is_err() {
        return ExitCode::from(INTERNAL_EXIT_CODE);
    }
    ExitCode::from(output.exit_code)
}

//! Process entry point of the tiny per-sandbox init (T08). All behaviour
//! lives in `ls_guest::init` so it is unit-testable without PID 1; this is
//! only the argv/os handshake.

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    std::process::exit(ls_guest::init::main_entry(argv));
}

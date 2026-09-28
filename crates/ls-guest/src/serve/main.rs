//! Process entry point of the guest control agent (`lsguest serve --fd
//! <N>`). All behaviour lives in `ls_guest::serve` so integration tests
//! exercise it in-process over socketpairs; this is only the argv handshake.

#[cfg(unix)]
fn main() {
    let argv: Vec<String> = std::env::args().collect();
    std::process::exit(ls_guest::serve::agent::main_entry(argv));
}

#[cfg(not(unix))]
fn main() {
    eprintln!("lsguest: the guest control agent is unix-only");
    std::process::exit(2);
}

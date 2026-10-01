//! The binary is a shim. Everything it could get wrong lives in the library, under test.

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    chock::cli::dispatch(&refs)
}

fn main() -> std::process::ExitCode {
    unsafe { loom::run_cli() }
}

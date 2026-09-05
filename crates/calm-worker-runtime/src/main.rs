fn main() {
    if let Err(error) = calm_worker_runtime::helper_main() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

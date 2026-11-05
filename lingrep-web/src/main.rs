#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use clap::Parser;

fn main() {
    if let Err(message) = lingrep_web::serve::run(&lingrep_web::Args::parse()) {
        eprintln!("lingrep-web: {message}");
        std::process::exit(2);
    }
}

pub mod cli;
pub mod errors;
pub mod fs;
pub mod journal;
pub mod vendor;

fn main() {
    env_logger::init();
    match cli::main() {
        Err(e) => eprintln!("{}", e),
        Ok(()) => (),
    }
}

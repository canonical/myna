#[path = "../build-support/version.rs"]
mod version;

fn main() {
    version::emit();
}

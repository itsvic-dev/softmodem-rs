fn main() {
    if let Err(error) = pkg_config::probe_library("spandsp") {
        panic!("spandsp not found, use `nix develop`: {error}");
    }
}

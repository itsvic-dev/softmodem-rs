fn main() {
    println!("cargo::rustc-check-cfg=cfg(spandsp_3_1)");
    let spandsp = match pkg_config::probe_library("spandsp") {
        Ok(spandsp) => spandsp,
        Err(error) => panic!("spandsp not found, use `nix develop`: {error}"),
    };
    let mut version = spandsp.version.split('.').map(str::parse::<u32>);
    if let (Some(Ok(major)), Some(Ok(minor))) = (version.next(), version.next())
        && (major, minor) >= (3, 1)
    {
        println!("cargo::rustc-cfg=spandsp_3_1");
    }
}

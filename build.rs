fn main() {
    println!("cargo:rerun-if-changed=native");
    if std::env::var_os("CARGO_FEATURE_DMABUF").is_none() {
        return;
    }
    assert_eq!(
        std::env::var("CARGO_CFG_TARGET_OS").unwrap(),
        "linux",
        "DMA-BUF requires Linux"
    );
    let mut build = cc::Build::new();
    for package in ["egl", "glesv2", "gbm", "libdrm"] {
        let lib = pkg_config::Config::new().probe(package).expect(package);
        build.includes(lib.include_paths);
    }
    build
        .files(["native/exporter.c", "native/transport.c"])
        .flag("-std=c11")
        .warnings(true)
        .compile("omatuber-dmabuf");
}

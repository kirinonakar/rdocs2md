use std::{env, path::PathBuf};

fn main() {
    // app.res was compiled with rc.exe from `1 ICON "app.ico"`.
    // The ICO contains 16–256 px images generated from app.png.
    println!("cargo::rerun-if-changed=app.res");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        let resource = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("app.res");
        println!("cargo::rustc-link-arg-bin=rdocs2md={}", resource.display());
    }
}

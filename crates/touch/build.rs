fn main() {
    // MultitouchSupport is a private framework, so it isn't on the default framework path.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-search=framework=/System/Library/PrivateFrameworks");
    }
}

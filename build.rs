fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // Windows: load this program's DLL imports (mpr.dll, propsys.dll, ...) from System32 only.
    // By default the loader looks next to the .exe first, so a DLL planted in the folder a
    // downloaded fsls.exe sits in (Downloads) would run as soon as fsls starts.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/DEPENDENTLOADFLAG:0x800"); // LOAD_LIBRARY_SEARCH_SYSTEM32
    }
}

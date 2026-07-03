fn main() {
    println!("cargo:rerun-if-changed=vendor/m68k_cpu_tester.c");
    cc::Build::new()
        .file("vendor/m68k_cpu_tester.c")
        .include("vendor")
        .include("vendor/capstone-stub")
        // The vendored runner is C with sprintf-heavy reporting; keep its
        // warnings out of our build output.
        .warnings(false)
        .flag_if_supported("-Wno-everything")
        .compile("m68k_cpu_tester");
}

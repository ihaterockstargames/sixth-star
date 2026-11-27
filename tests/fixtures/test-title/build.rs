fn main() {
    // No C runtime: the entry is ours, the few memory routines too.
    println!("cargo:rustc-link-arg-bins=/ENTRY:title_main");
    println!("cargo:rustc-link-arg-bins=/SUBSYSTEM:CONSOLE");
    println!("cargo:rustc-link-arg-bins=/NODEFAULTLIB");
}

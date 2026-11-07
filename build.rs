fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=packaging/windows/tabula.ico");

    // Windows: embed the icon as resource 1. Explorer shows it for the .exe,
    // and gpui loads resource 1 as the window and taskbar icon.
    #[cfg(windows)]
    winresource::WindowsResource::new()
        .set_icon("packaging/windows/tabula.ico")
        .compile()
        .expect("embed the Windows icon");
}

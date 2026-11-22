//! Windows only: stamps the icon and a VERSIONINFO block into the executable,
//! so Explorer, the taskbar, Alt-Tab and the installer's uninstall entry show
//! them. Nothing to do on other targets.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/icon.ico");
    res.set("ProductName", "edownloader");
    res.set("FileDescription", "edownloader");
    res.set("LegalCopyright", "MIT License");
    if let Err(err) = res.compile() {
        println!("cargo:warning=windows resource not embedded: {err}");
    }
}

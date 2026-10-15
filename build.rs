fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=static");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "Quiekel Embed");
        res.set("FileDescription", "Quiekel Embed");
        if let Err(e) = res.compile() {
            println!("cargo:warning=no icon in the program: {e}");
        }
    }
}

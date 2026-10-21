fn main() {
    println!("cargo:rerun-if-changed=assets/folio.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/folio.ico");
        resource
            .compile()
            .expect("Could not embed the Windows icon");
    }
}

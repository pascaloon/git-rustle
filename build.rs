fn main() {
    println!("cargo:rerun-if-changed=assets/app-icon.rc");
    println!("cargo:rerun-if-changed=assets/app-icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // GPUI loads the executable's icon resource with ID 1 for its window class.
        embed_resource::compile("assets/app-icon.rc", embed_resource::NONE)
            .manifest_required()
            .expect("compile Windows application icon");
    }
}

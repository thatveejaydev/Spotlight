fn main() {
    slint_build::compile("ui/spotlight.slint").expect("Failed to compile ui/spotlight.slint");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=spotlight.rc");
        println!("cargo:rerun-if-changed=assets/spotlight.ico");
        embed_resource::compile_for("spotlight.rc", ["premiere-spotlight"], embed_resource::NONE)
            .manifest_required()
            .expect("Failed to embed the Windows icon");
    }
}

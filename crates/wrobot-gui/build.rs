fn main() {
    #[cfg(windows)]
    {
        embed_resource::compile("resources.rc", embed_resource::NONE)
            .manifest_optional()
            .unwrap();
        println!("cargo:rerun-if-changed=resources.rc");
        println!("cargo:rerun-if-changed=assets/icon.ico");
    }
}

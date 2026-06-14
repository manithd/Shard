fn main() {
    println!("cargo:rerun-if-changed=src/main.slint");
    println!("cargo:rerun-if-changed=ui/fonts");

    let config = slint_build::CompilerConfiguration::new()
        .embed_resources(slint_build::EmbedResourcesKind::EmbedFiles);

    slint_build::compile_with_config("src/main.slint", config)
        .expect("Slint UI compilation failed");
}

fn main() {
    // Compile the Slint UI with the fluent style, software-renderer resource
    // embedding (no GPU texture upload path).
    let config = slint_build::CompilerConfiguration::new()
        .with_style("fluent".into())
        .embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer);
    slint_build::compile_with_config("ui/main.slint", config)
        .expect("Slint UI failed to compile");

    // Embed the Windows manifest and app icon. winresource is a no-op off Windows,
    // so this is safe to run from any host during development.
    #[cfg(target_os = "windows")]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_manifest_file("app.manifest");
        res.set_icon("assets/icon.ico");
        res.compile().expect("failed to embed Windows resources");
    }

    println!("cargo:rerun-if-changed=ui/main.slint");
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rerun-if-changed=assets/icon.ico");
}

// Embeds the application icon into ai-gateway.exe on Windows.
// A no-op on other targets. A failure to embed (e.g. no Windows SDK `rc.exe`)
// is a warning, never a build failure: the icon is cosmetic.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if let embed_resource::CompilationResult::Failed(reason) =
        embed_resource::compile("assets/app.rc", embed_resource::NONE)
    {
        println!("cargo:warning=could not embed the Windows application icon: {reason}");
    }
}

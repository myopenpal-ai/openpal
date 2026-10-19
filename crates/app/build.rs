use std::{env, fs, path::PathBuf};

/// tauri-build's default Windows manifest, verbatim: Common Controls v6, which
/// has the `TaskDialogIndirect` rfd's message dialogs import.
const WINDOWS_APP_MANIFEST: &str = r#"<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity
        type="win32"
        name="Microsoft.Windows.Common-Controls"
        version="6.0.0.0"
        processorArchitecture="*"
        publicKeyToken="6595b64144ccf1df"
        language="*"
      />
    </dependentAssembly>
  </dependency>
</assembly>
"#;

fn main() {
    // tauri-build links its manifest into bin targets only, so the lib's test
    // binary got comctl32 v5, which has no `TaskDialogIndirect`, and Windows
    // refused to start it (STATUS_ENTRYPOINT_NOT_FOUND) before any test ran.
    // The same manifest goes into every linked artifact instead.
    let windows = tauri_build::WindowsAttributes::new_without_app_manifest();
    tauri_build::try_build(tauri_build::Attributes::new().windows_attributes(windows))
        .expect("failed to run tauri-build");
    embed_windows_manifest();
}

fn embed_windows_manifest() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let manifest = out_dir.join("app.manifest");
    fs::write(&manifest, WINDOWS_APP_MANIFEST).expect("write app.manifest");
    // 1 = CREATEPROCESS_MANIFEST_RESOURCE_ID, 24 = RT_MANIFEST.
    let rc = out_dir.join("app-manifest.rc");
    let manifest_path = manifest.display().to_string().replace('\\', "\\\\");
    fs::write(&rc, format!("1 24 \"{manifest_path}\"\n")).expect("write app-manifest.rc");
    embed_resource::compile_for_everything(&rc, embed_resource::NONE)
        .manifest_required()
        .expect("failed to embed the Windows manifest");
}

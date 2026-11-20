//! Embeds the application icon (resource id 1, which gpui also uses as the window icon).

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(windows)]
    {
        use std::path::PathBuf;

        let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
        let ico = manifest.join("../../assets/app-icon/icon.ico");
        println!("cargo:rerun-if-changed={}", ico.display());
        let out = PathBuf::from(std::env::var("OUT_DIR")?);
        let rc = out.join("selachi.rc");
        let ico = ico.canonicalize().unwrap_or(ico);
        // rc.exe wants forward slashes and no verbatim (`\\?\`) prefix.
        let ico_path = ico.display().to_string();
        let ico_path = ico_path
            .strip_prefix("\\\\?\\")
            .unwrap_or(&ico_path)
            .replace('\\', "/");
        std::fs::write(&rc, format!("1 ICON \"{ico_path}\"\n"))?;
        embed_resource::compile(&rc, embed_resource::NONE)
            .manifest_optional()
            .map_err(|e| format!("embedding the icon resource failed: {e}"))?;
    }
    Ok(())
}

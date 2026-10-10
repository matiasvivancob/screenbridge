// Embeds the app icon (resource id 1): Explorer, the installer, the tray and the viewer windows use it.
fn main() {
    embed_resource::compile("assets/screenbridge.rc", embed_resource::NONE).manifest_optional().unwrap();
}

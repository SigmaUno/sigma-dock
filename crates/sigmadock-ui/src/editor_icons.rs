//! Installed application icons from macOS, without vendoring third-party branding.
use crate::{
    editor::Editor,
    icons::{Icon, icon},
};
use gpui::{Pixels, prelude::*};
use std::path::PathBuf;

#[cfg(target_os = "macos")]
fn bundle_ids(editor: Editor) -> &'static [&'static str] {
    match editor {
        Editor::Zed => &["dev.zed.Zed"],
        Editor::Cursor => &["com.todesktop.230313mzl4w4u92"],
        Editor::VsCode => &["com.microsoft.VSCode"],
        Editor::Sublime => &["com.sublimetext.4", "com.sublimetext.3"],
        Editor::Idea => &["com.jetbrains.intellij", "com.jetbrains.intellij.ce"],
        Editor::RustRover => &["com.jetbrains.rustrover"],
        Editor::GoLand => &["com.jetbrains.goland"],
        Editor::PyCharm => &["com.jetbrains.pycharm", "com.jetbrains.pycharm.ce"],
        Editor::WebStorm => &["com.jetbrains.WebStorm"],
        Editor::Xcode => &["com.apple.dt.Xcode"],
        Editor::Environment | Editor::Custom | Editor::System => &[],
    }
}
pub(crate) fn app_bundle(editor: Editor) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::NSWorkspace;
        use objc2_foundation::NSString;
        let workspace = NSWorkspace::sharedWorkspace();
        bundle_ids(editor)
            .iter()
            .find_map(|id| {
                workspace
                    .URLForApplicationWithBundleIdentifier(&NSString::from_str(id))
                    .and_then(|url| url.path())
                    .map(|path| PathBuf::from(path.to_string()))
            })
            .filter(|path| path.is_dir())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = editor;
        None
    }
}
#[cfg(target_os = "macos")]
fn app_icon(editor: Editor) -> Option<std::sync::Arc<gpui::Image>> {
    use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSWorkspace};
    use objc2_foundation::{NSDictionary, NSString};
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex, OnceLock},
    };
    // Cache successful images only: a later install should still become discoverable.
    static CACHE: OnceLock<Mutex<HashMap<Editor, Arc<gpui::Image>>>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(Default::default).lock().ok()?;
    if let Some(icon) = cache.get(&editor) {
        return Some(icon.clone());
    }
    let app = app_bundle(editor)?;
    let image = NSWorkspace::sharedWorkspace().iconForFile(&NSString::from_str(app.to_str()?));
    let tiff = image.TIFFRepresentation()?;
    let bitmap = NSBitmapImageRep::imageRepWithData(&tiff)?;
    // Empty properties meet AppKit's key/value type requirements. NSData is immutable
    // and retained until the bytes have been copied into GPUI's owned image.
    let data = unsafe {
        bitmap.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
    }?;
    let icon = Arc::new(gpui::Image::from_bytes(
        gpui::ImageFormat::Png,
        unsafe { data.as_bytes_unchecked() }.to_vec(),
    ));
    cache.insert(editor, icon.clone());
    Some(icon)
}
pub(crate) fn editor_icon(editor: Editor, size: Pixels, color: u32) -> gpui::AnyElement {
    #[cfg(target_os = "macos")]
    if let Some(image) = app_icon(editor) {
        return gpui::img(image).size(size).flex_none().into_any_element();
    }
    icon(
        if editor == Editor::Environment {
            Icon::Terminal
        } else {
            Icon::ExternalLink
        },
        size,
        gpui::rgb(color),
    )
    .into_any_element()
}

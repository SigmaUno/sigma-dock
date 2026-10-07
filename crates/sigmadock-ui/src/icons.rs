//! Vendored Lucide icons (ISC), embedded so app and crate builds need no asset files at runtime.
//! Source and license: packaging/licenses/Lucide.txt.

use gpui::{AssetSource, Pixels, Rgba, SharedString, Styled, Svg, svg};
use std::borrow::Cow;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Icon {
    ArrowLeft,
    Check,
    Folder,
    GitBranch,
    GitPullRequest,
    Plus,
    Settings,
    Terminal,
}

macro_rules! embed {
    ($name:literal) => {
        (
            concat!("icons/", $name, ".svg"),
            include_bytes!(concat!("../assets/icons/", $name, ".svg")),
        )
    };
}

const ICONS: &[(&str, &[u8])] = &[
    embed!("arrow-left"),
    embed!("check"),
    embed!("folder"),
    embed!("git-branch"),
    embed!("git-pull-request"),
    embed!("plus"),
    embed!("settings"),
    embed!("square-terminal"),
];

impl Icon {
    fn path(self) -> &'static str {
        match self {
            Self::ArrowLeft => "icons/arrow-left.svg",
            Self::Check => "icons/check.svg",
            Self::Folder => "icons/folder.svg",
            Self::GitBranch => "icons/git-branch.svg",
            Self::GitPullRequest => "icons/git-pull-request.svg",
            Self::Plus => "icons/plus.svg",
            Self::Settings => "icons/settings.svg",
            Self::Terminal => "icons/square-terminal.svg",
        }
    }
}

/// A monochrome icon tinted with a theme color; GPUI paints SVGs as a mask in the text color.
pub(crate) fn icon(icon: Icon, size: Pixels, color: impl Into<Rgba>) -> Svg {
    svg()
        .path(icon.path())
        .size(size)
        .flex_none()
        .text_color(color.into())
}

pub(crate) struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::from(*name))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_resolves_to_an_embedded_svg() {
        for icon in [
            Icon::ArrowLeft,
            Icon::Check,
            Icon::Folder,
            Icon::GitBranch,
            Icon::GitPullRequest,
            Icon::Plus,
            Icon::Settings,
            Icon::Terminal,
        ] {
            let bytes = Assets.load(icon.path()).unwrap().expect("embedded icon");
            assert!(std::str::from_utf8(&bytes).unwrap().contains("<svg"));
        }
        assert_eq!(Assets.list("icons/").unwrap().len(), ICONS.len());
    }
}

//! Vendored Lucide icons (ISC), embedded so app and crate builds need no asset files at runtime.
//! Source and license: packaging/licenses/Lucide.txt.

use gpui::{AssetSource, Img, Pixels, Rgba, SharedString, Styled, Svg, img, svg};
use std::borrow::Cow;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Icon {
    ArrowLeft,
    Check,
    ChevronDown,
    ChevronRight,
    CircleCheck,
    CircleDot,
    ExternalLink,
    GitBranch,
    GitPullRequest,
    Inbox,
    Plus,
    Question,
    Refresh,
    Settings,
    Stop,
    Terminal,
    X,
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
    embed!("chevron-down"),
    embed!("chevron-right"),
    embed!("circle-check"),
    embed!("circle-dot"),
    embed!("square-arrow-out-up-right"),
    embed!("git-branch"),
    embed!("git-pull-request"),
    embed!("inbox"),
    embed!("plus"),
    embed!("message-circle-question"),
    embed!("refresh-cw"),
    embed!("settings"),
    embed!("square"),
    embed!("square-terminal"),
    (APP_ICON, include_bytes!("../assets/brand/sigmadock-64.png")),
    embed!("x"),
];
/// The SigmaDock app icon, rendered in color (unlike the tinted Lucide masks).
const APP_ICON: &str = "brand/sigmadock-64.png";

impl Icon {
    fn path(self) -> &'static str {
        match self {
            Self::ArrowLeft => "icons/arrow-left.svg",
            Self::Check => "icons/check.svg",
            Self::ChevronDown => "icons/chevron-down.svg",
            Self::ChevronRight => "icons/chevron-right.svg",
            Self::CircleCheck => "icons/circle-check.svg",
            Self::CircleDot => "icons/circle-dot.svg",
            Self::ExternalLink => "icons/square-arrow-out-up-right.svg",
            Self::GitBranch => "icons/git-branch.svg",
            Self::GitPullRequest => "icons/git-pull-request.svg",
            Self::Inbox => "icons/inbox.svg",
            Self::Plus => "icons/plus.svg",
            Self::Question => "icons/message-circle-question.svg",
            Self::Refresh => "icons/refresh-cw.svg",
            Self::Settings => "icons/settings.svg",
            Self::Stop => "icons/square.svg",
            Self::Terminal => "icons/square-terminal.svg",
            Self::X => "icons/x.svg",
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

/// The SigmaDock app icon at `size`, used for the brand and as the agent avatar.
pub(crate) fn app_icon(size: Pixels) -> Img {
    img(APP_ICON).size(size).flex_none()
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
            Icon::ChevronDown,
            Icon::ChevronRight,
            Icon::CircleCheck,
            Icon::CircleDot,
            Icon::ExternalLink,
            Icon::GitBranch,
            Icon::GitPullRequest,
            Icon::Inbox,
            Icon::Plus,
            Icon::Question,
            Icon::Refresh,
            Icon::Settings,
            Icon::Stop,
            Icon::Terminal,
            Icon::X,
        ] {
            let bytes = Assets.load(icon.path()).unwrap().expect("embedded icon");
            assert!(std::str::from_utf8(&bytes).unwrap().contains("<svg"));
        }
        assert_eq!(Assets.list("icons/").unwrap().len(), ICONS.len() - 1);
        assert!(Assets.load(APP_ICON).unwrap().is_some());
    }
}

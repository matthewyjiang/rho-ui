//! A surface's images, videos and sounds.
//!
//! Only a `data:` URL's picture is drawn: drawing any other URL would load
//! it, a request the model chose and nobody clicked. Any other URL, and a
//! video or a sound, is a chip that opens an `http` or `https` link in the
//! browser.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    avatar::Avatar, h_flex, link::Link, tag::Tag, ActiveTheme as _, Icon, Sizable as _,
};
use gpui_kit::{
    div, img, rems, AnyElement, App, InteractiveElement as _, IntoElement, ObjectFit,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledImage as _,
};
use rho_ui_core::{
    surface::{ImageFit, ImageSize, Media, MediaKind},
    Image,
};

use super::{Part, SurfacePanel};
use crate::attachments::Preview;

/// The pictures a panel's images show from `data:` URLs, decoded once
/// each, by URL; `None` for a URL that holds no picture this app draws.
#[derive(Default)]
pub(super) struct Pictures(HashMap<String, Option<Preview>>);

impl Pictures {
    /// Keeps the pictures of `urls`, decoding the new ones, and frees
    /// those of any other URL.
    pub(super) fn keep(&mut self, urls: &HashSet<&str>, cx: &mut App) {
        let gone: Vec<String> = self
            .0
            .keys()
            .filter(|url| !urls.contains(url.as_str()))
            .cloned()
            .collect();
        for url in gone {
            if let Some(Some(preview)) = self.0.remove(&url) {
                preview.release(cx);
            }
        }
        for url in urls {
            if !self.0.contains_key(*url) {
                let preview = Image::from_data_url(url).and_then(|image| Preview::of_image(&image));
                self.0.insert((*url).to_owned(), preview);
            }
        }
    }

    /// Frees every picture, as the panel goes.
    pub(super) fn release(&mut self, cx: &mut App) {
        for preview in self.0.drain().filter_map(|(_, preview)| preview) {
            preview.release(cx);
        }
    }

    fn get(&self, url: &str) -> Option<Arc<gpui_kit::Image>> {
        Some(self.0.get(url)?.as_ref()?.image()?.image())
    }
}

impl SurfacePanel {
    pub(super) fn render_media(&self, media: &Media, cx: &App) -> AnyElement {
        let id = self.id(Part::Control(&media.target));
        let (icon, what) = match media.kind {
            MediaKind::Image => (Lucide::Image, "Image"),
            MediaKind::Video => (Lucide::Film, "Video"),
            MediaKind::Audio => (Lucide::Music, "Audio"),
        };
        let name = if media.description.is_empty() {
            media.url.rsplit('/').next().unwrap_or(what).to_owned()
        } else {
            media.description.clone()
        };
        let picture = match media.kind {
            MediaKind::Image => self.pictures.get(&media.url),
            MediaKind::Video | MediaKind::Audio => None,
        };
        let Some(picture) = picture else {
            return chip(id, icon, name, &media.url, cx);
        };
        let radius = cx.theme().radius;
        let frame = div().flex_shrink_0().overflow_hidden();
        let frame = match media.size {
            ImageSize::Avatar => {
                return Avatar::new().name(name).src(picture).into_any_element();
            }
            ImageSize::Icon => frame.size_5(),
            ImageSize::SmallFeature => frame.w(rems(8.)).h(rems(5.)).rounded(radius),
            ImageSize::MediumFeature => frame.w(rems(16.)).h(rems(10.)).rounded(radius),
            ImageSize::LargeFeature => frame.w(rems(28.)).h(rems(16.)).rounded(radius),
            ImageSize::Header => frame.w_full().h(rems(10.)).rounded(radius),
        };
        frame
            .id(id)
            .test_support()
            .aria_label(name)
            .child(img(picture).size_full().object_fit(match media.fit {
                ImageFit::Contain => ObjectFit::Contain,
                ImageFit::Cover => ObjectFit::Cover,
                ImageFit::Fill => ObjectFit::Fill,
                ImageFit::None => ObjectFit::None,
                ImageFit::ScaleDown => ObjectFit::ScaleDown,
            }))
            .into_any_element()
    }
}

/// A chip naming media this app does not draw, which opens it in the
/// browser when its URL is a web link.
fn chip(id: SharedString, icon: Lucide, name: String, url: &str, cx: &App) -> AnyElement {
    let tag = Tag::secondary()
        .small()
        .child(h_flex().gap_1().child(Icon::new(icon).small()).child(name));
    let opens = url.starts_with("https://") || url.starts_with("http://");
    let chip = if opens {
        Link::new(id)
            .href(url.to_owned())
            .child(tag)
            .into_any_element()
    } else {
        div()
            .id(id)
            .test_support()
            .text_color(cx.theme().muted_foreground)
            .child(tag)
            .into_any_element()
    };
    h_flex().child(chip).into_any_element()
}

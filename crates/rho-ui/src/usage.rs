//! Context use and cost in the composer's footer: a ring that fills as the
//! context window does, and on hover a card with the tokens in use and what
//! the session has cost. When the daemon knows no window size there is no
//! ring to fill, so the cost shows as text instead. A click on either opens
//! the session's context in the right panel, if the view gives it
//! somewhere to open it.

use std::rc::Rc;

use gpui_kit::base::TestSupportExt as _;
use gpui_kit::component::{
    button::{Button, ButtonVariants as _},
    h_flex,
    hover_card::HoverCard,
    progress::ProgressCircle,
    v_flex, ActiveTheme as _, Sizable as _,
};
use gpui_kit::{
    div, Anchor, AnyElement, App, InteractiveElement as _, IntoElement, ParentElement as _,
    RenderOnce, StatefulInteractiveElement as _, Styled as _, Window,
};
use rho_ui_core::Usage;

/// What a click on the ring does.
type OnClick = Rc<dyn Fn(&mut Window, &mut App)>;

/// The daemon's last usage report, drawn for the footer.
#[derive(IntoElement)]
pub(crate) struct UsageRing {
    usage: Usage,
    on_click: Option<OnClick>,
}

impl UsageRing {
    pub(crate) fn new(usage: Usage) -> Self {
        Self {
            usage,
            on_click: None,
        }
    }

    /// Runs `handler` on a click, as to open the session's context.
    pub(crate) fn on_click(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }

    /// The share of the context window in use, in whole percent, when the
    /// window's size is known.
    fn percent(&self) -> Option<u64> {
        let size = self.usage.size().filter(|size| *size > 0)?;
        Some(self.usage.used().saturating_mul(100) / size)
    }

    /// "$0.01", when the daemon reports a cost.
    fn cost(&self) -> Option<String> {
        self.usage.cost_usd().map(|cost| format!("${cost:.2}"))
    }

    /// The card the ring opens on hover.
    fn render_details(usage: &Usage, percent: u64, cost: Option<String>, cx: &App) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let row = |name: &'static str, value: String| {
            h_flex()
                .gap_4()
                .justify_between()
                .child(div().text_color(muted).child(name))
                .child(value)
        };
        let window = usage.size().map_or_else(String::new, tokens);
        v_flex()
            .id("usage-details")
            .test_support()
            .min_w_40()
            .gap_1()
            .text_xs()
            .child(row("Context", format!("{percent}%")))
            .child(
                div()
                    .text_color(muted)
                    .child(format!("{} of {window} tokens", tokens(usage.used()))),
            )
            .children(cost.map(|cost| row("Cost", cost)))
            .into_any_element()
    }
}

impl RenderOnce for UsageRing {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let cost = self.cost();
        let Some(percent) = self.percent() else {
            let Some(cost) = cost else {
                return div().into_any_element();
            };
            let text = div().text_xs().text_color(muted).child(cost.clone());
            return trigger(self.on_click, cost, text.into_any_element());
        };
        let label = match &cost {
            Some(cost) => format!("Context {percent}% · {cost}"),
            None => format!("Context {percent}%"),
        };
        // Past 100% the model has overrun its window; the ring can only
        // show it full.
        let shown = u16::try_from(percent.min(100)).unwrap_or(100);
        let ring = ProgressCircle::new("usage-ring")
            .value(f32::from(shown))
            .color(muted)
            .size_4();
        let ring = trigger(self.on_click, label, ring.into_any_element());
        let usage = self.usage;
        HoverCard::new("usage-card")
            .anchor(Anchor::BottomRight)
            .mb_1()
            .trigger(ring)
            .content(move |_, _, cx| Self::render_details(&usage, percent, cost.clone(), cx))
            .into_any_element()
    }
}

/// The ring or cost as drawn, labelled `label`: a button when a click
/// opens something, else plain, so it doesn't look pressable.
fn trigger(on_click: Option<OnClick>, label: String, shows: AnyElement) -> AnyElement {
    match on_click {
        Some(on_click) => Button::new("composer-usage")
            .ghost()
            .xsmall()
            .compact()
            .flex_shrink_0()
            .accessibility_label(label)
            .child(shows)
            .on_click(move |_, window, cx| on_click(window, cx))
            .into_any_element(),
        None => div()
            .id("composer-usage")
            .test_support()
            .flex_shrink_0()
            .aria_label(label)
            .p_1()
            .child(shows)
            .into_any_element(),
    }
}

/// A token count in at most three figures: "950", "2.5k", "200k", "1.2M".
/// Rounds down, so a count never reads as more than it is.
pub(crate) fn tokens(count: u64) -> String {
    let (unit, scale) = match count {
        0..1_000 => return count.to_string(),
        1_000..1_000_000 => ("k", 1_000),
        _ => ("M", 1_000_000),
    };
    let tenths = count / (scale / 10);
    if tenths < 100 && !tenths.is_multiple_of(10) {
        format!("{}.{}{unit}", tenths / 10, tenths % 10)
    } else {
        format!("{}{unit}", count / scale)
    }
}

#[cfg(test)]
mod tests {
    use super::tokens;

    #[test]
    fn token_counts_shorten_without_rounding_up() {
        for (count, text) in [
            (950, "950"),
            (1_000, "1k"),
            (2_500, "2.5k"),
            (10_000, "10k"),
            (199_999, "199k"),
            (999_999, "999k"),
            (1_000_000, "1M"),
            (1_250_000, "1.2M"),
        ] {
            assert_eq!(tokens(count), text, "{count}");
        }
    }
}

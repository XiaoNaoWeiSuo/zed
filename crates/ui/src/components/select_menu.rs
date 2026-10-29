use std::rc::Rc;

use gpui::{Anchor, App, IntoElement, Point, RenderOnce, SharedString, Window, px};

use crate::{
    Color, ContextMenu, DropdownMenu, DropdownStyle, IconName, IconPosition, LabelSize, Tooltip,
    prelude::*,
};

/// One entry in a [`SelectMenu`].
pub struct SelectMenuOption {
    label: SharedString,
    selected: bool,
    on_select: Rc<dyn Fn(&mut Window, &mut App)>,
}

impl SelectMenuOption {
    pub fn new(
        label: impl Into<SharedString>,
        selected: bool,
        on_select: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            label: label.into(),
            selected,
            on_select: Rc::new(on_select),
        }
    }
}

/// A compact select control with the same trigger styling and popover
/// placement as the agent profile and model pickers.
///
/// Use this instead of styling a bare `DropdownMenu` at each call site so
/// hover feedback, typography, anchoring, and selected-state checks stay
/// consistent.
#[derive(IntoElement)]
pub struct SelectMenu {
    id: gpui::ElementId,
    label: SharedString,
    options: Vec<SelectMenuOption>,
    tooltip: Option<SharedString>,
}

impl SelectMenu {
    pub fn new(
        id: impl Into<gpui::ElementId>,
        label: impl Into<SharedString>,
        options: Vec<SelectMenuOption>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            options,
            tooltip: None,
        }
    }

    pub fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }
}

impl RenderOnce for SelectMenu {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let menu = ContextMenu::build(window, cx, |menu, _, _| {
            let mut menu = menu;
            for option in self.options {
                let on_select = option.on_select;
                menu = menu.toggleable_entry(
                    option.label,
                    option.selected,
                    IconPosition::End,
                    None,
                    move |window, cx| on_select(window, cx),
                );
            }
            menu
        });

        let dropdown = DropdownMenu::new(self.id, self.label, menu)
            .style(DropdownStyle::Subtle)
            .label_size(LabelSize::Small)
            .label_color(Color::Muted)
            .anchor(Anchor::BottomRight)
            .offset(Point {
                x: px(0.),
                y: px(-2.),
            })
            .trigger_icon(IconName::ChevronDown);

        match self.tooltip {
            Some(tooltip) => dropdown.trigger_tooltip(Tooltip::text(tooltip)),
            None => dropdown,
        }
    }
}

impl crate::Component for SelectMenu {
    fn scope() -> crate::ComponentScope {
        crate::ComponentScope::Input
    }

    fn name() -> &'static str {
        "SelectMenu"
    }

    fn description() -> &'static str {
        "A compact dropdown select matching the agent profile and model pickers."
    }

    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        div().into_any_element()
    }
}

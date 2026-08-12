use crate::input::Mode;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HelpItem {
    pub key: &'static str,
    pub action: &'static str,
}

pub fn contextual_help(mode: Mode) -> &'static [HelpItem] {
    match mode {
        Mode::Normal => &[
            HelpItem { key: "j/k", action: "move" },
            HelpItem { key: "Enter", action: "open" },
            HelpItem { key: "r", action: "refresh" },
            HelpItem { key: "i", action: "compose" },
            HelpItem { key: "q", action: "quit" },
        ],
        Mode::Insert => &[
            HelpItem { key: "text", action: "edit draft" },
            HelpItem { key: "Esc", action: "normal mode" },
        ],
        Mode::Visual => &[
            HelpItem { key: "j/k", action: "extend selection" },
            HelpItem { key: "Esc", action: "normal mode" },
        ],
        Mode::Command => &[
            HelpItem { key: "text", action: "enter command" },
            HelpItem { key: "Enter", action: "run command" },
            HelpItem { key: "Esc", action: "cancel" },
        ],
        Mode::SearchForward => &[
            HelpItem { key: "text", action: "search forward" },
            HelpItem { key: "Enter", action: "run search" },
            HelpItem { key: "Esc", action: "cancel" },
        ],
        Mode::SearchBackward => &[
            HelpItem { key: "text", action: "search backward" },
            HelpItem { key: "Enter", action: "run search" },
            HelpItem { key: "Esc", action: "cancel" },
        ],
    }
}

pub fn mode_label(mode: Mode) -> &'static str {
    match mode {
        Mode::Normal => "NORMAL",
        Mode::Insert => "INSERT",
        Mode::Visual => "VISUAL",
        Mode::Command => "COMMAND",
        Mode::SearchForward => "SEARCH FORWARD",
        Mode::SearchBackward => "SEARCH BACKWARD",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    MoveDown {
        count: u32,
    },
    MoveUp {
        count: u32,
    },
    /// Jump to the top of the primary content (default key: `gg`).
    GoToFirst {
        count: u32,
    },
    /// Jump to the bottom of the primary content (default key: `G`).
    GoToLast {
        count: u32,
    },
    MoveLeft {
        count: u32,
    },
    MoveRight {
        count: u32,
    },
    Open,
    Refresh,
    /// Flip to the next feed page, replacing the list (default key: `>`).
    NextPage,
    /// Flip back to the previous feed page (default key: `<`).
    PreviousPage,
    EnterInsert,
    EnterVisual,
    EnterCommand,
    EnterSearch {
        backward: bool,
    },
    Back,
    Quit,
    /// Scroll the open detail/thread pane down (default key: Ctrl-d).
    ScrollDetailDown {
        count: u32,
    },
    /// Scroll the open detail/thread pane up (default key: Ctrl-u).
    ScrollDetailUp {
        count: u32,
    },
    /// Collapse the detail/thread pane, returning to the content-only view
    /// (command: `:close`, rebindable as `close-pane`).
    ClosePane,
    /// Confirm the pending destructive action (default key: `y`).
    Confirm,
    /// Cancel the pending destructive action (default key: `n`).
    Cancel,
    Text(String),
    /// Delete the last character of the compose/command line (Backspace).
    Backspace,
    /// Abandon the current command or search line without submitting
    /// (Esc in command/search mode); the open view is left untouched.
    CancelLine,
    SubmitLine(String),
    Noop,
}

impl Command {
    /// Resolve a documented command name to the command it binds, for
    /// configurable key mappings (`[keymaps]` in the config file). Unknown
    /// names return `None` so persisted-but-unrecognized entries are skipped
    /// at startup instead of crashing the input engine.
    pub fn by_name(name: &str) -> Option<Command> {
        match name {
            "down" => Some(Command::MoveDown { count: 1 }),
            "up" => Some(Command::MoveUp { count: 1 }),
            "go-to-first" | "top" => Some(Command::GoToFirst { count: 1 }),
            "go-to-last" | "bottom" => Some(Command::GoToLast { count: 1 }),
            "left" => Some(Command::MoveLeft { count: 1 }),
            "right" => Some(Command::MoveRight { count: 1 }),
            "open" => Some(Command::Open),
            "refresh" => Some(Command::Refresh),
            "next-page" | "load-more" => Some(Command::NextPage),
            "previous-page" => Some(Command::PreviousPage),
            "insert" => Some(Command::EnterInsert),
            "visual" => Some(Command::EnterVisual),
            "command" => Some(Command::EnterCommand),
            "backspace" => Some(Command::Backspace),
            "cancel-line" => Some(Command::CancelLine),
            "search" | "search-forward" => Some(Command::EnterSearch { backward: false }),
            "search-backward" => Some(Command::EnterSearch { backward: true }),
            "back" => Some(Command::Back),
            "close-pane" | "close" => Some(Command::ClosePane),
            "quit" => Some(Command::Quit),
            "scroll-detail-down" => Some(Command::ScrollDetailDown { count: 1 }),
            "scroll-detail-up" => Some(Command::ScrollDetailUp { count: 1 }),
            "confirm" => Some(Command::Confirm),
            "cancel" => Some(Command::Cancel),
            _ => None,
        }
    }
}

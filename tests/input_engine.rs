use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lemmy::input::{Command, InputEngine, MappingMatch, MappingTable, Mode};

fn key(character: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE)
}

fn enter() -> KeyEvent {
    KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
}

fn escape() -> KeyEvent {
    KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)
}

fn ctrl(character: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(character), KeyModifiers::CONTROL)
}

#[test]
fn greater_than_loads_the_next_feed_page() {
    assert_eq!(InputEngine::default().handle(key('>')), Command::NextPage);
}

#[test]
fn ctrl_d_and_ctrl_u_scroll_the_detail_pane() {
    let mut engine = InputEngine::default();
    assert_eq!(
        engine.handle(ctrl('d')),
        Command::ScrollDetailDown { count: 1 }
    );
    assert_eq!(
        engine.handle(ctrl('u')),
        Command::ScrollDetailUp { count: 1 }
    );
    // Ctrl keys are inert outside Normal/Visual mode.
    engine.handle(key('i'));
    assert_eq!(engine.handle(ctrl('d')), Command::Noop);
    engine.handle(escape());
}

#[test]
fn ctrl_scroll_commands_apply_counts() {
    let mut engine = InputEngine::default();
    engine.handle(key('3'));
    assert_eq!(
        engine.handle(ctrl('d')),
        Command::ScrollDetailDown { count: 3 }
    );
    engine.handle(key('2'));
    assert_eq!(
        engine.handle(ctrl('u')),
        Command::ScrollDetailUp { count: 2 }
    );
}

#[test]
fn normal_j_moves_down_once() {
    let mut engine = InputEngine::default();

    assert_eq!(engine.handle(key('j')), Command::MoveDown { count: 1 });
    assert_eq!(engine.mode(), Mode::Normal);
}

#[test]
fn normal_decimal_prefix_applies_to_motion() {
    let mut engine = InputEngine::default();

    assert_eq!(engine.handle(key('1')), Command::Noop);
    assert_eq!(engine.handle(key('2')), Command::Noop);
    assert_eq!(engine.handle(key('j')), Command::MoveDown { count: 12 });
}

#[test]
fn digit_leading_keymap_sequence_prefers_mapping_over_count() {
    let mut keymaps = std::collections::HashMap::new();
    // A multi-key sequence that begins with a digit must be reachable: the
    // digit joins prefix matching instead of accumulating a motion count.
    keymaps.insert("refresh".to_owned(), "2r".to_owned());
    let mut engine = InputEngine::default().with_keymaps(&keymaps);

    assert_eq!(engine.handle(key('2')), Command::Noop);
    assert_eq!(engine.handle(key('r')), Command::Refresh);

    // An exact single-digit mapping wins over counting that digit too.
    let mut single = std::collections::HashMap::new();
    single.insert("open".to_owned(), "3".to_owned());
    let mut engine = InputEngine::default().with_keymaps(&single);
    assert_eq!(engine.handle(key('3')), Command::Open);
}

#[test]
fn discarded_pending_sequence_resets_accumulated_count() {
    let mut engine = InputEngine::default();

    assert_eq!(engine.handle(key('2')), Command::Noop);
    // `x` is unmapped and no mapping begins with it: the pending sequence is
    // discarded and the accumulated count must not leak into later keys.
    assert_eq!(engine.handle(key('x')), Command::Noop);
    assert_eq!(engine.handle(key('j')), Command::MoveDown { count: 1 });

    // The same reset applies to a partially typed multi-key sequence.
    let mut keymaps = std::collections::HashMap::new();
    keymaps.insert("refresh".to_owned(), "gg".to_owned());
    let mut engine = InputEngine::default().with_keymaps(&keymaps);
    assert_eq!(engine.handle(key('4')), Command::Noop);
    assert_eq!(engine.handle(key('g')), Command::Noop); // prefix of "gg"
    assert_eq!(engine.handle(key('x')), Command::Noop); // discards "4" and "g"
    assert_eq!(engine.handle(key('g')), Command::Noop); // still a prefix
    assert_eq!(engine.handle(key('g')), Command::Refresh);
}

#[test]
fn visual_decimal_prefix_applies_to_motion() {
    let mut engine = InputEngine::default();

    assert_eq!(engine.handle(key('v')), Command::EnterVisual);
    assert_eq!(engine.handle(key('2')), Command::Noop);
    assert_eq!(engine.handle(key('j')), Command::MoveDown { count: 2 });
}

#[test]
fn colon_enters_command_mode_and_enter_submits_line() {
    let mut engine = InputEngine::default();

    assert_eq!(engine.handle(key(':')), Command::EnterCommand);
    assert_eq!(engine.mode(), Mode::Command);
    assert_eq!(engine.handle(key('r')), Command::Text("r".into()));
    assert_eq!(engine.handle(key('e')), Command::Text("e".into()));
    assert_eq!(engine.handle(key('f')), Command::Text("f".into()));
    assert_eq!(engine.handle(enter()), Command::SubmitLine("ref".into()));
    assert_eq!(engine.mode(), Mode::Normal);
}

#[test]
fn escape_leaves_insert_and_visual_modes() {
    let mut engine = InputEngine::default();

    assert_eq!(engine.handle(key('i')), Command::EnterInsert);
    assert_eq!(engine.mode(), Mode::Insert);
    assert_eq!(engine.handle(escape()), Command::Back);
    assert_eq!(engine.mode(), Mode::Normal);

    assert_eq!(engine.handle(key('v')), Command::EnterVisual);
    assert_eq!(engine.mode(), Mode::Visual);
    assert_eq!(engine.handle(escape()), Command::Back);
    assert_eq!(engine.mode(), Mode::Normal);
}
#[test]
fn normal_maps_motions_open_refresh_search_and_quit() {
    let mut engine = InputEngine::default();

    assert_eq!(engine.handle(key('h')), Command::MoveLeft { count: 1 });
    assert_eq!(engine.handle(key('k')), Command::MoveUp { count: 1 });
    assert_eq!(engine.handle(key('l')), Command::MoveRight { count: 1 });
    assert_eq!(engine.handle(key('r')), Command::Refresh);
    assert_eq!(engine.handle(enter()), Command::Open);
    assert_eq!(
        engine.handle(key('/')),
        Command::EnterSearch { backward: false }
    );
    assert_eq!(engine.mode(), Mode::SearchForward);
    assert_eq!(engine.handle(escape()), Command::Back);
    assert_eq!(
        engine.handle(key('?')),
        Command::EnterSearch { backward: true }
    );
    assert_eq!(engine.mode(), Mode::SearchBackward);
    assert_eq!(engine.handle(escape()), Command::Back);
    assert_eq!(engine.handle(key('q')), Command::Quit);
}

#[test]
fn insert_emits_printable_text() {
    let mut engine = InputEngine::default();

    assert_eq!(engine.handle(key('i')), Command::EnterInsert);
    assert_eq!(engine.handle(key('x')), Command::Text("x".into()));
    assert_eq!(engine.handle(key(' ')), Command::Text(" ".into()));
    assert_eq!(engine.handle(enter()), Command::Noop);
}

#[test]
fn search_submits_and_backspace_updates_line() {
    let mut engine = InputEngine::default();

    assert_eq!(
        engine.handle(key('/')),
        Command::EnterSearch { backward: false }
    );
    assert_eq!(engine.handle(key('a')), Command::Text("a".into()));
    assert_eq!(engine.handle(key('b')), Command::Text("b".into()));
    assert_eq!(
        engine.handle(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
        Command::Noop
    );
    assert_eq!(engine.handle(enter()), Command::SubmitLine("a".into()));
    assert_eq!(engine.mode(), Mode::Normal);
}

#[test]
fn unknown_key_is_noop() {
    let mut engine = InputEngine::default();

    assert_eq!(engine.handle(key('x')), Command::Noop);
    assert_eq!(engine.handle(key('j')), Command::MoveDown { count: 1 });
}
#[test]
fn mappings_choose_longest_complete_sequence_without_waiting() {
    let mut mappings = MappingTable::new();
    mappings.insert("g", Command::Refresh);
    mappings.insert("gg", Command::Open);

    assert_eq!(mappings.resolve("gg"), Some(Command::Open));
    assert_eq!(mappings.resolve("ggx"), Some(Command::Open));
    assert_eq!(
        mappings.classify("g"),
        MappingMatch::Complete(Command::Refresh)
    );
    assert_eq!(mappings.classify("x"), MappingMatch::NoMatch);
}

#[test]
fn persisted_keymaps_bind_documented_commands_at_startup() {
    let mut keymaps = std::collections::HashMap::new();
    // Rebind the documented `refresh` and `quit` commands to new sequences,
    // and add a multi-key motion sequence.
    keymaps.insert("refresh".to_owned(), "R".to_owned());
    keymaps.insert("quit".to_owned(), "qq".to_owned());
    keymaps.insert("down".to_owned(), "jk".to_owned());
    // An unknown command name must not break the engine.
    keymaps.insert("not-a-command".to_owned(), "x".to_owned());

    let mut engine = InputEngine::default().with_keymaps(&keymaps);

    assert_eq!(engine.handle(key('R')), Command::Refresh);
    // `q` is still the default quit; the persisted multi-key `qq` sequence
    // must be preferred over the single-key default when it completes.
    assert_eq!(engine.handle(key('q')), Command::Noop);
    assert_eq!(engine.handle(key('q')), Command::Quit);
    // The custom `jk` motion binds down; the default `j` no longer fires
    // alone because `jk` is a prefix.
    assert_eq!(engine.handle(key('j')), Command::Noop);
    assert_eq!(engine.handle(key('k')), Command::MoveDown { count: 1 });
    assert_eq!(
        engine.handle(key('x')),
        Command::Noop,
        "unknown command names must be skipped"
    );
}

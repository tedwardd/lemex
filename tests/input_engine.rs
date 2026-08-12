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
    assert_eq!(engine.handle(key('/')), Command::EnterSearch { backward: false });
    assert_eq!(engine.mode(), Mode::SearchForward);
    assert_eq!(engine.handle(escape()), Command::Back);
    assert_eq!(engine.handle(key('?')), Command::EnterSearch { backward: true });
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

    assert_eq!(engine.handle(key('/')), Command::EnterSearch { backward: false });
    assert_eq!(engine.handle(key('a')), Command::Text("a".into()));
    assert_eq!(engine.handle(key('b')), Command::Text("b".into()));
    assert_eq!(engine.handle(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)), Command::Noop);
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
    assert_eq!(mappings.classify("g"), MappingMatch::Complete(Command::Refresh));
    assert_eq!(mappings.classify("x"), MappingMatch::NoMatch);
}

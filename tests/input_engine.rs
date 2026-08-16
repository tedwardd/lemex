use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lemex::input::{Command, InputEngine, MappingMatch, MappingTable, Mode};

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
fn o_opens_the_selected_media() {
    assert_eq!(InputEngine::default().handle(key('o')), Command::OpenMedia);
}

#[test]
fn n_loads_the_next_feed_page_and_p_the_previous() {
    assert_eq!(InputEngine::default().handle(key('n')), Command::NextPage);
    assert_eq!(
        InputEngine::default().handle(key('p')),
        Command::PreviousPage
    );
}

#[test]
fn angle_brackets_no_longer_flip_pages() {
    let mut engine = InputEngine::default();
    assert_eq!(engine.handle(key('>')), Command::Noop);
    assert_eq!(engine.handle(key('<')), Command::Noop);
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
fn gg_and_g_jump_to_the_top_and_bottom() {
    let mut engine = InputEngine::default();
    assert_eq!(engine.handle(key('g')), Command::Noop, "g waits for gg");
    assert_eq!(engine.handle(key('g')), Command::GoToFirst { count: 1 });
    assert_eq!(
        InputEngine::default().handle(key('G')),
        Command::GoToLast { count: 1 }
    );
}

#[test]
fn jump_commands_apply_counts() {
    let mut engine = InputEngine::default();
    engine.handle(key('5'));
    engine.handle(key('g'));
    assert_eq!(engine.handle(key('g')), Command::GoToFirst { count: 5 });
    engine.handle(key('5'));
    assert_eq!(engine.handle(key('G')), Command::GoToLast { count: 5 });
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
fn escape_abandons_the_command_line_without_touching_the_view() {
    let mut engine = InputEngine::default();
    engine.handle(key(':'));
    engine.handle(key('m'));
    assert_eq!(
        engine.handle(escape()),
        Command::CancelLine,
        "Esc in command mode abandons the line"
    );
    assert_eq!(engine.mode(), Mode::Normal);
    // The app clears the visible text on CancelLine (covered by the
    // application test); the engine's own line buffer is gone.
}

#[test]
fn backspace_emits_a_command_in_insert_mode_too() {
    let mut engine = InputEngine::default();
    engine.handle(key('i'));
    assert_eq!(
        engine.handle(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
        Command::Backspace,
        "insert-mode backspace must reach the app so the draft shrinks"
    );
    assert_eq!(
        engine.handle(escape()),
        Command::Back,
        "insert Esc keeps its semantics"
    );
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
    assert_eq!(engine.handle(escape()), Command::CancelLine);
    assert_eq!(
        engine.handle(key('?')),
        Command::EnterSearch { backward: true }
    );
    assert_eq!(engine.mode(), Mode::SearchBackward);
    assert_eq!(engine.handle(escape()), Command::CancelLine);
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
        Command::Backspace
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

#[test]
fn c_opens_the_communities_shortcut() {
    assert_eq!(
        InputEngine::default().handle(key('C')),
        Command::Communities
    );
}

fn tab() -> KeyEvent {
    KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)
}

/// Drive command mode: `:` (unless already there), then each character, then
/// a final key, returning the final command and the engine's line.
fn type_command(engine: &mut InputEngine, text: &str, final_key: KeyEvent) -> (Command, String) {
    if engine.mode() != Mode::Command {
        engine.handle(key(':'));
    }
    for character in text.chars() {
        engine.handle(key(character));
    }
    let command = engine.handle(final_key);
    (command, engine.line().to_owned())
}

#[test]
fn tab_completes_a_single_command_fully() {
    let mut engine = InputEngine::default().with_completions(vec![
        "communities".into(),
        "community".into(),
        "feed".into(),
    ]);
    let (command, line) = type_command(&mut engine, "feed", tab());
    assert_eq!(
        command,
        Command::CompleteLine("feed".into()),
        "a unique match completes to the full command"
    );
    assert_eq!(line, "feed");
    // The completed line still submits as a command.
    let (submitted, line) = type_command(&mut engine, "", enter());
    assert_eq!(submitted, Command::SubmitLine("feed".into()));
    assert!(line.is_empty(), "submitting clears the line");
}

#[test]
fn tab_completes_to_the_longest_common_prefix() {
    let mut engine = InputEngine::default().with_completions(vec![
        "communities".into(),
        "community".into(),
        "feed".into(),
    ]);
    let (command, line) = type_command(&mut engine, "comm", tab());
    assert_eq!(
        command,
        Command::CompleteLine("communit".into()),
        "two matches share the prefix 'communit'"
    );
    assert_eq!(line, "communit");
    // A second press cannot extend past the common prefix.
    let (second, line) = type_command(&mut engine, "", tab());
    assert_eq!(second, Command::CompleteLine("communit".into()));
    assert_eq!(line, "communit");
}

#[test]
fn tab_without_matches_leaves_the_line_alone() {
    let mut engine = InputEngine::default().with_completions(vec!["feed".into()]);
    let (command, line) = type_command(&mut engine, "xyz", tab());
    assert_eq!(command, Command::CompleteLine("xyz".into()));
    assert_eq!(line, "xyz", "no match: the typed text is untouched");
}

#[test]
fn completion_is_case_insensitive_and_preserves_spelling() {
    let mut engine = InputEngine::default().with_completions(vec!["Communities".into()]);
    let (command, line) = type_command(&mut engine, "comm", tab());
    assert_eq!(command, Command::CompleteLine("Communities".into()));
    assert_eq!(line, "Communities", "the canonical spelling is inserted");
}

#[test]
fn tab_does_nothing_without_completions_configured() {
    let mut engine = InputEngine::default();
    let (command, line) = type_command(&mut engine, "feed", tab());
    assert_eq!(command, Command::CompleteLine("feed".into()));
    assert_eq!(line, "feed", "no completions: the line is untouched");
}

#[test]
fn z_and_Z_toggle_comment_threads() {
    let mut engine = InputEngine::default();
    assert_eq!(engine.handle(key('z')), Command::ToggleCommentThread);
    assert_eq!(engine.handle(key('Z')), Command::CollapseAllCommentThreads);
}

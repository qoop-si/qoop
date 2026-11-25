//! The window headless, on GPUI's test platform: home from fixture rows, the prefix keys, New
//! work's keyboard path, and the input method in the text field.

use gpui::{AppContext, Entity, EntityInputHandler, TestAppContext, VisualTestContext};
use qoop_runtime::control::{ControlRequest, ControlResponse, LinkState};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::app::{Desk, Page};
use crate::link::{Down, Latest, Up};
use crate::state::tests::fixture;

fn open(cx: &mut TestAppContext) -> (Entity<Desk>, VisualTestContext, UnboundedReceiver<Up>) {
    // Every setting its default (prefix ctrl-a), never the person's own file.
    open_with(cx, std::path::Path::new("/nonexistent/config.toml"))
}

fn open_with(
    cx: &mut TestAppContext,
    config: &std::path::Path,
) -> (Entity<Desk>, VisualTestContext, UnboundedReceiver<Up>) {
    let (up, rx) = tokio::sync::mpsc::unbounded_channel();
    let config = qoop_tui::config::read_from(config);
    let window = cx.add_window(|window, cx| Desk::new(up, Latest::default(), &config, window, cx));
    let mut vcx = VisualTestContext::from_window(window.into(), cx);
    let desk = window.root(&mut vcx).expect("the window's view");
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.apply(Down::Connected, window, cx);
            d.config_path = std::path::PathBuf::from("/nonexistent/qoop/config.toml");
            d.pet_path = std::path::PathBuf::from("/nonexistent/qoop/avatar.json");
            d.model = fixture();
            d.note_hand();
            d.listed = true;
            d.fresh = true;
            d.now = 2000;
            cx.notify();
        })
    });
    // The pointer on the page, not over the column (where it would hold the list still).
    vcx.simulate_mouse_move(
        gpui::point(gpui::px(900.), gpui::px(600.)),
        None,
        gpui::Modifiers::none(),
    );
    vcx.run_until_parked();
    (desk, vcx, rx)
}

fn sent(rx: &mut UnboundedReceiver<Up>) -> Vec<Up> {
    let mut out = Vec::new();
    while let Ok(up) = rx.try_recv() {
        out.push(up);
    }
    out
}

/// The column lists every space; one where something waits is open with all its agents, a
/// quiet one is its heading alone, and the space of the agent on screen opens. Qass's own row
/// is never listed. Opened, a space lists every agent, sleepers included.
#[gpui::test]
fn the_sidebar_lists_what_waits_the_open_agent_and_folds_the_rest(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    let y = |vcx: &mut VisualTestContext, what: &'static str| {
        vcx.debug_bounds(what)
            .unwrap_or_else(|| panic!("{what} is not drawn"))
            .origin
            .y
    };
    let remote = 1i64 << 40 | 1;
    let remote_space: &'static str = format!("side-space-{remote}").leak();
    // acme-web: 2 waits, so it is open with all three; api and staging are their headings.
    let order = [
        "side-space-1",
        "tree-row-1",
        "tree-row-2",
        "tree-row-3",
        "side-space-2",
        remote_space,
    ];
    let ys: Vec<_> = order.iter().map(|s| y(&mut vcx, s)).collect();
    assert!(ys.windows(2).all(|w| w[0] < w[1]), "{order:?} at {ys:?}");
    for folded in ["tree-row-4", "tree-row-5", "tree-row-9"] {
        assert!(
            vcx.debug_bounds(folded).is_none(),
            "{folded} is folded or qass's"
        );
    }
    // The agent on screen opens its space.
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(5), window, cx)));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("tree-row-5").is_some(), "the open agent");
    assert!(vcx.debug_bounds("tree-row-4").is_some(), "its space whole");
    // Opened, the remote space lists its sleeper too.
    vcx.update(|_, cx| {
        desk.update(cx, |d, cx| {
            d.folds.insert(remote, true);
            cx.notify();
        })
    });
    vcx.run_until_parked();
    for listed in ["tree-row-2164260865", "tree-row-2164260866"] {
        assert!(vcx.debug_bounds(listed).is_some(), "{listed} once opened");
    }
    assert!(
        vcx.debug_bounds("tree-row-2164260867").is_none(),
        "the second pane of a sleeping tab is no agent"
    );
}

/// The computers sit pinned under the spaces: This Mac and every other machine, with how it is
/// reached; Retry now only where it can help, and no tag for This Mac's own agents.
#[gpui::test]
fn the_computers_are_pinned_under_the_spaces_with_their_reach(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    let parked = 2i64 << 40 | 1;
    vcx.update(|_, cx| {
        desk.update(cx, |d, cx| {
            let mut idle = crate::state::tests::space(parked, "cdev:scratch");
            idle.link = Some(LinkState::Parked);
            d.model.spaces.push(idle);
            cx.notify();
        })
    });
    vcx.run_until_parked();
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.page),
        Page::Inbox,
        "the window opens on it"
    );
    let y = |vcx: &mut VisualTestContext, what: &'static str| {
        vcx.debug_bounds(what)
            .unwrap_or_else(|| panic!("{what} is not drawn"))
            .origin
            .y
    };
    let remote_space: &'static str = format!("side-space-{}", 1i64 << 40 | 1).leak();
    let order = [remote_space, "computer-0", "machine-1", "machine-2"];
    let ys: Vec<_> = order.iter().map(|s| y(&mut vcx, s)).collect();
    assert!(ys.windows(2).all(|w| w[0] < w[1]), "{order:?} at {ys:?}");
    assert!(
        vcx.debug_bounds("machine-0").is_none(),
        "This Mac has its own line"
    );
    assert!(
        vcx.debug_bounds("retry-1").is_none(),
        "online: nothing to retry"
    );
    assert!(
        vcx.debug_bounds("retry-2").is_none(),
        "parked: nothing to retry"
    );
    assert_eq!(
        desk.read_with(&vcx, |d, _| crate::state::reach_words(
            d.machine_state(1),
            d.zone
        )),
        "online"
    );
    // A computer's name opens its page, which lists its agents as last read.
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Computer(1), window, cx)));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("computer-page").is_some());
    assert!(vcx.debug_bounds("computer-agent-2164260865").is_some());
    assert!(
        vcx.debug_bounds("computer-agent-1").is_none(),
        "only that computer's"
    );
}

#[gpui::test]
fn ctrl_a_c_opens_new_work_and_ctrl_a_i_the_inbox(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    vcx.simulate_keystrokes("ctrl-a c");
    assert!(
        desk.read_with(&vcx, |d, _| d.sheet.is_some()),
        "New work is open"
    );
    vcx.simulate_keystrokes("escape");
    assert!(
        desk.read_with(&vcx, |d, _| d.sheet.is_none()),
        "Escape closes it"
    );
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(1), window, cx)));
    vcx.run_until_parked();
    vcx.simulate_keystrokes("ctrl-a i");
    assert_eq!(desk.read_with(&vcx, |d, _| d.page), Page::Inbox);
    assert!(
        vcx.debug_bounds("inbox-card-t2").is_some(),
        "the agent that needs you has its card in the Inbox"
    );
    // Again: back to the page it was opened from.
    vcx.simulate_keystrokes("ctrl-a i");
    assert_eq!(desk.read_with(&vcx, |d, _| d.page), Page::Session(1));
}

/// A `Scrollback` out when the connection drops is never answered: a new connection forgets
/// it, so the next wheel step asks again instead of waiting for it.
#[gpui::test]
fn a_new_connection_forgets_the_scrollback_ask_out(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(1), window, cx)));
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            let term = d.term.as_mut().expect("the session's terminal");
            term.asked = Some(("lost".into(), 23));
            d.apply(Down::Connected, window, cx);
        })
    });
    let asked = desk.read_with(&vcx, |d, _| d.term.as_ref().map(|t| t.asked.clone()));
    assert_eq!(asked, Some(None));
}

#[gpui::test]
fn the_prefix_twice_sends_it_to_the_terminal(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(1), window, cx)));
    vcx.run_until_parked();
    sent(&mut rx);
    vcx.simulate_keystrokes("ctrl-a ctrl-a");
    let typed: Vec<Vec<u8>> = sent(&mut rx)
        .into_iter()
        .filter_map(|u| match u {
            Up::Input(1, bytes) => Some(bytes),
            _ => None,
        })
        .collect();
    assert_eq!(typed, vec![vec![0x01]]);
    // A key alone goes to the terminal as typed.
    vcx.simulate_keystrokes("x");
    assert!(
        sent(&mut rx)
            .iter()
            .any(|u| matches!(u, Up::Input(1, b) if b == b"x"))
    );
}

/// Shift+Enter in the terminal on screen reaches the agent as the kitty protocol's `CSI 13;2u`
/// once the program pushed the flags, and as a plain return while it has not; Option+Enter is
/// `ESC CR` and Ctrl+J a line feed either way. Nothing in the window takes them first.
#[gpui::test]
fn new_line_keys_reach_the_terminal_the_way_a_terminal_sends_them(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(1), window, cx)));
    vcx.run_until_parked();
    let mut press = |vcx: &mut VisualTestContext, keys: &str| -> Vec<u8> {
        sent(&mut rx);
        vcx.simulate_keystrokes(keys);
        sent(&mut rx)
            .into_iter()
            .find_map(|u| match u {
                Up::Input(1, bytes) => Some(bytes),
                _ => None,
            })
            .unwrap_or_default()
    };
    let flags = |vcx: &mut VisualTestContext, kitty: u8| {
        vcx.update(|_, cx| {
            desk.update(cx, |d, _| {
                let term = d.term.as_mut().expect("the session's terminal");
                let mut screen = term
                    .screen
                    .as_deref()
                    .cloned()
                    .unwrap_or_else(|| crate::session::page_of(Vec::new(), 80, 24));
                screen.modes.kitty_keyboard = kitty;
                term.screen = Some(std::rc::Rc::new(screen));
            })
        })
    };
    flags(&mut vcx, 0);
    assert_eq!(press(&mut vcx, "shift-enter"), b"\r");
    assert_eq!(press(&mut vcx, "alt-enter"), b"\x1b\r");
    assert_eq!(press(&mut vcx, "ctrl-j"), b"\n");
    flags(&mut vcx, 1);
    assert_eq!(press(&mut vcx, "shift-enter"), b"\x1b[13;2u");
    assert_eq!(press(&mut vcx, "alt-enter"), b"\x1b[13;3u");
    assert_eq!(press(&mut vcx, "enter"), b"\r");
    assert_eq!(press(&mut vcx, "shift-tab"), b"\x1b[Z");
    assert_eq!(press(&mut vcx, "ctrl-right"), b"\x1b[1;5C");
}

#[gpui::test]
fn new_work_by_keyboard_empty_enter_sends_nothing_then_starts(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    vcx.simulate_keystrokes("ctrl-a c");
    sent(&mut rx);
    vcx.simulate_keystrokes("enter");
    assert!(
        !sent(&mut rx).iter().any(|u| matches!(u, Up::Request(_))),
        "an empty task starts nothing"
    );
    vcx.simulate_input("giriş sayfasını düzelt");
    vcx.simulate_keystrokes("enter");
    let asked = sent(&mut rx)
        .into_iter()
        .find_map(|u| match u {
            Up::Request(r) => match *r {
                ControlRequest::CreateTerminal { id, workspace, .. } => Some((id, workspace)),
                _ => None,
            },
            _ => None,
        })
        .expect("a terminal asked for");
    assert_eq!(asked.1, Some(1), "in the first space");
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.apply(
                Down::Answer(Box::new(ControlResponse::TerminalCreated {
                    id: asked.0.clone(),
                    terminal: 12,
                })),
                window,
                cx,
            )
        })
    });
    let typed = sent(&mut rx)
        .into_iter()
        .find_map(|u| match u {
            Up::Input(12, bytes) => Some(String::from_utf8(bytes).expect("utf8")),
            _ => None,
        })
        .expect("the line typed");
    assert_eq!(
        typed,
        "qoop worktree new feat/giris-sayfasini-duzelt --space 1 -- claude 'giriş sayfasını düzelt' && exit\r"
    );
    assert!(
        desk.read_with(&vcx, |d, _| d.sheet.as_ref().is_some_and(|s| s.started)),
        "the sheet stays, waiting for the agent"
    );
}

#[gpui::test]
fn the_field_marks_then_commits_what_the_input_method_composes(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    vcx.simulate_keystrokes("ctrl-a c");
    let field = desk.read_with(&vcx, |d, _| d.sheet.as_ref().expect("open").task.clone());
    vcx.update(|window, cx| {
        field.update(cx, |f, cx| {
            f.replace_and_mark_text_in_range(None, "ka", None, window, cx);
        })
    });
    vcx.run_until_parked();
    field.read_with(&vcx, |f, _| {
        assert_eq!(f.text, "ka");
        assert_eq!(f.marked, Some(0..2), "drawn underlined while it composes");
    });
    vcx.update(|window, cx| {
        field.update(cx, |f, cx| f.replace_text_in_range(None, "か", window, cx));
    });
    field.read_with(&vcx, |f, _| {
        assert_eq!(f.text, "か", "the commit replaces the marked text");
        assert_eq!(f.marked, None);
    });
    // Undo right after the commit goes back to before the composition, never to the romaji.
    vcx.simulate_keystrokes("cmd-z");
    field.read_with(&vcx, |f, _| assert_eq!(f.text, ""));
    // Undo of the last edit, and a selection cut.
    vcx.simulate_input("x");
    vcx.simulate_input("y");
    vcx.simulate_keystrokes("cmd-z");
    field.read_with(&vcx, |f, _| assert_eq!(f.text, "x"));
    vcx.simulate_keystrokes("cmd-a backspace");
    field.read_with(&vcx, |f, _| assert_eq!(f.text, ""));
}

/// In a box whose words go out on Enter, Shift+Enter, Option+Enter and Ctrl+J type a new line
/// (the box grows a line, the arrows move between lines) and only a plain Enter sends.
#[gpui::test]
fn shift_enter_types_a_new_line_and_enter_sends(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    vcx.simulate_keystrokes("ctrl-a c");
    sent(&mut rx);
    let field = desk.read_with(&vcx, |d, _| d.sheet.as_ref().expect("open").task.clone());
    let shown = |vcx: &mut VisualTestContext| {
        vcx.run_until_parked();
        field.read_with(vcx, |f, _| {
            let height = f.drawn().expect("painted").size.height;
            (f.text.clone(), height / f.line_height().expect("painted"))
        })
    };
    vcx.simulate_input("ab");
    vcx.simulate_keystrokes("shift-enter");
    vcx.simulate_input("ab");
    assert_eq!(shown(&mut vcx), ("ab\nab".to_string(), 2.));
    assert!(
        !sent(&mut rx).iter().any(|u| matches!(u, Up::Request(_))),
        "Shift+Enter sends nothing"
    );
    // The arrows go between lines, keeping the column: up to the first, down to the second.
    vcx.simulate_keystrokes("up");
    vcx.simulate_input("X");
    vcx.simulate_keystrokes("down");
    vcx.simulate_input("Y");
    assert_eq!(shown(&mut vcx).0, "abX\nabY");
    vcx.simulate_keystrokes("alt-enter ctrl-j");
    assert_eq!(shown(&mut vcx), ("abX\nabY\n\n".to_string(), 4.));
    assert!(!sent(&mut rx).iter().any(|u| matches!(u, Up::Request(_))));
    vcx.simulate_keystrokes("backspace backspace enter");
    assert!(
        sent(&mut rx).iter().any(|u| matches!(
            u,
            Up::Request(r) if matches!(**r, ControlRequest::CreateTerminal { .. })
        )),
        "Enter starts it"
    );
}

/// A field wraps what is typed in it to its width and grows a line at a time, up to six lines;
/// past them it scrolls, keeping the caret's line in view. Nothing runs past its edge.
#[gpui::test]
fn the_field_wraps_and_grows_to_six_lines(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    vcx.simulate_keystrokes("ctrl-a c");
    let field = desk.read_with(&vcx, |d, _| d.sheet.as_ref().expect("open").task.clone());
    let fill = |vcx: &mut VisualTestContext, text: &str| {
        vcx.update(|_, cx| {
            field.update(cx, |f, cx| {
                f.set_text(text);
                cx.notify();
            })
        });
        vcx.run_until_parked();
        field.read_with(vcx, |f, _| {
            let bounds = f.drawn().expect("painted");
            (
                f.rows,
                f.top,
                bounds.size.height / f.line_height().expect("painted"),
            )
        })
    };
    // Empty (the hint), one word, and a word longer than the field.
    assert_eq!(fill(&mut vcx, ""), (1, 0, 1.));
    assert_eq!(fill(&mut vcx, "Fix the login"), (1, 0, 1.));
    let (rows, _, shown) = fill(&mut vcx, &"x".repeat(400));
    assert!(
        rows > 1 && shown == rows.min(6) as f32,
        "broken at the edge: {rows} {shown}"
    );
    // A long prompt: wrapped, two to six lines high.
    let words = "Check why the login test fails on CI but not here, then fix it ";
    let (rows, top, shown) = fill(&mut vcx, &words.repeat(3));
    assert!(
        (2..=6).contains(&rows) && top == 0 && shown == rows as f32,
        "{rows} {shown}"
    );
    // Past six lines: six high, scrolled so the caret (at the end) is on the last one shown.
    let (rows, top, shown) = fill(&mut vcx, &words.repeat(40));
    assert!(rows > 6, "{rows}");
    assert_eq!((shown, top), (6., rows - 6));
}

/// Option moves and deletes by word, Cmd+Backspace deletes to the start, Shift extends.
#[gpui::test]
fn the_field_moves_and_deletes_by_word(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    vcx.simulate_keystrokes("ctrl-a c");
    let field = desk.read_with(&vcx, |d, _| d.sheet.as_ref().expect("open").task.clone());
    vcx.update(|_, cx| field.update(cx, |f, _| f.set_text("fix the-login test")));
    let state =
        |vcx: &mut VisualTestContext| field.read_with(vcx, |f, _| (f.text.clone(), f.selection()));
    vcx.simulate_keystrokes("alt-left");
    assert_eq!(state(&mut vcx).1, 14..14, "before `test`");
    vcx.simulate_keystrokes("alt-left");
    assert_eq!(state(&mut vcx).1, 8..8, "before `login`");
    vcx.simulate_keystrokes("alt-left alt-left");
    assert_eq!(state(&mut vcx).1, 0..0);
    vcx.simulate_keystrokes("alt-right");
    assert_eq!(state(&mut vcx).1, 3..3, "after `fix`");
    vcx.simulate_keystrokes("alt-shift-right");
    assert_eq!(state(&mut vcx).1, 3..7, "`the` selected");
    vcx.simulate_keystrokes("cmd-right alt-backspace");
    assert_eq!(state(&mut vcx).0, "fix the-login ");
    vcx.simulate_keystrokes("alt-backspace");
    assert_eq!(state(&mut vcx).0, "fix the-");
    vcx.simulate_keystrokes("cmd-backspace");
    assert_eq!(state(&mut vcx), (String::new(), 0..0));
    vcx.simulate_keystrokes("cmd-z");
    assert_eq!(state(&mut vcx).0, "fix the-", "undone");
}

/// A click on a wrapped field's second line puts the caret on that line.
#[gpui::test]
fn a_click_on_a_wrapped_line_puts_the_caret_there(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    vcx.simulate_keystrokes("ctrl-a c");
    let field = desk.read_with(&vcx, |d, _| d.sheet.as_ref().expect("open").task.clone());
    let text = "Check why the login test fails on CI but not here ".repeat(4);
    vcx.update(|_, cx| {
        field.update(cx, |f, cx| {
            f.set_text(&text);
            cx.notify();
        })
    });
    vcx.run_until_parked();
    let (bounds, line) =
        field.read_with(&vcx, |f, _| (f.drawn().unwrap(), f.line_height().unwrap()));
    vcx.simulate_click(
        gpui::point(bounds.left() + gpui::px(2.), bounds.top() + line * 1.5),
        gpui::Modifiers::default(),
    );
    let (at, rows) = field.read_with(&vcx, |f, _| (f.selection().start, f.rows));
    assert!(rows > 1, "{rows}");
    assert!(
        at > 0 && at < text.len() / 2,
        "the second line's start, not the first: {at}"
    );
}

/// New work offers the `[agents]` names of the config too; one qoop cannot hand the task to
/// starts as typed, and the sheet says so.
#[gpui::test]
fn new_work_offers_the_config_agents(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "[agents]\nfast = \"claude --model opus\"\naider = \"aider --model x\"\n",
    )
    .expect("config");
    let (desk, mut vcx, mut rx) = open_with(cx, &path);
    let names = desk.read_with(&vcx, |d, _| {
        d.offers.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>()
    });
    assert_eq!(names, ["claude", "codex", "opencode", "fast", "aider"]);
    vcx.simulate_keystrokes("ctrl-a c");
    vcx.simulate_input("fix it");
    vcx.update(|_, cx| {
        desk.update(cx, |d, cx| {
            d.sheet.as_mut().expect("open").agent = 4;
            cx.notify();
        })
    });
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("new-work-note").is_some(),
        "the note that aider starts without the task"
    );
    sent(&mut rx);
    vcx.simulate_keystrokes("enter");
    let id = sent(&mut rx)
        .into_iter()
        .find_map(|u| match u {
            Up::Request(r) => match *r {
                ControlRequest::CreateTerminal { id, .. } => Some(id),
                _ => None,
            },
            _ => None,
        })
        .expect("a terminal asked for");
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.apply(
                Down::Answer(Box::new(ControlResponse::TerminalCreated {
                    id,
                    terminal: 15,
                })),
                window,
                cx,
            )
        })
    });
    let typed = sent(&mut rx)
        .into_iter()
        .find_map(|u| match u {
            Up::Input(15, bytes) => Some(String::from_utf8(bytes).expect("utf8")),
            _ => None,
        })
        .expect("the line typed");
    assert_eq!(
        typed,
        "qoop worktree new feat/fix-it --space 1 -- aider --model x && exit\r"
    );
}

/// A New work whose sheet was closed while it started still gets its line, and one asked
/// for before a reconnect is asked again under the same id (the daemon answers a known id
/// with its first answer, so there is still one tab).
#[gpui::test]
fn new_work_survives_escape_and_a_reconnect(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    vcx.simulate_keystrokes("ctrl-a c");
    vcx.simulate_input("fix it");
    vcx.simulate_keystrokes("enter");
    let created = |ups: Vec<Up>| {
        ups.into_iter().find_map(|u| match u {
            Up::Request(r) => match *r {
                ControlRequest::CreateTerminal { id, .. } => Some(id),
                _ => None,
            },
            _ => None,
        })
    };
    let id = created(sent(&mut rx)).expect("a terminal asked for");
    vcx.simulate_keystrokes("escape");
    assert!(desk.read_with(&vcx, |d, _| d.sheet.is_none()));
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.apply(Down::Disconnected("gone".into()), window, cx);
            d.apply(Down::Connected, window, cx);
        })
    });
    assert_eq!(
        created(sent(&mut rx)).as_deref(),
        Some(id.as_str()),
        "asked again"
    );
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.apply(
                Down::Answer(Box::new(ControlResponse::TerminalCreated {
                    id: id.clone(),
                    terminal: 14,
                })),
                window,
                cx,
            )
        })
    });
    let typed = sent(&mut rx).into_iter().any(|u| {
        matches!(u, Up::Input(14, bytes) if bytes.starts_with(b"qoop worktree new feat/fix-it "))
    });
    assert!(typed, "the line is typed with the sheet closed");
    assert!(desk.read_with(&vcx, |d, _| d.pending.is_none()));
    assert!(
        desk.read_with(&vcx, |d, _| d.work_draft.is_none()),
        "a started task is no draft"
    );
}

/// A task typed into New work and closed with Escape comes back on the next open, and the
/// focus goes back to the terminal that opened the sheet (STATE-41, WEB-06).
#[gpui::test]
fn new_work_keeps_its_draft_and_gives_the_focus_back(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(1), window, cx)));
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        let term = desk.read(cx).term_focus.clone();
        window.focus(&term, cx);
    });
    vcx.simulate_keystrokes("ctrl-a c");
    vcx.simulate_input("fix the login page");
    vcx.simulate_keystrokes("escape");
    assert!(desk.read_with(&vcx, |d, _| d.sheet.is_none()));
    assert!(
        vcx.update(|window, cx| desk.read(cx).term_focus.is_focused(window)),
        "the terminal has the focus again"
    );
    vcx.simulate_keystrokes("ctrl-a c");
    let task = desk.read_with(&vcx, |d, cx| {
        d.sheet.as_ref().expect("open").task.read(cx).text.clone()
    });
    assert_eq!(task, "fix the login page", "the draft is back");
    // The close button keeps it too.
    let at = vcx
        .debug_bounds("sheet-close")
        .expect("the close button")
        .center();
    vcx.simulate_click(at, gpui::Modifiers::none());
    assert!(desk.read_with(&vcx, |d, _| d.sheet.is_none() && d.work_draft.is_some()));
    // Cancel, as on the schedule sheet, drops it.
    vcx.simulate_keystrokes("ctrl-a c");
    let at = vcx
        .debug_bounds("sheet-cancel")
        .expect("the Cancel button")
        .center();
    vcx.simulate_click(at, gpui::Modifiers::none());
    assert!(
        desk.read_with(&vcx, |d, _| d.sheet.is_none() && d.work_draft.is_none()),
        "Cancel keeps no draft"
    );
}

/// A field made before a live theme switch draws in the new theme: fields read the theme the
/// window draws in now, never one kept from when they were made.
#[gpui::test]
fn a_field_made_before_a_theme_switch_takes_the_new_theme(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    vcx.simulate_keystrokes("ctrl-a c");
    assert!(desk.read_with(&vcx, |d, _| d.sheet.is_some()));
    for dark in [false, true] {
        vcx.update(|_, cx| {
            desk.update(cx, |d, cx| {
                d.dark = dark;
                cx.notify();
            })
        });
        vcx.run_until_parked();
        assert_eq!(
            vcx.update(|_, cx| crate::theme::current(cx)),
            crate::theme::Theme::pick(dark),
            "the field's theme after a switch to dark: {dark}"
        );
    }
}

/// A click on a control half in view focuses it and leaves its list where it is: only the
/// keyboard's focus scrolls, so the button never moves away under the pointer mid-click.
#[gpui::test]
fn a_click_at_a_lists_edge_never_scrolls_it(cx: &mut TestAppContext) {
    use gpui::{Context, Render, Role, Window, div, prelude::*, px};

    /// A 100 px list of five 40 px buttons: the third shows only its top half.
    struct List;
    impl Render for List {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            crate::ui::scroll(
                div()
                    .id("list")
                    .h(px(100.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .children((0..5usize).map(|i| {
                        crate::ui::control(("item", i), Role::Button, "item", &crate::theme::DARK)
                            .flex_none()
                            .h(px(40.))
                            .debug_selector(move || format!("item-{i}"))
                            .on_click(|_, _, _| {})
                    })),
            )
        }
    }

    let window = cx.add_window(|_, _| List);
    let mut vcx = VisualTestContext::from_window(window.into(), cx);
    vcx.run_until_parked();
    let before = vcx.debug_bounds("item-2").expect("the third button");
    let at = gpui::point(before.center().x, before.top() + px(10.));
    vcx.simulate_click(at, gpui::Modifiers::none());
    vcx.run_until_parked();
    assert!(
        vcx.update(|window, cx| window.focused(cx).is_some()),
        "the click focused it"
    );
    assert_eq!(
        vcx.debug_bounds("item-2").expect("the third button").top(),
        before.top(),
        "the list stayed under the pointer"
    );
}

/// A Tab stop below the fold scrolls its list into view: the settings list on a short window,
/// Tab until its last switch has the focus, and it is on screen.
#[gpui::test]
fn tab_scrolls_a_stop_below_the_fold_into_view(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    vcx.simulate_resize(gpui::size(gpui::px(1100.), gpui::px(480.)));
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Settings, window, cx)));
    vcx.run_until_parked();
    let height = vcx.update(|window, _| window.viewport_size().height);
    let last = |vcx: &mut VisualTestContext| {
        vcx.debug_bounds("setting-telemetry")
            .expect("the Usage counts switch")
    };
    assert!(
        last(&mut vcx).top() > height,
        "below the fold at first: {:?} in {height:?}",
        last(&mut vcx)
    );
    let mut shown = false;
    for _ in 0..120 {
        vcx.simulate_keystrokes("tab");
        vcx.run_until_parked();
        let at = last(&mut vcx);
        if at.top() >= gpui::px(0.) && at.bottom() <= height {
            shown = true;
            break;
        }
    }
    assert!(shown, "Tab brought it into view: {:?}", last(&mut vcx));
}

/// Escape on the Inbox goes back to the session it was opened from and never reaches the agent
/// in the terminal: an Escape typed into Claude Code interrupts its turn.
#[gpui::test]
fn escape_leaves_the_inbox_before_the_terminal_hears_it(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(1), window, cx)));
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        let term = desk.read(cx).term_focus.clone();
        window.focus(&term, cx);
    });
    vcx.simulate_keystrokes("ctrl-a i");
    assert_eq!(desk.read_with(&vcx, |d, _| d.page), Page::Inbox);
    sent(&mut rx);
    vcx.simulate_keystrokes("escape");
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.page),
        Page::Session(1),
        "Escape goes back"
    );
    assert!(
        !sent(&mut rx).iter().any(|u| matches!(u, Up::Input(..))),
        "nothing typed into the terminal"
    );
    // On the session, Escape is the agent's again.
    vcx.simulate_keystrokes("escape");
    assert!(
        sent(&mut rx)
            .iter()
            .any(|u| matches!(u, Up::Input(1, b) if b == b"\x1b"))
    );
}

/// The session open before the Inbox keeps its terminal alive behind it: the pane is not hidden
/// and shown again (no blank frame, no lost scrollback), and coming back needs no new subscription.
#[gpui::test]
fn the_open_sessions_terminal_stays_alive_across_the_inbox(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(1), window, cx)));
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        desk.update(cx, |d, _| {
            d.term.as_mut().expect("the pane").scroll = 7;
        })
    });
    sent(&mut rx);
    vcx.simulate_keystrokes("ctrl-a i");
    assert_eq!(desk.read_with(&vcx, |d, _| d.page), Page::Inbox);
    assert!(
        desk.read_with(&vcx, |d, _| d.term.as_ref().map(|t| (t.terminal, t.scroll)))
            == Some((1, 7)),
        "the pane and its scroll are kept"
    );
    vcx.simulate_keystrokes("ctrl-a i");
    let ups = sent(&mut rx);
    assert!(
        !ups.iter().any(|u| matches!(u, Up::Hide(_) | Up::Show(_))),
        "never hidden or shown again"
    );
    assert!(
        !ups.iter().any(|u| matches!(u, Up::Resize(..))),
        "the Inbox never resizes the terminal behind it"
    );
    assert_eq!(desk.read_with(&vcx, |d, _| d.page), Page::Session(1));
}

/// Cmd-J (the next agent that waits) and Cmd-N (New agent) are the app's: the terminal under
/// them never hears them, whatever the layout, and they act from a session page.
#[gpui::test]
fn cmd_j_and_cmd_n_never_reach_the_terminal(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(1), window, cx)));
    vcx.run_until_parked();
    vcx.update(|window, cx| {
        let term = desk.read(cx).term_focus.clone();
        window.focus(&term, cx);
    });
    sent(&mut rx);
    vcx.simulate_keystrokes("cmd-j");
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.page),
        Page::Session(2),
        "the agent that waits opens"
    );
    vcx.update(|window, cx| {
        let term = desk.read(cx).term_focus.clone();
        window.focus(&term, cx);
    });
    vcx.simulate_keystrokes("cmd-n");
    assert!(
        desk.read_with(&vcx, |d, _| d.sheet.is_some()),
        "New agent opens"
    );
    vcx.simulate_keystrokes("escape");
    assert!(
        !sent(&mut rx).iter().any(|u| matches!(u, Up::Input(..))),
        "neither chord reached a terminal"
    );
}

/// The schedule sheet: a close button and Cancel to leave by mouse, Tab and Shift-Tab kept
/// inside it, the focus back on New schedule when it closes, and what was typed kept by
/// Escape and dropped by Cancel.
#[gpui::test]
fn the_schedule_sheet_traps_the_focus_and_keeps_its_draft(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Schedules, window, cx)));
    vcx.run_until_parked();
    let click = |vcx: &mut VisualTestContext, what: &'static str| {
        let at = vcx
            .debug_bounds(what)
            .unwrap_or_else(|| panic!("{what} is not drawn"))
            .center();
        vcx.simulate_click(at, gpui::Modifiers::none());
    };
    click(&mut vcx, "new-schedule");
    let (frame, back) = desk.read_with(&vcx, |d, _| {
        let sheet = d.schedule_page.sheet.as_ref().expect("open");
        (
            sheet.frame.clone(),
            sheet.back.clone().expect("New schedule had the focus"),
        )
    });
    assert!(vcx.debug_bounds("schedule-sheet-close").is_some());
    assert!(vcx.debug_bounds("schedule-sheet-cancel").is_some());
    for keys in ["tab", "shift-tab"] {
        for _ in 0..20 {
            vcx.simulate_keystrokes(keys);
            assert!(
                vcx.update(|window, cx| frame.contains_focused(window, cx)),
                "{keys} left the sheet"
            );
        }
    }
    // Back on the name field, words typed, Escape: closed, the focus on New schedule.
    vcx.update(|window, cx| {
        let name = desk
            .read(cx)
            .schedule_page
            .sheet
            .as_ref()
            .expect("open")
            .name
            .clone();
        let focus = name.read(cx).focus.clone();
        window.focus(&focus, cx);
    });
    vcx.simulate_input("nightly-tests");
    vcx.simulate_keystrokes("escape");
    assert!(desk.read_with(&vcx, |d, _| d.schedule_page.sheet.is_none()));
    assert!(vcx.update(|window, _| back.is_focused(window)));
    click(&mut vcx, "new-schedule");
    let (name, restored) = desk.read_with(&vcx, |d, cx| {
        let sheet = d.schedule_page.sheet.as_ref().expect("open");
        (sheet.name.read(cx).text.clone(), sheet.restored)
    });
    assert_eq!(name, "nightly-tests", "the draft is back");
    assert!(restored, "and says it is not saved");
    click(&mut vcx, "schedule-sheet-cancel");
    assert!(desk.read_with(&vcx, |d, _| d.schedule_page.sheet.is_none()
        && d.schedule_page.drafts.is_empty()));
    click(&mut vcx, "new-schedule");
    let name = desk.read_with(&vcx, |d, cx| {
        d.schedule_page
            .sheet
            .as_ref()
            .expect("open")
            .name
            .read(cx)
            .text
            .clone()
    });
    assert_eq!(name, "", "Cancel dropped it");
}

fn request_of(ups: &[Up], pick: impl Fn(&ControlRequest) -> bool) -> Option<ControlRequest> {
    ups.iter().find_map(|u| match u {
        Up::Request(r) if pick(r) => Some((**r).clone()),
        _ => None,
    })
}

fn answer(desk: &Entity<Desk>, vcx: &mut VisualTestContext, response: ControlResponse) {
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.apply(Down::Answer(Box::new(response)), window, cx);
            cx.notify();
        })
    });
    vcx.run_until_parked();
}

/// A split tab as the daemon keeps it: both panes are shown, each terminal is told the size
/// of its own pane, and the wider share gets the wider pane.
#[gpui::test]
fn a_split_tab_lays_out_its_panes_and_sizes_each(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.model.splits = vec![crate::state::tests::split(1, 3)];
            d.open(Page::Session(1), window, cx);
        })
    });
    vcx.run_until_parked();
    let ups = sent(&mut rx);
    let shown: Vec<u32> = ups
        .iter()
        .filter_map(|u| match u {
            Up::Show(t) => Some(t.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(shown, [1, 3], "both panes of the tab");
    let size_of = |terminal: u32| {
        ups.iter()
            .rev()
            .find_map(|u| match u {
                Up::Resize(t, cols, rows) if *t == terminal => Some((*cols, *rows)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("terminal {terminal} was told no size: {ups:?}"))
    };
    let (left, right) = (size_of(1), size_of(3));
    assert!(
        left.0 > right.0,
        "60 of 100 is the wider: {left:?} {right:?}"
    );
    assert_eq!(left.1, right.1, "side by side, the same height");
}

/// The strip shows the answer to its own read of the tab's task, and says so when there is no
/// task.
#[gpui::test]
fn the_task_strip_is_the_tabs_own_task(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(1), window, cx)));
    vcx.run_until_parked();
    let read = request_of(&sent(&mut rx), |r| {
        matches!(
            r,
            ControlRequest::TaskRead {
                terminal: Some(1),
                token: None,
                ..
            }
        )
    })
    .expect("the tab's task asked for by its number");
    let task: qoop_runtime::control::TaskInfo = serde_json::from_value(serde_json::json!({
        "id": 12, "terminal": 1, "goal": "Check the bun test suite", "stage": "implementing",
        "opened_at": 0, "updated_at": 0,
    }))
    .expect("task");
    let tasks = |id: &str, tasks| ControlResponse::Tasks {
        id: id.into(),
        tasks,
        events: Vec::new(),
        more: false,
        space: None,
        items: None,
        item: None,
        loose: None,
    };
    answer(&desk, &mut vcx, tasks("someone-else", vec![task.clone()]));
    assert!(
        desk.read_with(&vcx, |d, _| d.strip.is_none()),
        "another read's answer"
    );
    answer(&desk, &mut vcx, tasks(read.request_id(), vec![task]));
    let heading = desk.read_with(&vcx, |d, _| d.strip.as_ref().and_then(|s| s.heading()));
    assert_eq!(heading.map(|h| h.0).as_deref(), Some("Task #12"));
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(2), window, cx)));
    vcx.run_until_parked();
    let read = request_of(&sent(&mut rx), |r| {
        matches!(r, ControlRequest::TaskRead { .. })
    })
    .expect("asked again for the new tab");
    answer(&desk, &mut vcx, tasks(read.request_id(), Vec::new()));
    let none = desk.read_with(&vcx, |d, _| d.strip.as_ref().map(|s| s.heading().is_none()));
    assert_eq!(none, Some(true), "No task for this terminal");
}

/// Cmd+V pastes the clipboard's text into the tab on screen as a paste.
#[gpui::test]
fn cmd_v_pastes_into_the_tab(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(1), window, cx)));
    vcx.run_until_parked();
    sent(&mut rx);
    vcx.write_to_clipboard(gpui::ClipboardItem::new_string("cargo test\n".into()));
    vcx.simulate_keystrokes("cmd-v");
    let pasted = sent(&mut rx)
        .into_iter()
        .any(|u| matches!(u, Up::Paste(1, bytes) if bytes == b"cargo test\n"));
    assert!(pasted, "pasted, not typed");
}

/// The macOS word and line keys reach the agent as Ghostty and iTerm2 send them, each whole in
/// one message: an Esc split from its letter would reach the agent as an Escape.
#[gpui::test]
fn the_mac_word_and_line_keys_reach_the_tab(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(1), window, cx)));
    vcx.run_until_parked();
    sent(&mut rx);
    for (keys, bytes) in [
        ("alt-left", &b"\x1bb"[..]),
        ("alt-right", b"\x1bf"),
        ("alt-backspace", b"\x1b\x7f"),
        ("cmd-left", b"\x01"),
        ("cmd-right", b"\x05"),
        ("cmd-backspace", b"\x15"),
    ] {
        vcx.simulate_keystrokes(keys);
        let typed: Vec<Vec<u8>> = sent(&mut rx)
            .into_iter()
            .filter_map(|u| match u {
                Up::Input(1, bytes) => Some(bytes),
                _ => None,
            })
            .collect();
        assert_eq!(typed, [bytes.to_vec()], "{keys}");
    }
}

/// d-review by keyboard only: from the terminal, `ctrl-a D` reads the tab's checkout in this
/// process, `n` opens the next file, `x` marks the finish read for its stop, Escape closes it
/// and gives the terminal the keys back; a remote tab has no review.
#[gpui::test]
fn the_review_reads_here_and_takes_n_and_x(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            let row = d
                .model
                .rows
                .iter_mut()
                .find(|r| r.terminal == 3)
                .expect("row");
            row.cwd = Some("/srv/acme-web".into());
            row.stop = Some("1700000000.4".into());
            d.open(Page::Session(3), window, cx);
        })
    });
    vcx.run_until_parked();
    // The terminal has the keys; the prefix opens Changes.
    vcx.simulate_keystrokes("ctrl-a shift-d");
    vcx.run_until_parked();
    let id = sent(&mut rx)
        .into_iter()
        .find_map(|u| match u {
            Up::Changes {
                id,
                terminal: 3,
                cwd,
            } if cwd == "/srv/acme-web" => Some(id),
            _ => None,
        })
        .expect("read here, for this tab's folder");
    let file = |path: &str, added, removed| qoop_runtime::control::ChangedFile {
        path: path.into(),
        status: "M".into(),
        added,
        removed,
    };
    answer(
        &desk,
        &mut vcx,
        ControlResponse::Changes {
            id,
            root: "/srv/acme-web".into(),
            branch: Some("acme-web/coupon-tests".into()),
            base: "main".into(),
            files: vec![
                file("src/a.ts", Some(12), Some(0)),
                file("src/b.ts", Some(1), Some(1)),
            ],
            diff: String::new(),
            cut: false,
            pr: None,
            pr_known: false,
        },
    );
    let totals = desk.read_with(&vcx, |d, _| {
        d.review.as_ref().and_then(|r| match &r.read {
            Some(Ok(c)) => Some(c.totals()),
            _ => None,
        })
    });
    assert_eq!(totals, Some((13, 1)));
    vcx.simulate_keystrokes("n");
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.review.as_ref().map(|r| r.open)),
        Some(1)
    );
    vcx.simulate_keystrokes("x");
    let seen = request_of(&sent(&mut rx), |r| matches!(r, ControlRequest::Seen { .. }));
    assert_eq!(
        seen,
        Some(ControlRequest::Seen {
            id: seen
                .as_ref()
                .map(|s| s.request_id().to_string())
                .unwrap_or_default(),
            terminal: 3,
            stop: Some("1700000000.4".into()),
        })
    );
    vcx.simulate_keystrokes("escape");
    assert!(desk.read_with(&vcx, |d, _| d.review.is_none()));
    assert!(vcx.update(|window, cx| desk.read(cx).term_focus.is_focused(window)));
    // A remote tab: no review is read.
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.open(Page::Session(0x8100_0001), window, cx);
        })
    });
    vcx.run_until_parked();
    vcx.simulate_keystrokes("ctrl-a shift-d");
    assert!(desk.read_with(&vcx, |d, _| d.review.is_none()));
    assert!(
        !sent(&mut rx)
            .iter()
            .any(|u| matches!(u, Up::Changes { .. })),
        "nothing read for a remote tab"
    );
}

/// `ctrl-a a` takes the keys off the terminal: `tab` then moves between the controls and types
/// nothing into the agent.
#[gpui::test]
fn ctrl_a_a_takes_the_keys_off_the_terminal(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(1), window, cx)));
    vcx.run_until_parked();
    assert!(vcx.update(|window, cx| desk.read(cx).term_focus.is_focused(window)));
    sent(&mut rx);
    vcx.simulate_keystrokes("ctrl-a a tab tab");
    assert!(!vcx.update(|window, cx| desk.read(cx).term_focus.is_focused(window)));
    assert!(
        !sent(&mut rx).iter().any(|u| matches!(u, Up::Input(..))),
        "nothing typed into the agent"
    );
    // Again: the keys are the terminal's once more.
    vcx.simulate_keystrokes("ctrl-a a");
    assert!(vcx.update(|window, cx| desk.read(cx).term_focus.is_focused(window)));
    // Off again, then Escape with nothing open gives them back too.
    vcx.simulate_keystrokes("ctrl-a a tab escape");
    assert!(vcx.update(|window, cx| desk.read(cx).term_focus.is_focused(window)));
    assert!(
        !sent(&mut rx).iter().any(|u| matches!(u, Up::Input(..))),
        "the Escape never reached the agent"
    );
}

/// The focused control leaves the frame: the keys come back to the terminal on screen instead
/// of dying above the desk (WEB-09), and the prefix works at once.
#[gpui::test]
fn focus_on_a_control_that_goes_comes_back_to_the_terminal(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(1), window, cx)));
    vcx.run_until_parked();
    // A control that is not drawn has the focus: what a link that went leaves behind.
    vcx.update(|window, cx| {
        let gone = cx.focus_handle();
        window.focus(&gone, cx);
        desk.update(cx, |_, cx| cx.notify());
    });
    vcx.run_until_parked();
    assert!(
        vcx.update(|window, cx| desk.read(cx).term_focus.is_focused(window)),
        "the terminal has the keys again"
    );
    vcx.simulate_keystrokes("ctrl-a i");
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.page),
        Page::Inbox,
        "the prefix still works"
    );
}

/// On the Inbox, a control that goes (a card answered there) leaves the keys in the window,
/// never in the agent kept alive behind it.
#[gpui::test]
fn focus_lost_on_the_inbox_stays_off_the_terminal(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(1), window, cx)));
    vcx.run_until_parked();
    vcx.simulate_keystrokes("ctrl-a i");
    vcx.update(|window, cx| {
        let gone = cx.focus_handle();
        window.focus(&gone, cx);
        desk.update(cx, |_, cx| cx.notify());
    });
    vcx.run_until_parked();
    assert!(
        vcx.update(|window, cx| desk.read(cx).focus.is_focused(window)),
        "the keys stay in the window"
    );
    assert!(!vcx.update(|window, cx| desk.read(cx).term_focus.is_focused(window)));
}

/// `ctrl-a i` puts the keys in the Inbox: Tab moves inside the window and types nothing into
/// the agent; Escape goes back and the terminal has them again.
#[gpui::test]
fn ctrl_a_i_puts_the_keys_in_the_inbox(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(1), window, cx)));
    vcx.run_until_parked();
    sent(&mut rx);
    vcx.simulate_keystrokes("ctrl-a i tab tab");
    assert!(!vcx.update(|window, cx| desk.read(cx).term_focus.is_focused(window)));
    assert!(
        !sent(&mut rx).iter().any(|u| matches!(u, Up::Input(..))),
        "no Tab typed into the agent"
    );
    vcx.simulate_keystrokes("escape");
    assert_eq!(desk.read_with(&vcx, |d, _| d.page), Page::Session(1));
    assert!(vcx.update(|window, cx| desk.read(cx).term_focus.is_focused(window)));
}

/// A pick on the session's card gives the keys back to its terminal, wherever the focus was,
/// so the next question's digit works with no Tab walk.
#[gpui::test]
fn a_pick_on_the_card_gives_the_keys_to_the_terminal(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    let mut rows = crate::state::tests::fixture().rows;
    rows[1].menu = Some(crate::state::tests::edit_menu(77));
    answer(
        &desk,
        &mut vcx,
        ControlResponse::Agents {
            id: "agents".into(),
            agents: rows,
            splits: Vec::new(),
            qass_checks: None,
        },
    );
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(2), window, cx)));
    vcx.run_until_parked();
    vcx.simulate_keystrokes("ctrl-a a");
    assert!(!vcx.update(|window, cx| desk.read(cx).term_focus.is_focused(window)));
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            let (terminal, offers, ..) = d.card_offers().expect("a card");
            d.pick(terminal, &offers[0], window, cx);
        })
    });
    vcx.run_until_parked();
    assert!(desk.read_with(&vcx, |d, _| !d.holds.held.is_empty()));
    assert!(vcx.update(|window, cx| desk.read(cx).term_focus.is_focused(window)));
}

/// A remote agent's question handed from here goes for This Mac's qass, named by this Mac's
/// hand id; a far qoop that answers it as one for its own qass is too old for it, says so,
/// and is not offered "Hand to qass" again.
#[gpui::test]
fn a_remote_hand_goes_for_this_macs_qass(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    let remote = 0x8100_0001;
    let mut rows = crate::state::tests::fixture().rows;
    let r = rows.iter_mut().find(|r| r.terminal == remote).unwrap();
    r.state = "blocked".into();
    r.status = Some("blocked".into());
    r.menu = Some(crate::state::tests::edit_menu(77));
    answer(
        &desk,
        &mut vcx,
        ControlResponse::Agents {
            id: "agents".into(),
            agents: rows,
            splits: Vec::new(),
            qass_checks: None,
        },
    );
    sent(&mut rx);
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            let row = d.model.row(remote).cloned().unwrap();
            assert_eq!(d.qass_reason(&row), None, "offered for a remote agent");
            let qass = crate::state::Offer {
                digit: None,
                label: "Hand to qass".into(),
                detail: None,
                pick: crate::state::Pick::Qass,
            };
            d.pick(remote, &qass, window, cx);
            for hold in &mut d.holds.held {
                hold.until = std::time::Instant::now();
            }
            d.fire();
        })
    });
    let asked = request_of(&sent(&mut rx), |r| {
        matches!(r, ControlRequest::HandToQass { .. })
    })
    .expect("handed");
    let ControlRequest::HandToQass { id, hands } = asked else {
        unreachable!()
    };
    assert_eq!(hands.len(), 1);
    assert_eq!(hands[0].away.as_deref(), Some("testmac"));
    answer(
        &desk,
        &mut vcx,
        ControlResponse::Failed {
            id,
            reason: "qass_not_running".into(),
        },
    );
    let (said, why) = desk.read_with(&vcx, |d, _| {
        let row = d.model.row(remote).cloned().unwrap();
        (
            d.said.get(&crate::state::Slot::Agent(remote)).cloned(),
            d.qass_reason(&row),
        )
    });
    assert!(
        matches!(&said, Some(crate::question::Said::Refused(w)) if w.contains("too old")),
        "{said:?}"
    );
    assert_eq!(
        why.as_deref(),
        Some("qoop on staging is too old to hand questions to qass")
    );
}

/// This Mac's daemon down: the banner says how to start it and offers Start qoop, and another
/// machine waits on This Mac with no Retry now, which could reach nothing.
#[gpui::test]
fn the_local_daemon_down_offers_start_and_no_remote_retry(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.apply(Down::Disconnected("gone".into()), window, cx);
            cx.notify();
        })
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("start-qoop").is_some(), "Start qoop");
    assert!(vcx.debug_bounds("retry-1").is_none(), "no remote retry");
    let banner = desk.read_with(&vcx, |d, _| d.banner().unwrap_or_default());
    assert!(banner.contains("run qoop to start it again"), "{banner}");
    assert!(
        banner.contains("staging not reached while This Mac is down"),
        "{banner}"
    );
    // Its sidebar line wraps inside the sidebar rather than run out of it.
    let word = vcx.debug_bounds("reach-word-1").expect("staging's line");
    let side = desk.read_with(&vcx, |d, _| d.side_px());
    assert!(word.right() <= gpui::px(side), "{word:?}");
}

/// Every control at the column's top takes the ring: from the window's own focus, Tab once
/// reaches the fold toggle, twice qass (its agent page), three times New agent, four times
/// the Inbox, then its fold and its first rows, then the Board and Schedules, each focused
/// with a key as the last input (what
/// `focus_visible` draws the ring on), and Space presses the one that has it.
#[gpui::test]
fn every_sidebar_link_takes_tab_and_its_ring(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    let press = |vcx: &mut VisualTestContext, tabs: usize, what: &str| {
        vcx.simulate_keystrokes(&vec!["tab"; tabs].join(" "));
        let (keyboard, on_control) = vcx.update(|window, cx| {
            let root = desk.read(cx).focus.clone();
            (
                window.last_input_was_keyboard(),
                window.focused(cx).is_some() && !root.is_focused(window),
            )
        });
        assert!(keyboard, "{what}: a key was the last input");
        assert!(on_control, "{what}: a control has the focus");
        vcx.simulate_keystrokes("space");
        vcx.update(|window, cx| {
            window.dispatch_event(
                gpui::PlatformInput::KeyUp(gpui::KeyUpEvent {
                    keystroke: gpui::Keystroke::parse("space").expect("space"),
                }),
                cx,
            );
        });
        vcx.run_until_parked();
    };
    for (tabs, link, from) in [
        (2, Page::Session(9), Page::Inbox),
        (4, Page::Inbox, Page::Settings),
        // The Inbox's fold (5) and its one waiting row (6) come before the Board.
        (7, Page::Board, Page::Inbox),
        (8, Page::Schedules, Page::Inbox),
    ] {
        vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(from, window, cx)));
        vcx.run_until_parked();
        press(&mut vcx, tabs, &format!("{link:?}"));
        assert_eq!(
            desk.read_with(&vcx, |d, _| d.page),
            link,
            "Tab {tabs} times is the {link:?} link"
        );
    }
    // The fold toggle: Space folds the column, and the keys stay on the toggle, now in the
    // band, where Space brings the column back.
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Inbox, window, cx)));
    vcx.run_until_parked();
    press(&mut vcx, 1, "the fold toggle");
    assert!(desk.read_with(&vcx, |d, _| !d.side_open), "folded");
    let on_toggle = vcx.update(|window, cx| desk.read(cx).side_toggle.is_focused(window));
    assert!(on_toggle, "the keys are on the toggle");
    vcx.simulate_keystrokes("space");
    vcx.update(|window, cx| {
        window.dispatch_event(
            gpui::PlatformInput::KeyUp(gpui::KeyUpEvent {
                keystroke: gpui::Keystroke::parse("space").expect("space"),
            }),
            cx,
        );
    });
    vcx.run_until_parked();
    assert!(desk.read_with(&vcx, |d, _| d.side_open), "open again");
    // New agent: Space opens its sheet.
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Inbox, window, cx)));
    vcx.run_until_parked();
    press(&mut vcx, 3, "New agent");
    assert!(
        desk.read_with(&vcx, |d, _| d.sheet.is_some()),
        "New agent opens"
    );
    vcx.simulate_keystrokes("escape");
    // The Inbox's fold: Space folds its first rows.
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Inbox, window, cx)));
    vcx.run_until_parked();
    press(&mut vcx, 5, "the Inbox's fold");
    assert!(desk.read_with(&vcx, |d, _| !d.inbox_open), "folded");
    // Further down, the order switch, the filter and each heading's controls are Tab stops:
    // Tab reaches acme-web's `...`, and Space opens its menu.
    let more = desk
        .read_with(&vcx, |d, _| d.more_focus.borrow().get(&1).cloned())
        .expect("acme-web's ...");
    let mut reached = false;
    for _ in 0..40 {
        vcx.simulate_keystrokes("tab");
        if vcx.update(|window, _| more.is_focused(window)) {
            reached = true;
            break;
        }
    }
    assert!(reached, "Tab reaches a space's ...");
    press(&mut vcx, 0, "acme-web's ...");
    assert!(
        desk.read_with(&vcx, |d, _| d.space_menu.is_some()),
        "Space opens the menu"
    );
    vcx.simulate_keystrokes("escape");
    // In order after Schedules (8): Spaces (9), Priority (10), the strip's + (11), the seven
    // state tabs (12 to 18), then acme-web's chevron (19), name (20) and + (21). Each by Tab
    // from the window, each pressed with Space.
    let reset = |vcx: &mut VisualTestContext| {
        vcx.update(|window, cx| {
            desk.update(cx, |d, cx| {
                d.sheet = None;
                d.space_edit = None;
                d.space_menu = None;
                d.side_view = crate::state::SideView::Spaces;
                d.side_filter = crate::state::StateTab::All;
                d.folds.clear();
                d.inbox_open = true;
                d.open(Page::Inbox, window, cx);
            })
        });
        vcx.run_until_parked();
    };
    reset(&mut vcx);
    press(&mut vcx, 10, "Priority");
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.side_view),
        crate::state::SideView::Priority
    );
    reset(&mut vcx);
    press(&mut vcx, 11, "the strip's +");
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.space_edit.as_ref().map(|e| e.target)),
        Some(crate::column::EditFor::New)
    );
    reset(&mut vcx);
    press(&mut vcx, 17, "the working tab");
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.side_filter),
        crate::state::StateTab::Working
    );
    reset(&mut vcx);
    press(&mut vcx, 19, "acme-web's chevron");
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.folds.get(&1).copied()),
        Some(false)
    );
    reset(&mut vcx);
    press(&mut vcx, 20, "acme-web's name");
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.space_edit.as_ref().map(|e| e.target)),
        Some(crate::column::EditFor::Rename(1))
    );
    reset(&mut vcx);
    press(&mut vcx, 21, "acme-web's +");
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.sheet.as_ref().map(|s| s.space)),
        Some(Some(1))
    );
}

/// The question card under an agent that needs the owner: answers with digits 1 to 5, none on
/// the "always" option; a digit before the menu is armed does nothing; `1` holds "Yes", `u`
/// undoes it; held again it goes as `AnswerMenu` with key 1, and the card says Sent once the
/// daemon typed it.
#[gpui::test]
fn the_question_card_holds_a_digit_and_sends_it(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    let mut rows = crate::state::tests::fixture().rows;
    rows[1].menu = Some(crate::state::tests::edit_menu(77));
    answer(
        &desk,
        &mut vcx,
        ControlResponse::Agents {
            id: "agents".into(),
            agents: rows,
            splits: Vec::new(),
            qass_checks: None,
        },
    );
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(2), window, cx)));
    vcx.run_until_parked();
    for digit in 1..=5 {
        let at = format!("card-2-{digit}");
        let at: &'static str = Box::leak(at.into_boxed_str());
        assert!(vcx.debug_bounds(at).is_some(), "{at} is drawn");
    }
    assert!(
        vcx.debug_bounds("card-2-0").is_some(),
        "the always option, no digit"
    );
    assert!(
        vcx.debug_bounds("task-strip").is_none(),
        "the task strip is a drawer, closed until asked for"
    );
    let at = vcx.debug_bounds("task-toggle").expect("Task").center();
    vcx.simulate_click(at, gpui::Modifiers::none());
    assert!(
        vcx.debug_bounds("task-strip").is_some() && vcx.debug_bounds("card-2-1").is_some(),
        "opened beside the terminal, the question card stays under it"
    );
    assert!(vcx.debug_bounds("card-2-6").is_none());
    sent(&mut rx);
    vcx.simulate_keystrokes("1");
    assert!(
        desk.read_with(&vcx, |d, _| d.holds.held.is_empty()),
        "too soon after the menu came"
    );
    assert!(
        !sent(&mut rx).iter().any(|u| matches!(u, Up::Input(..))),
        "and not typed into the agent"
    );
    std::thread::sleep(qoop_tui::answers::ARMING);
    vcx.simulate_keystrokes("1");
    let held = desk.read_with(&vcx, |d, _| {
        d.holds
            .of(&crate::state::Slot::Agent(2))
            .map(|h| h.label.clone())
    });
    assert_eq!(held.as_deref(), Some("Yes"));
    vcx.simulate_keystrokes("u");
    assert!(
        desk.read_with(&vcx, |d, _| d.holds.held.is_empty()),
        "undone"
    );
    vcx.simulate_keystrokes("1");
    vcx.update(|_, cx| {
        desk.update(cx, |d, _| {
            for hold in &mut d.holds.held {
                hold.until = std::time::Instant::now();
            }
            d.fire();
        })
    });
    let asked = request_of(&sent(&mut rx), |r| {
        matches!(r, ControlRequest::AnswerMenu { terminal: 2, .. })
    })
    .expect("sent after its hold");
    let ControlRequest::AnswerMenu { id, key, seal, .. } = asked else {
        unreachable!()
    };
    assert_eq!((key.as_str(), seal), ("1", 77));
    answer(
        &desk,
        &mut vcx,
        ControlResponse::MenuAnswered { id, answers: true },
    );
    let said = desk.read_with(&vcx, |d, _| {
        d.said.get(&crate::state::Slot::Agent(2)).cloned()
    });
    assert!(
        matches!(said, Some(crate::question::Said::Sent { ref label, .. }) if label == "Yes"),
        "{said:?}"
    );
    // The agent asks again: the card is for the new question, without the old one's Sent line.
    let mut rows = crate::state::tests::fixture().rows;
    rows[1].menu = Some(crate::state::tests::edit_menu(78));
    answer(
        &desk,
        &mut vcx,
        ControlResponse::Agents {
            id: "agents".into(),
            agents: rows,
            splits: Vec::new(),
            qass_checks: None,
        },
    );
    assert!(
        desk.read_with(&vcx, |d, _| !d
            .said
            .contains_key(&crate::state::Slot::Agent(2))),
        "the new question starts clean"
    );
    // Answered again, and the agent asks its next question before the answer comes back: the
    // late "Sent" belongs to the old question and never lands under the new one.
    std::thread::sleep(qoop_tui::answers::ARMING);
    vcx.simulate_keystrokes("1");
    vcx.update(|_, cx| {
        desk.update(cx, |d, _| {
            for hold in &mut d.holds.held {
                hold.until = std::time::Instant::now();
            }
            d.fire();
        })
    });
    let asked = request_of(&sent(&mut rx), |r| {
        matches!(r, ControlRequest::AnswerMenu { terminal: 2, .. })
    })
    .expect("the second answer sent");
    let ControlRequest::AnswerMenu { id, seal, .. } = asked else {
        unreachable!()
    };
    assert_eq!(seal, 78);
    let mut rows = crate::state::tests::fixture().rows;
    rows[1].menu = Some(crate::state::tests::edit_menu(79));
    answer(
        &desk,
        &mut vcx,
        ControlResponse::Agents {
            id: "agents".into(),
            agents: rows,
            splits: Vec::new(),
            qass_checks: None,
        },
    );
    answer(
        &desk,
        &mut vcx,
        ControlResponse::MenuAnswered { id, answers: true },
    );
    assert!(
        desk.read_with(&vcx, |d, _| !d
            .said
            .contains_key(&crate::state::Slot::Agent(2))),
        "no Sent line under the next question"
    );
}

/// A closed answer says what it waits for: this Mac's daemon for a local agent, the link for a
/// remote one, by the machine's name.
#[gpui::test]
fn a_stale_answer_says_what_it_waits_for(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    let (local, remote) = desk.read_with(&vcx, |d, _| {
        let reason = |terminal: u32| d.stale_reason(d.model.row(terminal).expect("the row"));
        (reason(2), reason(0x8100_0001))
    });
    assert_eq!(local, "Answering opens again when the daemon is back");
    assert_eq!(remote, "Answering opens again when staging is reached");
    // This Mac's daemon lost: every machine waits for it, staging's card included.
    let remote = vcx.update(|_, cx| {
        desk.update(cx, |d, _| {
            d.lost_at = Some(d.now);
            d.stale_reason(d.model.row(0x8100_0001).expect("the row"))
        })
    });
    assert_eq!(remote, "Answering opens again when the daemon is back");
}

/// The Inbox counts what needs the owner once, the same in its header and its badge: the agents
/// that ask, and a qass card about none of them; a card about an asking agent is not a second.
#[gpui::test]
fn the_inbox_and_its_badge_count_qass_cards_once(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    let card = |id: &str, terminal: u32| -> qoop_runtime::control::AskInfo {
        serde_json::from_value(serde_json::json!({
            "card": id, "terminal": terminal, "status": "open", "opened_at": 1000,
            "body": {
                "handle": "soak", "urgency": "later", "doing": "running the soak test",
                "why": "the migration touches billing", "asks": "Run the migration?",
                "options": [
                    {"key": "a", "label": "Run it", "reason": "it passed", "words": "run it"},
                    {"key": "b", "label": "Wait", "reason": "after the release", "words": "wait"},
                ],
            },
        }))
        .expect("card")
    };
    let count = |vcx: &VisualTestContext| desk.read_with(vcx, |d, _| d.inbox().count);
    assert_eq!(count(&vcx), 1, "one agent asks");
    vcx.update(|_, cx| desk.update(cx, |d, _| d.asks = vec![card("E1", 2)]));
    assert_eq!(
        count(&vcx),
        1,
        "a card about the asking agent is not a second"
    );
    vcx.update(|_, cx| desk.update(cx, |d, _| d.asks.push(card("E2", 1))));
    assert_eq!(count(&vcx), 2, "a card about a working agent counts");
    // The header and the sidebar's badge read the same list.
    vcx.update(|_, cx| desk.update(cx, |_, cx| cx.notify()));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("inbox-card-E2").is_some());
}

/// A question from a machine not reached is the last one seen: neutral, with when it was seen,
/// and its own answer field cannot be opened.
#[gpui::test]
fn a_stale_question_card_is_neutral_and_closed(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    let mut rows = crate::state::tests::fixture().rows;
    rows[1].menu = Some(crate::state::tests::edit_menu(5));
    answer(
        &desk,
        &mut vcx,
        ControlResponse::Agents {
            id: "agents".into(),
            agents: rows,
            splits: Vec::new(),
            qass_checks: None,
        },
    );
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(2), window, cx)));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("question-card").is_some());
    assert!(vcx.debug_bounds("own-open-off").is_none(), "answerable");
    // As `wire` does after each batch: the news applied, then the window drawn again.
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.apply(Down::Disconnected("gone".into()), window, cx);
            cx.notify();
        })
    });
    vcx.run_until_parked();
    assert!(
        desk.read_with(&vcx, |d, _| d
            .model
            .row(2)
            .and_then(|r| d.as_of(r.workspace)))
            .is_some(),
        "stale"
    );
    assert!(vcx.debug_bounds("question-card-when").is_some());
    assert!(
        vcx.debug_bounds("own-open-off").is_some(),
        "the answer field is closed"
    );
    // The pane header and the tab chip above the card go neutral with it.
    let (kind, word) = desk.read_with(&vcx, |d, _| {
        let row = d.model.row(2).expect("the row");
        (
            d.shown_kind(crate::state::Entry::Live(row)),
            d.shown_word(row),
        )
    });
    assert_eq!(
        kind,
        crate::state::Kind::Status(qoop_domain::status::Status::Unknown)
    );
    assert!(word.starts_with("Needs you as of "), "{word}");
}

/// Explain goes as typed words through the agent's "tell it" option, and an older daemon's
/// refusal hides typed answers for that machine.
#[gpui::test]
fn explain_is_typed_words_and_an_old_daemon_hides_them(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    let mut rows = crate::state::tests::fixture().rows;
    rows[1].menu = Some(crate::state::tests::edit_menu(5));
    answer(
        &desk,
        &mut vcx,
        ControlResponse::Agents {
            id: "agents".into(),
            agents: rows,
            splits: Vec::new(),
            qass_checks: None,
        },
    );
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.open(Page::Session(2), window, cx);
            let explain =
                crate::state::offers(d.model.row(2).and_then(|r| r.menu.as_ref()).expect("menu"))
                    .into_iter()
                    .find(|o| o.digit == Some(4))
                    .expect("Explain is 4");
            d.pick(2, &explain, window, cx);
            for hold in &mut d.holds.held {
                hold.until = std::time::Instant::now();
            }
            d.fire();
        })
    });
    let asked = request_of(&sent(&mut rx), |r| {
        matches!(r, ControlRequest::AnswerMenuText { .. })
    })
    .expect("typed words");
    let ControlRequest::AnswerMenuText { id, key, text, .. } = asked else {
        unreachable!()
    };
    assert_eq!(key, "4");
    assert_eq!(text, qoop_tui::answers::EXPLAIN);
    answer(
        &desk,
        &mut vcx,
        ControlResponse::Failed {
            id,
            reason: "unknown variant `AnswerMenuText`".into(),
        },
    );
    let (said, offered) = desk.read_with(&vcx, |d, _| {
        (
            d.said.get(&crate::state::Slot::Agent(2)).cloned(),
            d.too_old.contains(&0),
        )
    });
    assert!(offered, "this machine takes no typed answers now");
    assert_eq!(
        said,
        Some(crate::question::Said::Refused(
            "qoop on This Mac is too old for typed answers.".into()
        ))
    );
}

/// A qass card's option goes through the same hold as an agent's answer: held, `u` undoes it
/// from anywhere it shows (never typed into the terminal), held again it is written as the
/// owner's answer after the hold, and the card says Sent.
#[gpui::test]
fn a_card_answer_waits_out_the_hold_too(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    let card: qoop_runtime::control::AskInfo = serde_json::from_value(serde_json::json!({
        "card": "E41", "agent": "staging/T3", "status": "open", "opened_at": 1000,
        "body": {
            "handle": "soak", "urgency": "later", "doing": "running the soak test",
            "why": "the migration touches billing", "asks": "Run the migration?",
            "options": [
                {"key": "a", "label": "Run it", "reason": "the test passed", "words": "run it"},
                {"key": "b", "label": "Wait", "reason": "after the release", "words": "wait"},
            ],
        },
    }))
    .expect("card");
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.asks = vec![card.clone()];
            d.open(Page::Inbox, window, cx);
            d.pick_card(&card, "a", window, cx);
        })
    });
    vcx.run_until_parked();
    sent(&mut rx);
    let slot = crate::state::Slot::Card("E41".into());
    assert!(
        desk.read_with(&vcx, |d, _| d.holds.of(&slot).is_some()),
        "held"
    );
    vcx.simulate_keystrokes("u");
    assert!(
        desk.read_with(&vcx, |d, _| d.holds.held.is_empty()),
        "undone"
    );
    assert!(
        !sent(&mut rx).iter().any(|u| matches!(u, Up::Input(..))),
        "u went to Undo, not to the agent on screen"
    );
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.pick_card(&card, "a", window, cx);
            for hold in &mut d.holds.held {
                hold.until = std::time::Instant::now();
            }
            d.fire();
        })
    });
    let asked = request_of(&sent(&mut rx), |r| {
        matches!(r, ControlRequest::AskWrite { .. })
    })
    .expect("written after the hold");
    let ControlRequest::AskWrite { id, action, .. } = asked else {
        unreachable!()
    };
    assert_eq!(
        action,
        qoop_runtime::control::AskOp::Answer {
            card: "E41".into(),
            option: Some("a".into()),
            text: None,
            seen: None,
            to_qass: false,
        }
    );
    answer(
        &desk,
        &mut vcx,
        ControlResponse::AskWritten {
            id,
            card: "E41".into(),
            result: qoop_runtime::control::AskResult::Passed,
        },
    );
    let said = desk.read_with(&vcx, |d, _| d.said.get(&slot).cloned());
    assert!(
        matches!(said, Some(crate::question::Said::Sent { ref label, .. }) if label == "Run it"),
        "{said:?}"
    );
}

/// The schedules page: opening it reads each schedule's runs; a schedule's switch pauses it;
/// Escape closes the calendar.
#[gpui::test]
fn the_schedules_page_reads_runs_opens_a_report_and_pauses(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    let schedule: qoop_runtime::control::ScheduleInfo = serde_json::from_value(serde_json::json!({
        "id": 1, "name": "nightly-tests", "cron": "0 2 * * 1-5", "agent": "claude",
        "cwd": "/srv/acme-web", "prompt": "Run bun test", "paused": false, "created_at": 0,
        "when": "weekdays 02:00", "next_at": 2000 + 86_400,
    }))
    .expect("schedule");
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.model.schedules = vec![schedule];
            d.open(Page::Schedules, window, cx);
        })
    });
    vcx.run_until_parked();
    let asked = request_of(&sent(&mut rx), |r| {
        matches!(r, ControlRequest::ScheduleRuns { schedule: 1, .. })
    })
    .expect("its runs asked for");
    let run: qoop_runtime::control::ScheduleRun = serde_json::from_value(serde_json::json!({
        "id": 7, "schedule": 1, "slot": 1000, "started_at": 1000, "terminal": 11,
        "result": "started", "ended_at": 1000 + 41 * 60, "outcome": "done",
        "summary": "13 of 13 pass; nothing to fix",
    }))
    .expect("run");
    answer(
        &desk,
        &mut vcx,
        ControlResponse::ScheduleRuns {
            id: asked.request_id().to_string(),
            runs: vec![run],
        },
    );
    let day = desk.read_with(&vcx, |d, _| crate::state::day_of(1000, d.zone));
    vcx.update(|_, cx| {
        desk.update(cx, |d, cx| {
            d.schedule_page.day = Some(day);
            cx.notify();
        })
    });
    vcx.run_until_parked();
    let at = vcx
        .debug_bounds("schedule-on-1")
        .expect("its switch")
        .center();
    vcx.simulate_click(at, gpui::Modifiers::none());
    let paused = request_of(&sent(&mut rx), |r| {
        matches!(r, ControlRequest::ScheduleWrite { .. })
    });
    let Some(ControlRequest::ScheduleWrite { action, .. }) = paused.clone() else {
        panic!("no write: {paused:?}");
    };
    assert_eq!(
        *action,
        qoop_runtime::control::ScheduleOp::Pause { schedule: 1 }
    );
    vcx.update(|_, cx| {
        desk.update(cx, |d, cx| {
            d.schedule_page.calendar = true;
            cx.notify();
        })
    });
    vcx.simulate_keystrokes("escape");
    assert!(
        desk.read_with(&vcx, |d, _| !d.schedule_page.calendar),
        "Escape closes it"
    );
}

/// The settings page on a config file of the test's own: Sound writes `sound = true` keeping
/// the comments and a key qoop does not know; `/` finds a setting; a file that is not TOML
/// leaves no switch to press and says why.
#[gpui::test]
fn settings_write_in_place_find_and_refuse_a_broken_file(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "# my settings\n[notify]\n# beep or not\ndesktop = true\nmine = 1\n",
    )
    .expect("config");
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.config_path = path.clone();
            d.pet_path = dir.path().join("avatar.json");
            d.open(Page::Settings, window, cx);
        })
    });
    vcx.run_until_parked();
    let at = vcx
        .debug_bounds("setting-notify.sound")
        .expect("the Sound switch")
        .center();
    vcx.simulate_click(at, gpui::Modifiers::none());
    let text = std::fs::read_to_string(&path).expect("config");
    assert!(text.contains("sound = true"), "{text}");
    assert!(
        text.contains("# my settings") && text.contains("# beep or not"),
        "{text}"
    );
    assert!(
        text.contains("mine = 1"),
        "a key qoop does not know stays: {text}"
    );
    let said = desk.read_with(&vcx, |d, _| d.settings.said.clone().unwrap_or_default());
    assert!(said.starts_with("Sound on: written as line"), "{said}");
    // The file view marks the line away from its default with the word, not a tint alone.
    vcx.run_until_parked();
    // A selector is `&'static str`; a test's few leak nothing that matters.
    let marker = |key: &str| -> &'static str {
        let n = text
            .lines()
            .position(|l| l.starts_with(key))
            .unwrap_or_else(|| panic!("{key} in {text}"));
        Box::leak(format!("config-changed-{}", n + 1).into_boxed_str())
    };
    assert!(
        vcx.debug_bounds(marker("sound =")).is_some(),
        "sound = true is marked changed"
    );
    assert!(
        vcx.debug_bounds(marker("mine =")).is_none(),
        "a key qoop does not know is not a change"
    );

    // `/` as a Turkish Q keyboard types it: shift-7.
    vcx.simulate_keystrokes("shift-7->/");
    vcx.simulate_input("sound");
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("setting-notify.sound").is_some());
    assert!(
        vcx.debug_bounds("setting-notify.desktop").is_none(),
        "filtered out"
    );
    vcx.simulate_keystrokes("escape");

    std::fs::write(&path, "[notify\nsound = = true\n").expect("broken");
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Settings, window, cx)));
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("setting-notify.sound").is_none(),
        "no switch to press"
    );
    assert!(desk.read_with(&vcx, |d, _| d.settings.broken));
}

/// A run still going reads as its tab does now: Needs you while its agent asks. One whose tab
/// is not listed keeps its own word.
#[gpui::test]
fn an_open_run_reads_as_its_tab(cx: &mut TestAppContext) {
    let (desk, vcx, _rx) = open(cx);
    let run = |terminal: i64| -> qoop_runtime::control::ScheduleRun {
        serde_json::from_value(serde_json::json!({
            "id": 8, "schedule": 1, "slot": 1000, "started_at": 1000,
            "terminal": terminal, "result": "started",
        }))
        .expect("run")
    };
    let (kind, words) = desk.read_with(&vcx, |d, _| d.run_words(&run(2)));
    assert_eq!(
        kind,
        Some(crate::state::Kind::Status(
            qoop_domain::status::Status::Blocked
        ))
    );
    assert_eq!(words, "Needs you");
    let (_, words) = desk.read_with(&vcx, |d, _| d.run_words(&run(99)));
    assert_eq!(words, "Working");
}

/// A schedule that never ran opens from All schedules: what it is, when it runs, and the same
/// Edit, Pause and Run now as a report. A run's report lists only runs earlier than it, and a
/// run with no message that waits on the owner says so.
#[gpui::test]
fn a_schedule_that_never_ran_opens_and_earlier_runs_are_earlier(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.model.schedules = vec![crate::state::tests::schedule_info(
                1,
                "review-pr",
                "0 9 * * *",
                0,
            )];
            d.open(Page::Schedules, window, cx);
        })
    });
    vcx.run_until_parked();
    let at = vcx
        .debug_bounds("schedule-row-1")
        .expect("the row is a control");
    vcx.simulate_click(at.center(), gpui::Modifiers::none());
    vcx.run_until_parked();
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.schedule_page.open),
        Some((1, None))
    );
    assert!(
        vcx.debug_bounds("unrun-report").is_some(),
        "the schedule alone"
    );

    let run = |id: i64, started_at: i64| -> qoop_runtime::control::ScheduleRun {
        serde_json::from_value(serde_json::json!({
            "id": id, "schedule": 1, "slot": started_at, "started_at": started_at,
            "terminal": 2, "result": "started",
        }))
        .expect("run")
    };
    vcx.update(|_, cx| {
        desk.update(cx, |d, cx| {
            d.schedule_page
                .runs
                .insert((0, 1), vec![run(9, 1900), run(8, 1000), run(7, 900)]);
            d.schedule_page.open = Some((1, Some(8)));
            cx.notify();
        })
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("earlier-7").is_some(), "the earlier run");
    assert!(vcx.debug_bounds("earlier-9").is_none(), "not the newer one");
}

/// A one-off's Edit sheet says the one-off's own date, which its time field (a clock only)
/// does not hold, not the next time that clock comes round.
#[gpui::test]
fn a_one_offs_edit_sheet_says_its_own_date(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    let (said, want) = vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            let at = d.now + 2 * 86_400 + 3_600;
            let s: qoop_runtime::control::ScheduleInfo =
                serde_json::from_value(serde_json::json!({
                    "id": 1, "name": "lint-check", "at": at, "agent": "claude",
                    "cwd": "/srv/acme-web", "prompt": "go", "paused": false, "created_at": 0,
                    "when": "once", "next_at": at,
                }))
                .expect("schedule");
            d.model.schedules = vec![s.clone()];
            d.open(Page::Schedules, window, cx);
            d.open_sheet(Some(&s), window, cx);
            let sheet = d.schedule_page.sheet.as_ref().expect("the sheet");
            (
                d.sheet_first_line(sheet, cx),
                format!(
                    "Next run: {}",
                    qoop_tui::schedules::moment(at, d.now, d.zone)
                ),
            )
        })
    });
    assert_eq!(said, want);
}

/// A sheet taller than the window scrolls inside it: its top is never above the window.
#[gpui::test]
fn a_sheet_taller_than_the_window_keeps_its_top_in_it(cx: &mut TestAppContext) {
    let (_desk, mut vcx, _rx) = open(cx);
    vcx.simulate_resize(gpui::size(gpui::px(900.), gpui::px(300.)));
    vcx.run_until_parked();
    vcx.simulate_keystrokes("ctrl-a c");
    vcx.run_until_parked();
    let at = vcx.debug_bounds("new-work-sheet").expect("the sheet");
    assert!(at.origin.y >= gpui::px(0.), "{at:?}");
}

/// A schedule opened before it ran shows its run's report once one starts, never "Not run yet"
/// over a run that waits.
#[gpui::test]
fn a_schedule_opened_before_its_run_shows_the_run(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.model.schedules = vec![crate::state::tests::schedule_info(
                1,
                "review-pr",
                "0 9 * * *",
                0,
            )];
            d.open(Page::Schedules, window, cx);
            d.schedule_page.open = Some((1, None));
        })
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("unrun-report").is_some());
    vcx.update(|_, cx| {
        desk.update(cx, |d, cx| {
            let run: qoop_runtime::control::ScheduleRun =
                serde_json::from_value(serde_json::json!({
                    "id": 8, "schedule": 1, "slot": 1000, "started_at": 1000,
                    "terminal": 2, "result": "started",
                }))
                .expect("run");
            d.schedule_page.runs.insert((0, 1), vec![run]);
            cx.notify();
        })
    });
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("unrun-report").is_none(),
        "the run's report"
    );
}

/// A hand that ended leaves no "Sent: Handed to qass" over a question that waits again.
#[gpui::test]
fn an_ended_hand_leaves_no_sent_line(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    let shown = vcx.update(|_, cx| {
        desk.update(cx, |d, cx| {
            let slot = crate::state::Slot::Agent(2);
            d.said.insert(
                slot.clone(),
                crate::question::Said::Sent {
                    label: "Handed to qass".into(),
                    at: d.now - 60,
                },
            );
            let t = d.theme();
            d.said_line(&slot, &t, cx).is_some()
        })
    });
    assert!(!shown, "the row is not with qass any more");
}

/// A field filled for editing puts the caret after its text, so typing adds to the end.
#[gpui::test]
fn a_filled_field_types_at_its_end(cx: &mut TestAppContext) {
    let (_desk, mut vcx, _rx) = open(cx);
    let at = vcx.update(|_, cx| {
        let field = cx.new(|cx| {
            let mut f = crate::field::Field::new("Prompt", cx);
            f.set_text("Review the open PR");
            f
        });
        field.read(cx).selection()
    });
    assert_eq!(at, 18..18);
}

/// The theme follows `config.toml` while the window runs: read on opening Settings, turned at
/// once by a write from Settings, and again when the file changes under the window.
#[gpui::test]
fn the_theme_follows_config_toml_at_once(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "theme = \"dark\"\n").expect("config");
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.config_path = path.clone();
            d.open(Page::Settings, window, cx);
        })
    });
    vcx.run_until_parked();
    assert!(desk.read_with(&vcx, |d, _| d.dark), "dark as the file says");
    let settings = crate::state::SETTINGS;
    let theme = settings
        .iter()
        .find(|s| s.key == "theme")
        .expect("the theme setting");
    vcx.update(|_, cx| desk.update(cx, |d, cx| d.write_setting(theme, "light", cx)));
    assert!(!desk.read_with(&vcx, |d, _| d.dark), "light at once");
    // What the window last read is the file as written; a change under it is read again.
    vcx.update(|_, cx| desk.update(cx, |d, _| d.follow_config()));
    std::fs::write(&path, "# set elsewhere\ntheme = \"dark\"\n").expect("config");
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.open(Page::Inbox, window, cx);
            assert!(d.follow_config(), "the change is seen");
        })
    });
    assert!(desk.read_with(&vcx, |d, _| d.dark), "dark again");
    vcx.update(|_, cx| desk.update(cx, |d, _| assert!(!d.follow_config(), "no change, no read")));
}

/// A pet picked from a new deal waits out the hold with Undo; once it is up the pet is saved to
/// the test's own file. Keep closes the deal and changes nothing.
#[gpui::test]
fn a_new_pet_waits_its_hold_and_can_be_undone(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    let dir = tempfile::tempdir().expect("dir");
    let pet_path = dir.path().join("avatar.json");
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.pet_path = pet_path.clone();
            d.open(Page::Settings, window, cx);
            d.deal_pets(cx);
        })
    });
    let first = desk.read_with(&vcx, |d, _| d.settings.pets[0].clone());
    vcx.update(|_, cx| {
        desk.update(cx, |d, cx| {
            d.pick_pet(first.clone(), cx);
            d.undo_pet(cx);
        })
    });
    assert!(
        desk.read_with(&vcx, |d, _| d.settings.pet_hold.is_none()),
        "undone"
    );
    assert!(!pet_path.exists(), "nothing saved");
    vcx.update(|_, cx| {
        desk.update(cx, |d, cx| {
            d.pick_pet(first.clone(), cx);
            if let Some((_, until)) = d.settings.pet_hold.as_mut() {
                *until = std::time::Instant::now();
            }
            d.save_held_pet(false);
        })
    });
    assert!(pet_path.exists(), "saved once the hold was up");
    assert_eq!(desk.read_with(&vcx, |d, _| d.my_pet.clone()), Some(first));
}

/// Every key hint names the prefix `config.toml` picked.
#[gpui::test]
fn key_hints_name_the_configured_prefix(cx: &mut TestAppContext) {
    let (up, _rx) = tokio::sync::mpsc::unbounded_channel();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "prefix = \"ctrl-b\"\n").expect("config");
    let config = qoop_tui::config::read_from(&path);
    let window = cx.add_window(|window, cx| Desk::new(up, Latest::default(), &config, window, cx));
    let mut vcx = VisualTestContext::from_window(window.into(), cx);
    let desk = window.root(&mut vcx).expect("the window's view");
    assert_eq!(desk.read_with(&vcx, |d, _| d.prefix_name()), "ctrl-b");
    vcx.simulate_keystrokes("ctrl-b c");
    assert!(
        desk.read_with(&vcx, |d, _| d.sheet.is_some()),
        "ctrl-b c opens the New agent sheet"
    );
}

/// The sidebar lists a sleeping tab as a current daemon lists it, under its space, and a space
/// where everything sleeps: once its space is opened up.
#[gpui::test]
fn the_sidebar_lists_sleeping_tabs_from_the_space_list(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            let mut spaces = d.model.spaces.clone();
            spaces.push(crate::state::tests::sleepy_space(
                7,
                "docs",
                &[(70, 70, Some("codex"), "")],
            ));
            d.apply(
                Down::Answer(Box::new(ControlResponse::Workspaces {
                    id: "spaces".into(),
                    workspaces: spaces,
                })),
                window,
                cx,
            );
            // The spaces opened up: every agent, sleepers included.
            d.folds.extend([(7, true), (1i64 << 40 | 1, true)]);
            // As the link's pump does after each batch.
            cx.notify();
        })
    });
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("tree-row-2164260866").is_some(),
        "the remote's sleeper"
    );
    assert!(
        vcx.debug_bounds("tree-row-2164260867").is_none(),
        "its second pane"
    );
    assert!(
        vcx.debug_bounds("tree-row-70").is_some(),
        "the space that sleeps whole"
    );
}

/// A remote that stops answering: retrying at first, then after 15 s its sidebar line and the banner
/// say "not reached, as of", its rows stay (neutral, as of then) and cannot be answered; back,
/// it is live again. This Mac's daemon gone: every machine as of then.
#[gpui::test]
fn a_machine_not_answering_goes_stale_and_comes_back(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    let remote_row = 0x8100_0001u32;
    let set_link = |link| {
        let mut spaces = fixture().spaces;
        for w in &mut spaces {
            if crate::state::machine_of(w.id) == 1 {
                w.link = Some(link);
            }
        }
        ControlResponse::Workspaces {
            id: "mux".into(),
            workspaces: spaces,
        }
    };
    let local_rows = || ControlResponse::Agents {
        id: "mux".into(),
        agents: fixture()
            .rows
            .into_iter()
            .filter(|r| crate::state::machine_of(r.workspace) == 0)
            .collect(),
        splits: Vec::new(),
        qass_checks: None,
    };
    answer(&desk, &mut vcx, set_link(LinkState::Up));
    let pet = sent(&mut rx).into_iter().any(|u| {
        matches!(u, Up::Request(r) if matches!(&*r, ControlRequest::Via { request, .. }
            if matches!(**request, ControlRequest::Pet { .. })))
    });
    assert!(pet, "the remote's pet asked through Via");
    // The link drops: the mux lists the spaces down, then the agents without the remote's.
    answer(&desk, &mut vcx, set_link(LinkState::Down));
    answer(&desk, &mut vcx, local_rows());
    assert!(
        desk.read_with(&vcx, |d, _| d.model.row(remote_row).is_some()),
        "its rows stay"
    );
    assert_eq!(desk.read_with(&vcx, |d, _| d.banner()), None, "retrying");
    vcx.update(|_, cx| {
        desk.update(cx, |d, cx| {
            d.now += crate::state::STALE_AFTER;
            cx.notify();
        })
    });
    vcx.run_until_parked();
    let zone = desk.read_with(&vcx, |d, _| d.zone);
    let words = format!("not reached, as of {}", crate::state::clock(2000, zone));
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.banner()),
        Some(format!("staging {words}"))
    );
    assert_eq!(
        desk.read_with(&vcx, |d, _| crate::state::reach_words(
            d.machine_state(1),
            d.zone
        )),
        words,
        "the sidebar line's words"
    );
    assert!(vcx.debug_bounds("machine-1").is_some());
    let workspace = desk.read_with(&vcx, |d, _| d.model.row(remote_row).map(|r| r.workspace));
    assert_eq!(
        desk.read_with(&vcx, |d, _| workspace.and_then(|w| d.as_of(w))),
        Some(format!("as of {}", crate::state::clock(2000, zone)))
    );
    // Back.
    answer(&desk, &mut vcx, set_link(LinkState::Up));
    assert_eq!(desk.read_with(&vcx, |d, _| d.banner()), None, "live again");
    // This Mac's daemon gone: every machine stale.
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.apply(Down::Disconnected("gone".into()), window, cx)
        })
    });
    let banner = desk.read_with(&vcx, |d, _| d.banner()).unwrap_or_default();
    assert!(banner.starts_with("This Mac not reached"), "{banner}");
    assert!(banner.contains("staging not reached"), "{banner}");
}

/// A remote tab opened while its link is down: once the link is up again its pane is shown
/// again (the mux took no subscription while the link was down) and its task is read again.
#[gpui::test]
fn a_remote_tab_opened_while_down_comes_back_with_its_link(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    let remote_row = 0x8100_0001u32;
    let set_link = |link| {
        let mut spaces = fixture().spaces;
        for w in &mut spaces {
            if crate::state::machine_of(w.id) == 1 {
                w.link = Some(link);
            }
        }
        ControlResponse::Workspaces {
            id: "mux".into(),
            workspaces: spaces,
        }
    };
    answer(&desk, &mut vcx, set_link(LinkState::Down));
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(remote_row), window, cx)));
    vcx.run_until_parked();
    sent(&mut rx);
    answer(&desk, &mut vcx, set_link(LinkState::Up));
    let ups = sent(&mut rx);
    assert!(
        ups.iter()
            .any(|u| matches!(u, Up::Show(t) if t.contains(&remote_row))),
        "shown again"
    );
    assert!(
        request_of(&ups, |r| matches!(
            r,
            ControlRequest::TaskRead { terminal: Some(t), .. } if *t == remote_row
        ))
        .is_some(),
        "its task read again"
    );
    // A link that stays up asks for nothing more.
    answer(&desk, &mut vcx, set_link(LinkState::Up));
    assert!(!sent(&mut rx).iter().any(|u| matches!(u, Up::Show(_))));
}

/// Settings for another machine: its sleep keys and qass's switch are read and written there
/// through `Via`; its answer fills the rows, and a write says where it landed.
#[gpui::test]
fn remote_settings_go_through_via(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    let remote = 1i64 << 40 | 1;
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.open(Page::Settings, window, cx);
            d.settings.via = Some(remote);
            cx.notify();
        })
    });
    vcx.run_until_parked();
    sent(&mut rx);
    vcx.update(|_, cx| desk.update(cx, |d, cx| d.show_settings_machine(Some(remote), cx)));
    let inner: Vec<ControlRequest> = sent(&mut rx)
        .into_iter()
        .filter_map(|u| match u {
            Up::Request(r) => match *r {
                ControlRequest::Via {
                    workspace, request, ..
                } if workspace == remote => Some(*request),
                _ => None,
            },
            _ => None,
        })
        .collect();
    let sleep = inner
        .iter()
        .find(|r| matches!(r, ControlRequest::SleepSettings { .. }))
        .expect("sleep read through Via");
    assert!(
        inner
            .iter()
            .any(|r| matches!(r, ControlRequest::QassWatch { on: None, .. }))
    );
    answer(
        &desk,
        &mut vcx,
        ControlResponse::SleepSettings {
            id: sleep.request_id().to_string(),
            idle_minutes: 30,
            exit_when_asleep: true,
            problem: None,
        },
    );
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.settings.remote.idle_minutes),
        Some(30)
    );
    assert!(vcx.debug_bounds("remote-exit").is_some(), "drawn");
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.title().1).as_deref(),
        Some("staging"),
        "the title names the machine, not This Mac's file"
    );
    let card = vcx.debug_bounds("remote-settings").expect("the card");
    let last = vcx
        .debug_bounds("remote-minutes-240")
        .expect("the 240 chip");
    assert!(
        last.right() <= card.right(),
        "the minutes stay inside their card: {last:?} in {card:?}"
    );
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.settings.said.clone()),
        None,
        "a read says nothing"
    );
}

/// The requests the window sent through `Via` to `workspace`, unwrapped.
fn via_sent(rx: &mut UnboundedReceiver<Up>, workspace: i64) -> Vec<ControlRequest> {
    sent(rx)
        .into_iter()
        .filter_map(|u| match u {
            Up::Request(r) => match *r {
                ControlRequest::Via {
                    workspace: to,
                    request,
                    ..
                } if to == workspace => Some(*request),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// A remote's answer that comes after the page went back to This Mac is dropped: it never
/// lands on This Mac's qass switch or its settings.
#[gpui::test]
fn a_late_remote_settings_answer_is_dropped(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    let remote = 1i64 << 40 | 1;
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.open(Page::Settings, window, cx);
            d.show_settings_machine(Some(remote), cx);
        })
    });
    let qass = via_sent(&mut rx, remote)
        .into_iter()
        .find(|r| matches!(r, ControlRequest::QassWatch { .. }))
        .expect("qass read through Via");
    let before = desk.read_with(&vcx, |d, _| d.model.qass_watch);
    vcx.update(|_, cx| desk.update(cx, |d, cx| d.show_settings_machine(None, cx)));
    answer(
        &desk,
        &mut vcx,
        ControlResponse::QassWatch {
            id: qass.request_id().to_string(),
            on: before != Some(true),
        },
    );
    assert_eq!(desk.read_with(&vcx, |d, _| d.model.qass_watch), before);
    assert_eq!(desk.read_with(&vcx, |d, _| d.settings.remote.qass), None);
    assert_eq!(desk.read_with(&vcx, |d, _| d.settings.said.clone()), None);
}

/// A link that stays up while the daemon behind it sends nothing: 15 s after its last answer
/// the machine reads "not reached, as of" that answer; its next answer brings it back.
#[gpui::test]
fn an_up_link_that_sends_nothing_goes_stale(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    let remote = 1i64 << 40 | 1;
    let up = || {
        let mut spaces = fixture().spaces;
        for w in &mut spaces {
            if crate::state::machine_of(w.id) == 1 {
                w.link = Some(LinkState::Up);
            }
        }
        ControlResponse::Workspaces {
            id: "mux".into(),
            workspaces: spaces,
        }
    };
    let pet_id = |rx: &mut UnboundedReceiver<Up>| {
        via_sent(rx, remote)
            .into_iter()
            .find_map(|r| match r {
                ControlRequest::Pet { id } => Some(id),
                _ => None,
            })
            .expect("the machine asked for a word")
    };
    let pet = |id: String| ControlResponse::Pet {
        id,
        rects: Vec::new(),
    };
    answer(&desk, &mut vcx, up());
    let id = pet_id(&mut rx);
    answer(&desk, &mut vcx, pet(id));
    let set_now = |vcx: &mut VisualTestContext, now: i64| {
        vcx.update(|_, cx| {
            desk.update(cx, |d, cx| {
                d.now = now;
                cx.notify();
            })
        })
    };
    set_now(&mut vcx, 2000 + crate::state::STALE_AFTER - 1);
    assert_eq!(desk.read_with(&vcx, |d, _| d.banner()), None, "still heard");
    set_now(&mut vcx, 2000 + crate::state::STALE_AFTER);
    let zone = desk.read_with(&vcx, |d, _| d.zone);
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.banner()),
        Some(format!(
            "staging not reached, as of {}",
            crate::state::clock(2000, zone)
        ))
    );
    // The list comes again with the link still up: a new ask, and its answer.
    answer(&desk, &mut vcx, up());
    let id = pet_id(&mut rx);
    answer(&desk, &mut vcx, pet(id));
    assert_eq!(desk.read_with(&vcx, |d, _| d.banner()), None, "heard again");
}

/// A busy remote whose pings wait behind its own traffic stays reached while its terminals
/// speak: any event, screen or answer from it counts, and no second ping goes while the first
/// is out.
#[gpui::test]
fn any_traffic_from_a_machine_counts_as_heard(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    let remote = 1i64 << 40 | 1;
    let up = || {
        let mut spaces = fixture().spaces;
        for w in &mut spaces {
            if crate::state::machine_of(w.id) == 1 {
                w.link = Some(LinkState::Up);
            }
        }
        ControlResponse::Workspaces {
            id: "mux".into(),
            workspaces: spaces,
        }
    };
    let pings = |rx: &mut UnboundedReceiver<Up>| {
        via_sent(rx, remote)
            .into_iter()
            .filter(|r| matches!(r, ControlRequest::Pet { .. }))
            .count()
    };
    answer(&desk, &mut vcx, up());
    assert_eq!(pings(&mut rx), 1);
    let at = |vcx: &mut VisualTestContext, now: i64| {
        vcx.update(|_, cx| {
            desk.update(cx, |d, cx| {
                d.now = now;
                cx.notify();
            })
        })
    };
    // Heard once through its answer to something else it was asked.
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.ask(ControlRequest::Scrollback {
                id: "older".into(),
                terminal: 0x8100_0001,
                up: 0,
                rows: 5,
            });
            d.apply(
                Down::Answer(Box::new(ControlResponse::Scrollback {
                    id: "older".into(),
                    terminal: 0x8100_0001,
                    from: 0,
                    total: 0,
                    lines: Vec::new(),
                })),
                window,
                cx,
            );
        })
    });
    // Its first ping is still out ten seconds on: no second one.
    at(&mut vcx, 2010);
    answer(&desk, &mut vcx, up());
    assert_eq!(pings(&mut rx), 0, "one ping out at a time");
    // An event about its terminal keeps it reached past 15 s from the answer.
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.apply(
                Down::Event(qoop_runtime::control::EventMessage::AgentChanged {
                    terminal: 0x8100_0001,
                }),
                window,
                cx,
            )
        })
    });
    at(&mut vcx, 2020);
    assert_eq!(desk.read_with(&vcx, |d, _| d.banner()), None, "still heard");
    at(&mut vcx, 2010 + crate::state::STALE_AFTER);
    assert!(
        desk.read_with(&vcx, |d, _| d.banner()).is_some(),
        "silent 15 s"
    );
}

/// A healthy idle remote: no events, no screens, only the answers to its pings, which come back
/// under the inner request's id. It stays connected past 15 s from any one answer.
#[gpui::test]
fn an_idle_remote_that_answers_its_pings_stays_live(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    let remote = 1i64 << 40 | 1;
    let up = || {
        let mut spaces = fixture().spaces;
        for w in &mut spaces {
            if crate::state::machine_of(w.id) == 1 {
                w.link = Some(LinkState::Up);
            }
        }
        ControlResponse::Workspaces {
            id: "mux".into(),
            workspaces: spaces,
        }
    };
    let ping = |rx: &mut UnboundedReceiver<Up>| {
        via_sent(rx, remote)
            .into_iter()
            .find_map(|r| match r {
                ControlRequest::Pet { id } => Some(id),
                _ => None,
            })
            .expect("a ping")
    };
    let at = |vcx: &mut VisualTestContext, now: i64| {
        vcx.update(|_, cx| {
            desk.update(cx, |d, cx| {
                d.now = now;
                cx.notify();
            })
        })
    };
    answer(&desk, &mut vcx, up());
    for now in [2000, 2010, 2020, 2030] {
        at(&mut vcx, now);
        // As the clock asks: the last ping was answered, so the next goes.
        answer(&desk, &mut vcx, up());
        let id = ping(&mut rx);
        answer(
            &desk,
            &mut vcx,
            ControlResponse::Pet {
                id,
                rects: Vec::new(),
            },
        );
    }
    at(&mut vcx, 2030 + crate::state::STALE_AFTER - 1);
    assert_eq!(desk.read_with(&vcx, |d, _| d.banner()), None, "still live");
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.machine_state(1)),
        crate::state::MachineState::Live
    );
}

/// An idle remote on a qoop from before pets (accept pass 4, staging on 0.17.1): its daemon
/// refuses every ping as unknown. That refusal is its daemon answering, so the machine is asked
/// again on the clock and stays connected, where it used to go "not reached" 15 s after the link
/// came up and stay so while the link held.
#[gpui::test]
fn an_idle_remote_from_before_pets_stays_live_on_its_refusals(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    let remote = 1i64 << 40 | 1;
    let mut spaces = fixture().spaces;
    for w in &mut spaces {
        if crate::state::machine_of(w.id) == 1 {
            w.link = Some(LinkState::Up);
        }
    }
    let up = ControlResponse::Workspaces {
        id: "mux".into(),
        workspaces: spaces,
    };
    answer(&desk, &mut vcx, up.clone());
    for now in [2000, 2010, 2020, 2030] {
        vcx.update(|_, cx| {
            desk.update(cx, |d, cx| {
                d.now = now;
                cx.notify();
            })
        });
        answer(&desk, &mut vcx, up.clone());
        let id = via_sent(&mut rx, remote)
            .into_iter()
            .find_map(|r| match r {
                ControlRequest::Pet { id } => Some(id),
                _ => None,
            })
            .unwrap_or_else(|| panic!("a ping at {now}, after a refused one"));
        answer(
            &desk,
            &mut vcx,
            ControlResponse::Failed {
                id,
                reason: "this daemon is older than the client that called it: it does not \
                         know the method that was asked for (unknown variant `pet`)"
                    .into(),
            },
        );
    }
    vcx.update(|_, cx| {
        desk.update(cx, |d, cx| {
            d.now = 2030 + crate::state::STALE_AFTER - 1;
            cx.notify();
        })
    });
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.machine_state(1)),
        crate::state::MachineState::Live
    );
    assert_eq!(desk.read_with(&vcx, |d, _| d.banner()), None, "no banner");
}

/// A machine whose link is down: its space reads by its own name (the ssh error only in the
/// sidebar line's tooltip), that line offers Retry now, and Retry now or the window coming
/// back asks it at once, which makes the mux try it then rather than at the end of its backoff.
#[gpui::test]
fn a_down_machine_reads_by_name_and_retries_at_once(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    let remote = 1i64 << 40 | 1;
    let mut spaces = fixture().spaces;
    for w in &mut spaces {
        if crate::state::machine_of(w.id) == 1 {
            w.link = Some(LinkState::Down);
            w.name = format!("{} (old)", w.name);
            w.link_reason = Some(
                "offline: ssh: connect to host staging port 22: Network is unreachable".into(),
            );
        }
    }
    answer(
        &desk,
        &mut vcx,
        ControlResponse::Workspaces {
            id: "mux".into(),
            workspaces: spaces,
        },
    );
    let name = desk.read_with(&vcx, |d, _| {
        d.model
            .spaces
            .iter()
            .find(|w| crate::state::machine_of(w.id) == 1)
            .map(|w| w.name.clone())
    });
    assert_eq!(
        name.as_deref(),
        Some("staging:billing-api (old)"),
        "a name of its own"
    );
    assert!(
        desk.read_with(&vcx, |d, _| d.link_reasons.get(&1).cloned())
            .is_some_and(|r| r.contains("Network is unreachable"))
    );
    assert!(
        vcx.debug_bounds("retry-1").is_some(),
        "Retry now on its space's machine line"
    );
    // Its computer page says why and carries the same Retry now.
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Computer(1), window, cx)));
    vcx.run_until_parked();
    let retry = vcx
        .debug_bounds("retry-page-1")
        .expect("Retry now on the page");
    sent(&mut rx);
    vcx.simulate_click(retry.center(), gpui::Modifiers::none());
    assert_eq!(
        via_sent(&mut rx, remote)
            .iter()
            .filter(|r| matches!(r, ControlRequest::Pet { .. }))
            .count(),
        1,
        "the page's Retry now asks it"
    );
    sent(&mut rx);
    vcx.update(|_, cx| desk.update(cx, |d, _| d.retry_machine(1)));
    let asked = |rx: &mut UnboundedReceiver<Up>| {
        via_sent(rx, remote)
            .iter()
            .filter(|r| matches!(r, ControlRequest::Pet { .. }))
            .count()
    };
    assert_eq!(asked(&mut rx), 1, "Retry now asks it");
    vcx.update(|_, cx| desk.update(cx, |d, _| d.retry_away()));
    assert_eq!(asked(&mut rx), 1, "the window back asks it too");
}

/// A question card `id` of the agent in `terminal`, `status` as the daemon lists it.
fn tab_card(id: &str, terminal: u32, status: &str) -> qoop_runtime::control::AskInfo {
    serde_json::from_value(serde_json::json!({
        "card": id, "terminal": terminal, "status": status, "opened_at": 1000,
        "body": {
            "handle": "soak", "urgency": "later", "doing": "running the soak test",
            "why": "the migration touches billing", "asks": format!("{id}: run the migration?"),
            "options": [
                {"key": "a", "label": "Run it", "reason": "the test passed", "words": "run it"},
                {"key": "b", "label": "Wait", "reason": "after the release", "words": "wait"},
            ],
        },
    }))
    .expect("card")
}

/// The count beside an agent on Home (the sidebar's, off Home, in the next test): its open cards only, nothing at zero,
/// and never on a remote agent whose number matches a card's (a card names this Mac's
/// terminal; the mux numbers a remote's its own way).
#[gpui::test]
fn an_agent_row_counts_its_open_question_cards(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    vcx.update(|_, cx| {
        desk.update(cx, |d, cx| {
            d.asks = vec![
                tab_card("Q1", 1, "open"),
                tab_card("Q2", 1, "open"),
                tab_card("Q3", 1, "passed"),
                tab_card("Q4", 0x8100_0001, "open"),
            ];
            cx.notify();
        })
    });
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("tree-cards-1").is_some(),
        "the sidebar counts"
    );
    assert!(
        vcx.debug_bounds("tree-cards-3").is_none(),
        "nothing at zero"
    );
    assert!(
        vcx.debug_bounds("tree-cards-2164260865").is_none(),
        "not a remote agent's"
    );
    desk.read_with(&vcx, |d, _| {
        assert_eq!(d.cards_of(1, 1).len(), 2, "a passed card is not counted");
        assert!(d.cards_of(0x8100_0001, 1 << 40 | 1).is_empty());
    });
}

/// The Session page shows its agent's cards and no other's: the oldest opened in place, the
/// rest one row each; a click on a row opens that one. A waiting agent with no card says so,
/// and before the task is read its progress comes from the agent's row.
#[gpui::test]
fn the_session_page_lists_its_agents_cards(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.asks = vec![
                tab_card("Q1", 1, "open"),
                tab_card("Q2", 1, "open"),
                tab_card("Q9", 5, "open"),
            ];
            d.open(Page::Session(1), window, cx);
        })
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("card-Q1").is_some(), "the oldest opened");
    assert!(
        vcx.debug_bounds("tree-cards-1").is_some(),
        "the sidebar counts, off Home"
    );
    assert!(
        vcx.debug_bounds("card-row-Q2").is_some(),
        "the other folded"
    );
    assert!(vcx.debug_bounds("card-row-Q9").is_none() && vcx.debug_bounds("card-Q9").is_none());
    let at = vcx.debug_bounds("card-row-Q2").expect("a row").center();
    vcx.simulate_click(at, gpui::Modifiers::none());
    assert!(vcx.debug_bounds("card-Q2").is_some() && vcx.debug_bounds("card-row-Q1").is_some());

    vcx.update(|_, cx| {
        desk.update(cx, |d, cx| {
            d.asks.clear();
            d.strip = None;
            let row = d.model.rows.iter_mut().find(|r| r.terminal == 1).unwrap();
            row.task = Some(qoop_runtime::control::TaskRef {
                id: 12,
                goal: "ship the soak test".into(),
                stage: "waiting-human".into(),
                claimed: false,
                item: Some(3),
                steps_done: 2,
                steps_total: 4,
            });
            cx.notify();
        })
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("tab-questions").is_some(), "says it waits");
    let at = vcx.debug_bounds("task-toggle").expect("Task").center();
    vcx.simulate_click(at, gpui::Modifiers::none());
    assert!(
        vcx.debug_bounds("task-progress").is_some(),
        "2 of 4 from the row"
    );
}

/// The owner's own words and the hand-off on a card are held like any answer, then written:
/// the words as text, the hand-off as `to_qass` with no option and no words, which the card
/// then calls handed to qass.
#[gpui::test]
fn a_cards_own_words_and_hand_off_are_written_after_the_hold(cx: &mut TestAppContext) {
    use qoop_runtime::control::AskOp;
    let (desk, mut vcx, mut rx) = open(cx);
    let written =
        |desk: &Entity<Desk>, vcx: &mut VisualTestContext, rx: &mut UnboundedReceiver<Up>| {
            vcx.update(|_, cx| {
                desk.update(cx, |d, _| {
                    for hold in &mut d.holds.held {
                        hold.until = std::time::Instant::now();
                    }
                    d.fire();
                })
            });
            let asked = request_of(&sent(rx), |r| matches!(r, ControlRequest::AskWrite { .. }))
                .expect("written after the hold");
            let ControlRequest::AskWrite { id, action, .. } = asked else {
                unreachable!()
            };
            (id, action)
        };
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.asks = vec![tab_card("Q1", 1, "open")];
            d.open(Page::Session(1), window, cx);
            d.open_own(1, Some("Q1".into()), window, cx);
            let own = d.own.as_ref().expect("the field").field.clone();
            own.update(cx, |f, _| f.text = "  only after the release ".into());
        })
    });
    vcx.run_until_parked();
    vcx.update(|window, cx| desk.update(cx, |d, cx| assert!(d.own_enter(window, cx))));
    sent(&mut rx);
    let (_, action) = written(&desk, &mut vcx, &mut rx);
    assert_eq!(
        action,
        AskOp::Answer {
            card: "Q1".into(),
            option: None,
            text: Some("only after the release".into()),
            seen: None,
            to_qass: false,
        }
    );
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.card_to_qass("Q1", window, cx)));
    let (id, action) = written(&desk, &mut vcx, &mut rx);
    assert_eq!(
        action,
        AskOp::Answer {
            card: "Q1".into(),
            option: None,
            text: None,
            seen: None,
            to_qass: true,
        }
    );
    answer(
        &desk,
        &mut vcx,
        ControlResponse::AskWritten {
            id,
            card: "Q1".into(),
            result: qoop_runtime::control::AskResult::Passed,
        },
    );
    let said = desk.read_with(&vcx, |d, _| {
        d.said.get(&crate::state::Slot::Card("Q1".into())).cloned()
    });
    assert!(
        matches!(&said, Some(crate::question::Said::Sent { label, .. }) if label == "Handed to qass"),
        "{said:?}"
    );
}

// The Inbox page: one list, the ring, the digits, the hold bar, the chips; the dividers.

/// The fixture's agents with `menus` asking: `(terminal, seal, turn, last active)`. Each is
/// blocked with an edit menu of its own text seal and its own turn; terminal 2 stays blocked
/// with no menu (a dialog) unless it is one of them.
fn asking(menus: &[(u32, u64, u64, i64)]) -> ControlResponse {
    let mut rows = crate::state::tests::fixture().rows;
    for row in &mut rows {
        // The fixture's agent on a dialog has waited the least: it never takes the first place.
        row.last_active_at_unix = Some(500);
        if let Some((_, seal, turn, at)) = menus.iter().find(|m| m.0 == row.terminal) {
            row.status = Some("blocked".into());
            row.state = "blocked".into();
            row.menu = Some(crate::state::tests::edit_menu(*seal));
            row.menu_seal = Some(*turn);
            row.last_active_at_unix = Some(*at);
        }
    }
    ControlResponse::Agents {
        id: "agents".into(),
        agents: rows,
        splits: Vec::new(),
        qass_checks: None,
    }
}

fn held_slot(desk: &Entity<Desk>, vcx: &VisualTestContext) -> Option<crate::state::Slot> {
    desk.read_with(vcx, |d, _| d.holds.held.last().map(|h| h.slot.clone()))
}

/// One card per agent, oldest first: an open card about an agent that is on a dialog replaces
/// its row, and an agent's own menu keeps its place by when it began waiting.
#[gpui::test]
fn the_inbox_lists_one_card_per_agent_oldest_first(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    // 3 has waited since 100, 1 since 300; 2 is on a dialog (no menu) since 200.
    answer(&desk, &mut vcx, {
        let ControlResponse::Agents {
            mut agents,
            id,
            splits,
            qass_checks,
        } = asking(&[(3, 33, 1, 100), (1, 11, 1, 300)])
        else {
            unreachable!()
        };
        agents[1].last_active_at_unix = Some(200);
        ControlResponse::Agents {
            agents,
            id,
            splits,
            qass_checks,
        }
    });
    let order = |vcx: &VisualTestContext| {
        desk.read_with(vcx, |d, _| {
            d.inbox()
                .items
                .iter()
                .map(|i| i.terminal)
                .collect::<Vec<_>>()
        })
    };
    assert_eq!(order(&vcx), [Some(3), Some(2), Some(1)], "oldest first");
    // A card (opened at 1000) about 2: the dialog row gives way, one card for the agent.
    vcx.update(|_, cx| {
        desk.update(cx, |d, cx| {
            d.asks = vec![tab_card("Q9", 2, "open")];
            cx.notify();
        })
    });
    vcx.run_until_parked();
    let slots = desk.read_with(&vcx, |d, _| {
        d.inbox()
            .items
            .iter()
            .map(|i| i.slot.clone())
            .collect::<Vec<_>>()
    });
    use crate::state::Slot;
    assert_eq!(
        slots,
        [Slot::Agent(3), Slot::Agent(1), Slot::Card("Q9".into())],
        "one item for agent 2, its card"
    );
    let y = |vcx: &mut VisualTestContext, what: &'static str| {
        vcx.debug_bounds(what)
            .unwrap_or_else(|| panic!("{what} is not drawn"))
            .origin
            .y
    };
    let ys = ["inbox-card-t3", "inbox-card-t1", "inbox-card-Q9"].map(|s| y(&mut vcx, s));
    assert!(
        ys.windows(2).all(|w| w[0] < w[1]),
        "drawn in that order: {ys:?}"
    );
    assert!(
        vcx.debug_bounds("inbox-card-t2").is_none(),
        "no second card for agent 2"
    );
}

/// A digit answers the card with the ring, never the first one: Cmd-J moves the ring on, and the
/// same digit then answers the next card. `u` undoes the pick held on the focused card.
#[gpui::test]
fn digits_answer_the_card_with_the_ring(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    answer(&desk, &mut vcx, asking(&[(3, 33, 1, 100), (1, 11, 1, 300)]));
    assert_eq!(desk.read_with(&vcx, |d, _| d.page), Page::Inbox);
    std::thread::sleep(qoop_tui::answers::ARMING);
    use crate::state::Slot;
    vcx.simulate_keystrokes("1");
    assert_eq!(
        held_slot(&desk, &vcx),
        Some(Slot::Agent(3)),
        "the oldest has the ring"
    );
    vcx.simulate_keystrokes("u");
    assert_eq!(held_slot(&desk, &vcx), None, "undone");
    vcx.simulate_keystrokes("cmd-j");
    vcx.simulate_keystrokes("1");
    assert_eq!(
        held_slot(&desk, &vcx),
        Some(Slot::Agent(1)),
        "the ring moved, so did the answer"
    );
}

/// A pick on the Inbox waits in the hold bar, which names its target and offers Send now and
/// Undo; Send now writes the answer at once and the bar goes.
#[gpui::test]
fn the_hold_bar_names_its_target_and_send_now_sends_at_once(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    answer(&desk, &mut vcx, asking(&[(3, 33, 1, 100)]));
    std::thread::sleep(qoop_tui::answers::ARMING);
    assert!(vcx.debug_bounds("hold-bar").is_none(), "nothing held yet");
    vcx.simulate_keystrokes("1");
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("hold-bar").is_some(), "the bar");
    let who = desk.read_with(&vcx, |d, _| d.who_of(&crate::state::Slot::Agent(3)));
    assert_eq!(
        who, "acme-web/Coupon.ts test suggestion",
        "it names the agent"
    );
    sent(&mut rx);
    let at = vcx.debug_bounds("send-now").expect("Send now").center();
    vcx.simulate_click(at, gpui::Modifiers::none());
    let asked = request_of(&sent(&mut rx), |r| {
        matches!(r, ControlRequest::AnswerMenu { terminal: 3, .. })
    });
    assert!(asked.is_some(), "written at once, with no 3 s wait");
    assert!(desk.read_with(&vcx, |d, _| d.holds.held.is_empty()));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("hold-bar").is_none(), "the bar went");
}

/// The released bug: a menu asked again over the same text is another question. A held answer
/// to the first never goes to the second, whether it was picked on the agent's page or on the
/// Inbox: nothing is written, and the card says it was answered already.
#[gpui::test]
fn an_identical_question_asked_again_never_takes_a_held_answer(cx: &mut TestAppContext) {
    for on_inbox in [false, true] {
        let (desk, mut vcx, mut rx) = open(cx);
        // The same text seal (77) both times; the turn (`menu_seal`) is what differs.
        answer(&desk, &mut vcx, asking(&[(2, 77, 1, 100)]));
        if !on_inbox {
            vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(2), window, cx)));
            vcx.run_until_parked();
        }
        std::thread::sleep(qoop_tui::answers::ARMING);
        vcx.simulate_keystrokes("1");
        assert!(
            held_slot(&desk, &vcx).is_some(),
            "held (on the inbox: {on_inbox})"
        );
        answer(&desk, &mut vcx, asking(&[(2, 77, 2, 100)]));
        assert!(
            desk.read_with(&vcx, |d, _| d.holds.held.is_empty()),
            "the same words, another turn: the held answer is dropped (on the inbox: {on_inbox})"
        );
        sent(&mut rx);
        vcx.update(|_, cx| desk.update(cx, |d, _| d.fire()));
        assert!(
            request_of(&sent(&mut rx), |r| matches!(
                r,
                ControlRequest::AnswerMenu { .. }
            ))
            .is_none(),
            "nothing written to the second question (on the inbox: {on_inbox})"
        );
        let said = desk.read_with(&vcx, |d, _| {
            d.said.get(&crate::state::Slot::Agent(2)).cloned()
        });
        assert!(
            matches!(said, Some(crate::question::Said::Refused(_))),
            "{said:?}"
        );
    }
}

/// The chips: From qass keeps only the cards qass asked, the chip that filters is the clear
/// control, and Failed keeps the agents that failed.
#[gpui::test]
fn the_chips_filter_the_list_and_the_active_one_clears(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    let mut rows = crate::state::tests::fixture().rows;
    rows[0].status = Some("failed".into());
    rows[0].state = "failed".into();
    answer(
        &desk,
        &mut vcx,
        ControlResponse::Agents {
            id: "agents".into(),
            agents: rows,
            splits: Vec::new(),
            qass_checks: None,
        },
    );
    let mut card = tab_card("E5", 0, "open");
    card.terminal = None;
    vcx.update(|_, cx| {
        desk.update(cx, |d, cx| {
            d.asks = vec![card];
            cx.notify();
        })
    });
    vcx.run_until_parked();
    let drawn = |vcx: &mut VisualTestContext, what: &'static str| vcx.debug_bounds(what).is_some();
    assert!(drawn(&mut vcx, "inbox-card-E5") && drawn(&mut vcx, "inbox-card-t1"));
    let click = |vcx: &mut VisualTestContext, what: &'static str| {
        let at = vcx
            .debug_bounds(what)
            .unwrap_or_else(|| panic!("{what}"))
            .center();
        vcx.simulate_click(at, gpui::Modifiers::none());
        vcx.run_until_parked();
    };
    click(&mut vcx, "chip-qass");
    assert!(drawn(&mut vcx, "inbox-card-E5"), "qass's card stays");
    assert!(!drawn(&mut vcx, "inbox-card-t1") && !drawn(&mut vcx, "inbox-card-t2"));
    click(&mut vcx, "chip-qass");
    assert!(drawn(&mut vcx, "inbox-card-t1"), "pressed again it clears");
    click(&mut vcx, "chip-failed");
    assert!(drawn(&mut vcx, "inbox-card-t1"));
    assert!(!drawn(&mut vcx, "inbox-card-E5") && !drawn(&mut vcx, "inbox-card-t2"));
}

/// The sidebar and a drawer follow a drag of their divider and never leave their clamp: the
/// window keeps the main column its share, however far the mouse goes.
#[gpui::test]
fn a_divider_drag_sizes_the_sidebar_inside_its_clamp(cx: &mut TestAppContext) {
    use crate::shell::{SIDE_DEFAULT, SIDE_MIN, clamp_side};
    let (desk, mut vcx, _rx) = open(cx);
    let width =
        |vcx: &mut VisualTestContext| vcx.debug_bounds("sidebar").expect("sidebar").size.width;
    assert_eq!(width(&mut vcx), gpui::px(SIDE_DEFAULT));
    let grip = vcx
        .debug_bounds("divider-side")
        .expect("the divider")
        .center();
    vcx.simulate_mouse_down(grip, gpui::MouseButton::Left, gpui::Modifiers::none());
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.drag),
        Some(crate::shell::Edge::Side)
    );
    let window = vcx.update(|window, _| f32::from(window.viewport_size().width));
    let to = |x: f32| gpui::point(gpui::px(x), grip.y);
    vcx.simulate_mouse_move(to(340.), gpui::MouseButton::Left, gpui::Modifiers::none());
    vcx.run_until_parked();
    assert_eq!(width(&mut vcx), gpui::px(clamp_side(340., window)));
    vcx.simulate_mouse_move(to(5.), gpui::MouseButton::Left, gpui::Modifiers::none());
    vcx.run_until_parked();
    assert_eq!(
        width(&mut vcx),
        gpui::px(SIDE_MIN),
        "not narrower than its minimum"
    );
    vcx.simulate_mouse_move(
        to(window - 1.),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    vcx.run_until_parked();
    assert_eq!(
        width(&mut vcx),
        gpui::px(clamp_side(window - 1., window)),
        "nor wider than its most"
    );
    vcx.simulate_mouse_up(
        to(window - 1.),
        gpui::MouseButton::Left,
        gpui::Modifiers::none(),
    );
    assert_eq!(desk.read_with(&vcx, |d, _| d.drag), None, "released");
    // A move after the release moves nothing.
    vcx.simulate_mouse_move(to(300.), gpui::MouseButton::Left, gpui::Modifiers::none());
    vcx.run_until_parked();
    assert_eq!(width(&mut vcx), gpui::px(clamp_side(window - 1., window)));
}

/// An agent that ended its turn waiting for words has a reply card: the words are held like any
/// answer, typed into its terminal when the hold is up, and dropped if the agent moves on first.
#[gpui::test]
fn a_reply_is_held_typed_into_the_agent_and_dropped_when_it_moves_on(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    let waiting = |stop: &str| {
        let mut rows = crate::state::tests::fixture().rows;
        rows[1].status = Some("blocked".into());
        // Blocked by its task while no dialog is up: it waits for words, no menu.
        rows[1].state = "idle".into();
        rows[1].stop = Some(stop.into());
        ControlResponse::Agents {
            id: "agents".into(),
            agents: rows,
            splits: Vec::new(),
            qass_checks: None,
        }
    };
    answer(&desk, &mut vcx, waiting("17.1"));
    let at = vcx
        .debug_bounds("reply-open-2")
        .expect("a reply card")
        .center();
    vcx.simulate_click(at, gpui::Modifiers::none());
    vcx.simulate_input("go on, ship it");
    vcx.simulate_keystrokes("enter");
    let slot = crate::state::Slot::Agent(2);
    assert_eq!(
        held_slot(&desk, &vcx),
        Some(slot.clone()),
        "held, not typed"
    );
    sent(&mut rx);
    vcx.update(|_, cx| {
        desk.update(cx, |d, _| {
            for hold in &mut d.holds.held {
                hold.until = std::time::Instant::now();
            }
            d.fire();
        })
    });
    let ups = sent(&mut rx);
    let at = |pred: &dyn Fn(&Up) -> bool| ups.iter().position(pred);
    let paste = at(&|u| matches!(u, Up::Paste(2, b) if b == b"go on, ship it"));
    let enter = at(&|u| matches!(u, Up::Input(2, b) if b == b"\r"));
    assert!(
        paste.is_some() && enter.is_some() && paste < enter,
        "pasted, then a separate Enter, once the hold is up: {ups:?}"
    );
    // Held again, and the agent ends another turn first: the words are for the old one.
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.holds.start(qoop_tui::answers::Hold {
                slot: slot.clone(),
                generation: Some(1),
                menu_seal: None,
                seal: 0,
                pick: qoop_tui::answers::Pick::Reply {
                    text: "old".into(),
                    turn: Some("17.1".into()),
                },
                label: "old".into(),
                until: std::time::Instant::now() + qoop_tui::answers::HOLD,
            });
            let _ = (window, cx);
        })
    });
    answer(&desk, &mut vcx, waiting("17.2"));
    assert_eq!(held_slot(&desk, &vcx), None, "dropped: another turn");
}

// Review round 1: the reply guard, kept words, the ring, the quiet lists, the stopped line.

/// An agent waiting for words (terminal 2), its stop `stop`, as the agent list says it.
fn waiting_reply(stop: &str) -> ControlResponse {
    let mut rows = crate::state::tests::fixture().rows;
    rows[1].status = Some("blocked".into());
    rows[1].state = "idle".into();
    rows[1].stop = Some(stop.into());
    ControlResponse::Agents {
        id: "agents".into(),
        agents: rows,
        splits: Vec::new(),
        qass_checks: None,
    }
}

/// Hold the reply `words` for terminal 2 from the Inbox, as typed in its own words field.
fn hold_reply(desk: &Entity<Desk>, vcx: &mut VisualTestContext, words: &str) {
    let at = vcx
        .debug_bounds("reply-open-2")
        .expect("a reply card")
        .center();
    vcx.simulate_click(at, gpui::Modifiers::none());
    vcx.simulate_input(words);
    vcx.simulate_keystrokes("enter");
    assert_eq!(held_slot(desk, vcx), Some(crate::state::Slot::Agent(2)));
}

fn due(desk: &Entity<Desk>, vcx: &mut VisualTestContext) {
    vcx.update(|_, cx| {
        desk.update(cx, |d, _| {
            for hold in &mut d.holds.held {
                hold.until = std::time::Instant::now();
            }
            d.fire();
        })
    });
}

/// A reply is never typed into a line the person began in that terminal after it was held: a
/// key typed there during the hold means nothing is typed, the words are kept as a draft, the
/// card says why, and the next open of the field has them back.
#[gpui::test]
fn a_reply_is_not_typed_over_a_line_the_person_began_and_is_kept(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    answer(&desk, &mut vcx, waiting_reply("17.1"));
    hold_reply(&desk, &mut vcx, "go on, ship it");
    // The person goes to the agent and types a letter into its terminal.
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(2), window, cx)));
    vcx.run_until_parked();
    vcx.simulate_keystrokes("x");
    sent(&mut rx);
    due(&desk, &mut vcx);
    let ups = sent(&mut rx);
    assert!(
        !ups.iter()
            .any(|u| matches!(u, Up::Paste(..) | Up::Input(..))),
        "nothing typed into their line: {ups:?}"
    );
    let (draft, said) = desk.read_with(&vcx, |d, _| {
        let slot = crate::state::Slot::Agent(2);
        (d.drafts.get(&slot).cloned(), d.said.get(&slot).cloned())
    });
    assert_eq!(
        draft.as_deref(),
        Some("go on, ship it"),
        "the words are kept"
    );
    assert_eq!(
        said,
        Some(crate::question::Said::Refused(
            "You typed in that terminal; your reply is kept here.".into()
        ))
    );
    // Back on the Inbox the card offers the draft, and the field opens with it.
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Inbox, window, cx)));
    vcx.run_until_parked();
    let at = vcx.debug_bounds("reply-open-2").expect("the card").center();
    vcx.simulate_click(at, gpui::Modifiers::none());
    let text = desk.read_with(&vcx, |d, cx| {
        d.own.as_ref().map(|o| o.field.read(cx).text.clone())
    });
    assert_eq!(text.as_deref(), Some("go on, ship it"));
}

/// A reply waits while its machine cannot be reached: nothing is typed, the reply stays held,
/// and leaving keeps the words.
#[gpui::test]
fn a_reply_is_kept_while_the_daemon_is_away(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    answer(&desk, &mut vcx, waiting_reply("17.1"));
    hold_reply(&desk, &mut vcx, "later then");
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.apply(Down::Disconnected("gone".into()), window, cx)
        })
    });
    sent(&mut rx);
    due(&desk, &mut vcx);
    assert!(
        !sent(&mut rx)
            .iter()
            .any(|u| matches!(u, Up::Paste(..) | Up::Input(..))),
        "nothing typed while the daemon is away"
    );
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.holds.held.len()),
        1,
        "the reply stays held until a fresh list has come"
    );
    // Leaving with it still held: never typed by a number that may be another agent's, so the
    // words are kept as the card's draft.
    vcx.update(|_, cx| desk.update(cx, |d, _| d.flush_holds()));
    assert!(
        !sent(&mut rx)
            .iter()
            .any(|u| matches!(u, Up::Paste(..) | Up::Input(..))),
        "nothing typed on the way out"
    );
    assert_eq!(
        desk.read_with(&vcx, |d, _| d
            .drafts
            .get(&crate::state::Slot::Agent(2))
            .cloned())
            .as_deref(),
        Some("later then")
    );
}

/// What was typed in the field for one's own words survives the question going: it is kept per
/// card, the card says so, and the next question's field opens with it.
#[gpui::test]
fn words_typed_for_a_question_that_went_are_kept(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    answer(&desk, &mut vcx, asking(&[(2, 77, 1, 100)]));
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open_own(2, None, window, cx)));
    vcx.simulate_input("a long answer I had nearly finished");
    // The agent is answered elsewhere and works again: its question is gone.
    answer(&desk, &mut vcx, {
        let mut rows = crate::state::tests::fixture().rows;
        rows[1].status = Some("working".into());
        rows[1].state = "working".into();
        ControlResponse::Agents {
            id: "agents".into(),
            agents: rows,
            splits: Vec::new(),
            qass_checks: None,
        }
    });
    assert!(
        desk.read_with(&vcx, |d, _| d.own.is_none()),
        "the field closed"
    );
    let slot = crate::state::Slot::Agent(2);
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.drafts.get(&slot).cloned())
            .as_deref(),
        Some("a long answer I had nearly finished"),
        "not thrown away"
    );
    // The next question: the field opens with the words.
    answer(&desk, &mut vcx, asking(&[(2, 78, 2, 100)]));
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open_own(2, None, window, cx)));
    let text = desk.read_with(&vcx, |d, cx| {
        d.own.as_ref().map(|o| o.field.read(cx).text.clone())
    });
    assert_eq!(text.as_deref(), Some("a long answer I had nearly finished"));
    assert!(desk.read_with(&vcx, |d, _| !d.drafts.contains_key(&slot)));
}

/// The agent's own page offers Hand to qass by the shared rule: not on a card qass asked, not
/// for an agent qass leaves alone, and on an agent's own card.
#[gpui::test]
fn the_agent_page_offers_hand_to_qass_only_where_the_daemon_takes_it(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    let page = |vcx: &mut VisualTestContext, cards: Vec<qoop_runtime::control::AskInfo>| {
        vcx.update(|window, cx| {
            desk.update(cx, |d, cx| {
                d.asks = cards;
                d.open(Page::Session(1), window, cx);
                cx.notify();
            })
        });
        vcx.run_until_parked();
    };
    page(&mut vcx, vec![tab_card("Q1", 1, "open")]);
    assert!(
        vcx.debug_bounds("card-qass-Q1").is_some(),
        "an agent's own card"
    );
    page(&mut vcx, vec![tab_card("E3", 1, "open")]);
    assert!(vcx.debug_bounds("card-E3").is_some());
    assert!(
        vcx.debug_bounds("card-qass-E3").is_none(),
        "qass asked this one"
    );
    vcx.update(|_, cx| {
        desk.update(cx, |d, cx| {
            d.model.rows[0].qass_skip = true;
            d.note_hand();
            cx.notify();
        })
    });
    page(&mut vcx, vec![tab_card("Q1", 1, "open")]);
    assert!(
        vcx.debug_bounds("card-qass-Q1").is_none(),
        "qass leaves this agent alone"
    );
}

/// A waiting agent's age in the sidebar is how long it has waited, by the Inbox's own wait (the
/// oldest of its items), not when the agent was last active.
#[gpui::test]
fn the_sidebar_says_how_long_a_waiting_agent_has_waited(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    // 2 was last active at 1900 but its card has waited since 1000.
    answer(&desk, &mut vcx, asking(&[(2, 77, 1, 1900)]));
    vcx.update(|_, cx| desk.update(cx, |d, _| d.asks = vec![tab_card("Q9", 2, "open")]));
    let (waited, other) = desk.read_with(&vcx, |d, _| {
        let list = d.inbox();
        let row = d.model.row(2).expect("row");
        (
            d.side_age(crate::state::Entry::Live(row), &list),
            d.side_age(
                crate::state::Entry::Live(d.model.row(1).expect("row")),
                &list,
            ),
        )
    });
    assert_eq!(waited.as_deref(), Some("16m"), "now 2000, since 1000");
    assert_eq!(
        other.as_deref(),
        Some("25m"),
        "a working agent keeps its last activity (500)"
    );
}

/// A filter hides the quiet lists with the cards it hides; All brings them back.
#[gpui::test]
fn a_filter_hides_the_quiet_lists(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    assert!(
        vcx.debug_bounds("quiet-row-3").is_some(),
        "Finished, under All"
    );
    vcx.update(|_, cx| {
        desk.update(cx, |d, cx| {
            d.inbox_filter = crate::inbox::Filter::FromQass;
            cx.notify();
        })
    });
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("quiet-row-3").is_none(),
        "hidden under a filter"
    );
}

/// Answered today lists every row, or says "N more" that opens them; nothing is dropped, and a
/// card put away is not an answer.
#[gpui::test]
fn answered_today_lists_all_or_opens_the_rest_and_put_away_is_no_answer(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    // Tall enough that every row is on screen, where a click can reach it.
    vcx.simulate_resize(gpui::size(gpui::px(1280.), gpui::px(2600.)));
    let now = crate::app::unix_now();
    vcx.update(|_, cx| {
        desk.update(cx, |d, cx| {
            d.now = now;
            for n in 0..25 {
                d.own_answers.push(qoop_tui::inbox::OwnAnswer {
                    at: now - n,
                    label: format!("answer {n}"),
                    to: "acme-web x".into(),
                    card: None,
                });
            }
            cx.notify();
        })
    });
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("quiet-more").is_some(),
        "the rest are one press away"
    );
    assert!(vcx.debug_bounds("answered-19").is_some() && vcx.debug_bounds("answered-20").is_none());
    let at = vcx.debug_bounds("quiet-more").expect("more").center();
    vcx.simulate_click(at, gpui::Modifiers::none());
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("answered-24").is_some() && desk.read_with(&vcx, |d, _| d.quiet_all),
        "opened: the rows past the first twenty are listed"
    );
    // Put away a card: sent, but not an answer.
    let before = desk.read_with(&vcx, |d, _| d.own_answers.len());
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.asks = vec![tab_card("Q5", 1, "open")];
            d.put_away("Q5", window, cx);
        })
    });
    sent(&mut rx);
    due(&desk, &mut vcx);
    let asked = request_of(&sent(&mut rx), |r| {
        matches!(r, ControlRequest::AskWrite { .. })
    })
    .expect("written");
    let ControlRequest::AskWrite { id, .. } = asked else {
        unreachable!()
    };
    answer(
        &desk,
        &mut vcx,
        ControlResponse::AskWritten {
            id,
            card: "Q5".into(),
            result: qoop_runtime::control::AskResult::Passed,
        },
    );
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.own_answers.len()),
        before,
        "a card put away is not an answer"
    );
}

/// A pick that falls due while the link is down, or after it came back but before a fresh
/// agent list, is held: the terminal numbers may be another agent's after a restart. Once the
/// list has come and been checked, it is sent. And an answer still "Sending" when the link
/// drops says so instead of sending for good.
#[gpui::test]
fn a_pick_due_after_a_drop_waits_for_a_fresh_list(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    let agents = |seal: u64| {
        let mut rows = crate::state::tests::fixture().rows;
        rows[1].menu = Some(crate::state::tests::edit_menu(seal));
        ControlResponse::Agents {
            id: "agents".into(),
            agents: rows,
            splits: Vec::new(),
            qass_checks: None,
        }
    };
    answer(&desk, &mut vcx, agents(77));
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(2), window, cx)));
    vcx.run_until_parked();
    std::thread::sleep(qoop_tui::answers::ARMING);
    vcx.simulate_keystrokes("1");
    assert!(desk.read_with(&vcx, |d, _| {
        d.holds.of(&crate::state::Slot::Agent(2)).is_some()
    }));
    let due = |vcx: &mut VisualTestContext| {
        vcx.update(|_, cx| {
            desk.update(cx, |d, _| {
                for hold in &mut d.holds.held {
                    hold.until = std::time::Instant::now();
                }
                d.fire();
            })
        })
    };
    sent(&mut rx);
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.apply(Down::Disconnected("gone".into()), window, cx);
            d.apply(Down::Connected, window, cx);
        })
    });
    due(&mut vcx);
    assert!(
        request_of(&sent(&mut rx), |r| matches!(
            r,
            ControlRequest::AnswerMenu { .. }
        ))
        .is_none(),
        "sent against rows from before the drop"
    );
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.holds.held.len()),
        1,
        "still held"
    );
    // The fresh list has come: the same menu, so the pick stands and goes.
    answer(&desk, &mut vcx, agents(77));
    due(&mut vcx);
    assert!(
        request_of(&sent(&mut rx), |r| matches!(
            r,
            ControlRequest::AnswerMenu { terminal: 2, .. }
        ))
        .is_some(),
        "sent once the list was fresh"
    );
    // Sending when the link drops: the card says it was not confirmed.
    let said = desk.read_with(&vcx, |d, _| {
        d.said.get(&crate::state::Slot::Agent(2)).cloned()
    });
    assert!(
        matches!(said, Some(crate::question::Said::Sending { .. })),
        "{said:?}"
    );
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.apply(Down::Disconnected("gone".into()), window, cx)
        })
    });
    let said = desk.read_with(&vcx, |d, _| {
        d.said.get(&crate::state::Slot::Agent(2)).cloned()
    });
    assert!(
        matches!(said, Some(crate::question::Said::Refused(_))),
        "Sending stayed after the drop: {said:?}"
    );
}

/// A put away held on a card is dropped once the card is no longer open, on the read that says
/// so and before it is sent: the store would close a card whose answer the phone queued.
#[gpui::test]
fn a_put_away_is_dropped_once_its_card_leaves_open(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    let card = |status: &str| -> qoop_runtime::control::AskInfo {
        serde_json::from_value(serde_json::json!({
            "card": "E41", "agent": "staging/T3", "status": status, "opened_at": 1000,
            "body": {
                "handle": "soak", "urgency": "later", "doing": "running the soak test",
                "why": "the migration touches billing", "asks": "Run the migration?",
                "options": [{"key": "a", "label": "Run it", "reason": "ok", "words": "run it"}],
            },
        }))
        .expect("card")
    };
    let slot = crate::state::Slot::Card("E41".into());
    let hold = || qoop_tui::answers::Hold {
        slot: crate::state::Slot::Card("E41".into()),
        generation: None,
        menu_seal: None,
        seal: 0,
        pick: qoop_tui::answers::Pick::PutAway { card: "E41".into() },
        label: "Put away".into(),
        until: std::time::Instant::now(),
    };
    vcx.update(|_, cx| {
        desk.update(cx, |d, _| {
            d.asks = vec![card("open")];
            d.holds.start(hold());
            // The phone answered it before the hold ran out: it is queued now.
            d.asks = vec![card("queued")];
            d.fire();
        })
    });
    assert!(
        request_of(&sent(&mut rx), |r| matches!(
            r,
            ControlRequest::AskWrite { .. }
        ))
        .is_none(),
        "dismissed a card that was no longer open"
    );
    assert!(desk.read_with(&vcx, |d, _| d.holds.held.is_empty()));
    assert!(desk.read_with(&vcx, |d, _| matches!(
        d.said.get(&slot),
        Some(crate::question::Said::Refused(_))
    )));
    // Still open: put away as before.
    vcx.update(|_, cx| {
        desk.update(cx, |d, _| {
            d.asks = vec![card("open")];
            d.holds.start(hold());
            d.fire();
        })
    });
    assert!(
        request_of(&sent(&mut rx), |r| matches!(
            r,
            ControlRequest::AskWrite { .. }
        ))
        .is_some(),
        "an open card is put away"
    );
}

/// A long tab title wraps whole in its tab, which grows to hold it: never cut to an ellipsis.
#[gpui::test]
fn a_long_tab_title_wraps_whole_in_its_tab(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    let long = "refactor the whole billing pipeline so that every invoice is rendered by one \
                template and the old per customer overrides are gone for good";
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            let row = d
                .model
                .rows
                .iter_mut()
                .find(|r| r.terminal == 1)
                .expect("row");
            row.name = Some(long.into());
            row.agent_title = Some(long.into());
            d.open(Page::Session(1), window, cx);
        })
    });
    vcx.run_until_parked();
    let tab = vcx.debug_bounds("tab-title-1").expect("the tab");
    assert!(
        tab.size.height > gpui::px(40.),
        "the title stayed on one line: {:?}",
        tab.size
    );
}

/// `u` is Undo on the Inbox, and on an agent's page, the terminal focused, only while a pick for
/// that agent is held and the menu it was made on is still up (a card's: while it is open). Digit
/// then `u` undoes and sends nothing to the agent; with no pick held for the page, on another
/// agent's page, or once the menu is gone, `u` reaches the terminal and the pick stays held.
#[gpui::test]
fn u_is_undo_only_while_a_pick_for_this_page_is_held_on_a_menu_still_up(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    let menu = |seal: u64| {
        let mut rows = crate::state::tests::fixture().rows;
        rows[1].menu = Some(crate::state::tests::edit_menu(seal));
        ControlResponse::Agents {
            id: "agents".into(),
            agents: rows,
            splits: Vec::new(),
            qass_checks: None,
        }
    };
    answer(&desk, &mut vcx, menu(77));
    let hold = |slot: crate::state::Slot, seal, pick| qoop_tui::answers::Hold {
        slot,
        generation: None,
        menu_seal: None,
        seal,
        pick,
        label: "Yes".into(),
        until: std::time::Instant::now() + qoop_tui::answers::HOLD,
    };
    let agent = || {
        hold(
            crate::state::Slot::Agent(2),
            77,
            qoop_tui::answers::Pick::Key("1".into()),
        )
    };
    let typed = |rx: &mut UnboundedReceiver<Up>, terminal: u32| {
        sent(rx)
            .iter()
            .any(|u| matches!(u, Up::Input(t, b) if *t == terminal && b == b"u"))
    };
    // No pick held for the page: a letter for the terminal.
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(2), window, cx)));
    vcx.run_until_parked();
    sent(&mut rx);
    vcx.simulate_keystrokes("u");
    assert!(
        typed(&mut rx, 2),
        "nothing held: the letter went to the terminal"
    );
    // Another agent's page with agent 2's pick held: still a letter, and the pick stays.
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.holds.start(agent());
            d.open(Page::Session(3), window, cx);
        })
    });
    vcx.run_until_parked();
    sent(&mut rx);
    vcx.simulate_keystrokes("u");
    assert!(typed(&mut rx, 3), "another agent's page");
    assert_eq!(desk.read_with(&vcx, |d, _| d.holds.held.len()), 1, "kept");
    // The held agent's page with its terminal focused and its menu up: Undo, nothing typed.
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.open(Page::Session(2), window, cx);
            window.focus(&d.term_focus, cx);
        })
    });
    vcx.run_until_parked();
    sent(&mut rx);
    vcx.simulate_keystrokes("u");
    assert!(
        desk.read_with(&vcx, |d, _| d.holds.held.is_empty()),
        "undone"
    );
    assert!(
        !sent(&mut rx).iter().any(|u| matches!(u, Up::Input(..))),
        "typed"
    );
    // The menu it was made on is gone (the hold is still there): a letter again.
    vcx.update(|_, cx| {
        desk.update(cx, |d, _| {
            d.holds.start(agent());
            d.model.rows[1].menu = None;
        })
    });
    vcx.simulate_keystrokes("u");
    assert!(typed(&mut rx, 2), "the menu is gone");
    assert_eq!(desk.read_with(&vcx, |d, _| d.holds.held.len()), 1, "kept");
    // A pick held on one of this agent's open question cards is undone from the terminal too.
    let card: qoop_runtime::control::AskInfo = serde_json::from_value(serde_json::json!({
        "card": "E7", "agent": "T2", "terminal": 2, "status": "open", "opened_at": 1000,
        "body": {
            "handle": "soak", "urgency": "later", "doing": "d", "why": "w", "asks": "a?",
            "options": [{"key": "a", "label": "Run it", "reason": "r", "words": "run it"}],
        },
    }))
    .expect("card");
    vcx.update(|_, cx| {
        desk.update(cx, |d, _| {
            d.holds.held.clear();
            d.asks = vec![card];
            d.holds.start(hold(
                crate::state::Slot::Card("E7".into()),
                0,
                qoop_tui::answers::Pick::PutAway { card: "E7".into() },
            ));
        })
    });
    vcx.simulate_keystrokes("u");
    assert!(
        desk.read_with(&vcx, |d, _| d.holds.held.is_empty()),
        "the card's pick undone"
    );
}

/// The window's one-letter keys belong to a text field that has the keys: `u` typed in the
/// settings find field is a letter even while a pick is on hold, and Cmd-J and Cmd-N do nothing
/// over an open schedule sheet.
#[gpui::test]
fn text_fields_keep_their_keys_and_a_sheet_keeps_the_page(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Settings, window, cx)));
    vcx.simulate_keystrokes("/");
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.holds.start(qoop_tui::answers::Hold {
                slot: crate::state::Slot::Card("Q1".into()),
                generation: None,
                menu_seal: None,
                seal: 0,
                pick: qoop_tui::answers::Pick::PutAway { card: "Q1".into() },
                label: "Put away".into(),
                until: std::time::Instant::now() + qoop_tui::answers::HOLD,
            });
            let _ = (window, cx);
        })
    });
    vcx.simulate_input("u");
    let found = desk.read_with(&vcx, |d, cx| {
        d.settings.find.as_ref().map(|f| f.read(cx).text.clone())
    });
    assert_eq!(found.as_deref(), Some("u"), "typed, not taken for Undo");
    assert_eq!(
        held_slot(&desk, &vcx),
        Some(crate::state::Slot::Card("Q1".into()))
    );
    // A schedule sheet open: Cmd-J leaves the page where it is and Cmd-N stacks no sheet.
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Schedules, window, cx)));
    vcx.run_until_parked();
    let at = vcx
        .debug_bounds("new-schedule")
        .expect("New schedule")
        .center();
    vcx.simulate_click(at, gpui::Modifiers::none());
    assert!(desk.read_with(&vcx, |d, _| d.schedule_page.sheet.is_some()));
    vcx.simulate_keystrokes("cmd-j");
    vcx.simulate_keystrokes("cmd-n");
    assert_eq!(desk.read_with(&vcx, |d, _| d.page), Page::Schedules);
    assert!(
        desk.read_with(&vcx, |d, _| d.sheet.is_none()),
        "no New agent over the sheet"
    );
}

/// The ring follows the keyboard: Tab into a card's controls gives that card the ring, so the
/// digit answers the card the person is on.
#[gpui::test]
fn the_ring_follows_tab_into_a_card(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    answer(&desk, &mut vcx, asking(&[(3, 33, 1, 100), (1, 11, 1, 300)]));
    std::thread::sleep(qoop_tui::answers::ARMING);
    let second = crate::state::Slot::Agent(1);
    // Through the column's controls first: its rows, strip, filter and headings.
    for _ in 0..120 {
        vcx.simulate_keystrokes("tab");
        let on = vcx.update(|window, cx| {
            desk.read(cx)
                .card_focus
                .borrow()
                .get(&second)
                .is_some_and(|h| h.contains_focused(window, cx))
        });
        if on {
            break;
        }
    }
    vcx.run_until_parked();
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.inbox_cursor.clone()),
        Some(second.clone()),
        "the ring is on the card Tab reached"
    );
    vcx.simulate_keystrokes("1");
    assert_eq!(
        held_slot(&desk, &vcx),
        Some(second),
        "and the digit answers it"
    );
}

/// A mouse button let go outside the window ends a divider drag at once; it does not wait for
/// the pointer to come back. And a divider dragged across the terminal selects nothing in it.
#[gpui::test]
fn a_divider_drag_ends_outside_the_window_and_selects_nothing(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    let grip = vcx.debug_bounds("divider-side").expect("divider").center();
    vcx.simulate_mouse_down(grip, gpui::MouseButton::Left, gpui::Modifiers::none());
    assert!(desk.read_with(&vcx, |d, _| d.drag.is_some()));
    let window = vcx.update(|window, _| window.viewport_size());
    let out = gpui::point(window.width + gpui::px(60.), grip.y);
    vcx.simulate_mouse_up(out, gpui::MouseButton::Left, gpui::Modifiers::none());
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.drag),
        None,
        "released outside"
    );
    // Over a terminal with a selection already started, a drag of a divider leaves it alone.
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(1), window, cx)));
    vcx.run_until_parked();
    vcx.update(|_, cx| {
        desk.update(cx, |d, _| {
            d.term.as_mut().expect("pane").selection = Some(((0, 0), (0, 0)));
            d.drag = Some(crate::shell::Edge::Drawer);
        })
    });
    let over = gpui::point(window.width * 0.5, window.height * 0.5);
    vcx.simulate_mouse_move(over, gpui::MouseButton::Left, gpui::Modifiers::none());
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.term.as_ref().and_then(|t| t.selection)),
        Some(((0, 0), (0, 0))),
        "the selection did not follow the divider"
    );
}

/// qass cannot take a question: the Inbox says so with the step, and leaves Hand to qass out
/// of every card.
#[gpui::test]
fn the_inbox_says_when_qass_is_stopped_and_starts_it(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    answer(&desk, &mut vcx, asking(&[(2, 77, 1, 100)]));
    assert!(
        vcx.debug_bounds("qass-stopped").is_none(),
        "qass runs and watches"
    );
    assert!(vcx.debug_bounds("inbox-2-4").is_some() || vcx.debug_bounds("inbox-2-5").is_some());
    // Not watching: Start watching.
    vcx.update(|_, cx| {
        desk.update(cx, |d, cx| {
            d.model.qass_watch = Some(false);
            d.note_hand();
            cx.notify();
        })
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("qass-stopped").is_some());
    sent(&mut rx);
    let at = vcx
        .debug_bounds("qass-watch")
        .expect("Start watching")
        .center();
    vcx.simulate_click(at, gpui::Modifiers::none());
    assert!(
        request_of(&sent(&mut rx), |r| matches!(
            r,
            ControlRequest::QassWatch { on: Some(true), .. }
        ))
        .is_some(),
        "asks qass to watch"
    );
    // Not running: Start qass, and no Hand to qass anywhere.
    vcx.update(|_, cx| {
        desk.update(cx, |d, cx| {
            d.model.rows.retain(|r| r.workspace != 3);
            d.note_hand();
            cx.notify();
        })
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("qass-start").is_some(), "Start qass");
    assert!(
        vcx.debug_bounds("card-qass-Q1").is_none() && vcx.debug_bounds("inbox-2-5").is_none(),
        "no Hand to qass while it is stopped"
    );
}

/// The words a person types are the hold's label whole: no cut, no quotes of their own.
#[gpui::test]
fn typed_words_are_the_hold_label_whole(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    answer(&desk, &mut vcx, waiting_reply("17.1"));
    let words = "this is a reply that is clearly longer than forty characters, said whole";
    hold_reply(&desk, &mut vcx, words);
    let label = desk.read_with(&vcx, |d, _| d.holds.held.last().map(|h| h.label.clone()));
    assert_eq!(label.as_deref(), Some(words));
}

// Desktop round 5: the agent page.

/// `ctrl-a tab` opens the agent that has waited longest, and so does the header's Next
/// waiting, which says how many other things wait; from the agent that waits there is nothing
/// else, and the control says so.
#[gpui::test]
fn ctrl_a_tab_and_next_waiting_open_the_agent_that_waits(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(1), window, cx)));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("next-waiting").is_some(), "in the header");
    vcx.simulate_keystrokes("ctrl-a tab");
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.page),
        Page::Session(2),
        "the agent on a dialog"
    );
    // Back on a quiet agent, the header's control does the same.
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(1), window, cx)));
    vcx.run_until_parked();
    let at = vcx.debug_bounds("next-waiting").expect("header").center();
    vcx.simulate_click(at, gpui::Modifiers::none());
    assert_eq!(desk.read_with(&vcx, |d, _| d.page), Page::Session(2));
    // On the agent that waits there is nothing else: the control is drawn, with no count, and
    // pressing it goes nowhere.
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("next-waiting").is_some(),
        "still in the header"
    );
    assert!(
        vcx.debug_bounds("next-waiting-count").is_none(),
        "no count when nothing else waits"
    );
    let at = vcx.debug_bounds("next-waiting").expect("header").center();
    vcx.simulate_click(at, gpui::Modifiers::none());
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.page),
        Page::Session(2),
        "nowhere to go"
    );
    // On a quiet agent it carries the count.
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(1), window, cx)));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("next-waiting-count").is_some());
}

/// The agent page keeps the question under the terminal with what is asked wrapped whole, the
/// panel that used to sit beside it is gone, and the task strip is a drawer that opens and
/// closes.
#[gpui::test]
fn the_agent_page_has_the_card_under_the_terminal_and_drawers_closed(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    let long = "Edit file: crates/qoop-desktop/src/session.rs and every word of this line has to \
        stay on screen whatever the width of the window is, wrapped onto the next line";
    let mut rows = crate::state::tests::fixture().rows;
    let mut menu = crate::state::tests::edit_menu(77);
    menu.context = vec![long.to_string()];
    rows[1].menu = Some(menu);
    answer(
        &desk,
        &mut vcx,
        ControlResponse::Agents {
            id: "agents".into(),
            agents: rows,
            splits: Vec::new(),
            qass_checks: None,
        },
    );
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(2), window, cx)));
    vcx.run_until_parked();
    let context = vcx.debug_bounds("question-context").expect("what is asked");
    assert!(
        context.size.height > gpui::px(30.),
        "one long line wraps to more than one: {:?}",
        context.size
    );
    let card = vcx.debug_bounds("question-card").expect("the card");
    assert!(
        card.contains(&context.origin),
        "the context is inside the card, not in a panel beside it"
    );
    // A tall card scrolls inside its own room: it never runs past the window.
    vcx.simulate_resize(gpui::size(gpui::px(1280.), gpui::px(700.)));
    vcx.run_until_parked();
    let below = vcx
        .debug_bounds("agent-below")
        .expect("the room under the terminal");
    assert!(
        below.bottom() <= gpui::px(700.) && below.size.height <= gpui::px(700. * 0.6 + 1.),
        "{below:?}"
    );
    assert!(vcx.debug_bounds("question-side").is_none(), "no side panel");
    assert!(vcx.debug_bounds("task-strip").is_none(), "closed at first");
    let at = vcx.debug_bounds("task-toggle").expect("Task").center();
    vcx.simulate_click(at, gpui::Modifiers::none());
    assert!(vcx.debug_bounds("task-strip").is_some(), "opened");
    let at = vcx.debug_bounds("task-toggle").expect("Task").center();
    vcx.simulate_click(at, gpui::Modifiers::none());
    assert!(vcx.debug_bounds("task-strip").is_none(), "closed again");
}

/// The words a person typed for an agent are never lost to it moving on: a reply held for a
/// turn that ended, and an answer in their own words held for a menu that was replaced, both
/// wait as the card's draft with the card saying so.
#[gpui::test]
fn words_held_for_an_agent_that_moved_on_are_kept(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    let slot = crate::state::Slot::Agent(2);
    let kept = |desk: &Entity<Desk>, vcx: &VisualTestContext| {
        desk.read_with(vcx, |d, _| {
            (d.drafts.get(&slot).cloned(), d.said.get(&slot).cloned())
        })
    };
    let moved_on = Some(crate::question::Said::Refused(
        "It moved on before your reply went; your words are kept.".into(),
    ));

    // A reply, held, and the agent ends another turn first.
    answer(&desk, &mut vcx, waiting_reply("17.1"));
    hold_reply(&desk, &mut vcx, "go on, ship it");
    answer(&desk, &mut vcx, waiting_reply("17.2"));
    assert_eq!(held_slot(&desk, &vcx), None, "not sent for the old turn");
    assert_eq!(
        kept(&desk, &vcx),
        (Some("go on, ship it".into()), moved_on.clone())
    );
    desk.update(&mut vcx, |d, _| {
        d.drafts.clear();
        d.said.clear();
    });

    // Their own words for a menu, held, and another menu replaces it.
    answer(&desk, &mut vcx, asking(&[(2, 77, 1, 100)]));
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open_own(2, None, window, cx)));
    vcx.simulate_input("only the tests, please");
    vcx.simulate_keystrokes("enter");
    assert_eq!(held_slot(&desk, &vcx), Some(slot.clone()), "held");
    answer(&desk, &mut vcx, asking(&[(2, 78, 2, 100)]));
    assert_eq!(held_slot(&desk, &vcx), None, "not sent to the new menu");
    assert_eq!(
        kept(&desk, &vcx),
        (Some("only the tests, please".into()), moved_on)
    );
}

// Desktop round 5: New agent.

fn agents_with(extra: Vec<qoop_runtime::control::AgentRow>) -> ControlResponse {
    let mut rows = crate::state::tests::fixture().rows;
    rows.extend(extra);
    ControlResponse::Agents {
        id: "agents".into(),
        agents: rows,
        splits: Vec::new(),
        qass_checks: None,
    }
}

/// New agent opens on the space of the page on screen: an agent's own space, a computer's first,
/// else the first space; and a space whose computer is not reached gives way to This Mac's, with
/// the words that say so.
#[gpui::test]
fn new_agent_opens_in_the_current_space(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    let remote = 1i64 << 40 | 1;
    let space_of = |desk: &Entity<Desk>, vcx: &VisualTestContext| {
        desk.read_with(vcx, |d, _| {
            d.sheet.as_ref().map(|s| (s.space, s.moved.clone()))
        })
    };
    let close = |vcx: &mut VisualTestContext| vcx.simulate_keystrokes("escape");

    // On an agent in the api space (2), not the first space.
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(5), window, cx)));
    vcx.run_until_parked();
    vcx.simulate_keystrokes("ctrl-a c");
    assert_eq!(space_of(&desk, &vcx), Some((Some(2), None)));
    close(&mut vcx);
    desk.update(&mut vcx, |d, _| d.work_draft = None);

    // On a remote agent.
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| d.open(Page::Session(0x8100_0001), window, cx))
    });
    vcx.run_until_parked();
    vcx.simulate_keystrokes("ctrl-a c");
    assert_eq!(space_of(&desk, &vcx), Some((Some(remote), None)));
    close(&mut vcx);
    desk.update(&mut vcx, |d, _| d.work_draft = None);

    // Its computer goes down: This Mac's first space is picked, and the sheet says why.
    let mut spaces = fixture().spaces;
    for w in &mut spaces {
        if crate::state::machine_of(w.id) == 1 {
            w.link = Some(LinkState::Down);
        }
    }
    answer(
        &desk,
        &mut vcx,
        ControlResponse::Workspaces {
            id: "mux".into(),
            workspaces: spaces,
        },
    );
    // Down for longer than a retry takes: not reached.
    desk.update(&mut vcx, |d, _| d.now += 30);
    vcx.simulate_keystrokes("ctrl-a c");
    assert_eq!(
        space_of(&desk, &vcx),
        Some((
            Some(1),
            Some("staging is not reached; This Mac is picked instead.".into())
        ))
    );
    assert!(vcx.debug_bounds("new-work-moved").is_some(), "drawn");
    close(&mut vcx);
    desk.update(&mut vcx, |d, _| d.work_draft = None);

    // Off any agent: the first space.
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Inbox, window, cx)));
    vcx.run_until_parked();
    vcx.simulate_keystrokes("ctrl-a c");
    assert_eq!(space_of(&desk, &vcx), Some((Some(1), None)));
}

/// The session's default space with nothing in it is not a place to work in yet: the sidebar and
/// Settings leave it out, and New agent opens on the page's space, else the space last worked
/// in, else the first space with agents. It shows as soon as a tab is in it.
#[gpui::test]
fn an_empty_default_space_stays_out_of_the_way(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    vcx.simulate_resize(gpui::size(gpui::px(1280.), gpui::px(2600.)));
    let mut spaces = fixture().spaces;
    let empty: qoop_runtime::control::WorkspaceRow =
        serde_json::from_value(serde_json::json!({"id": 7, "name": "default", "terminals": 0}))
            .expect("space");
    spaces.insert(0, empty);
    answer(
        &desk,
        &mut vcx,
        ControlResponse::Workspaces {
            id: "mux".into(),
            workspaces: spaces,
        },
    );
    let space_of = |desk: &Entity<Desk>, vcx: &VisualTestContext| {
        desk.read_with(vcx, |d, _| d.sheet.as_ref().map(|s| s.space))
    };
    let close = |vcx: &mut VisualTestContext| vcx.simulate_keystrokes("escape");

    // Settings: the empty default has no switch; the others do.
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Settings, window, cx)));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("watch-space-1").is_some());
    assert!(
        vcx.debug_bounds("watch-space-7").is_none(),
        "the empty default"
    );
    assert!(
        vcx.debug_bounds("side-space-7").is_none(),
        "nor in the sidebar"
    );

    // New agent from a page with no space of its own: the first space that has agents.
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Inbox, window, cx)));
    vcx.run_until_parked();
    vcx.simulate_keystrokes("ctrl-a c");
    assert_eq!(space_of(&desk, &vcx), Some(Some(1)));
    close(&mut vcx);
    desk.update(&mut vcx, |d, _| d.work_draft = None);

    // After an agent in api (2), off any agent again: the space last worked in.
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(5), window, cx)));
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Inbox, window, cx)));
    vcx.run_until_parked();
    vcx.simulate_keystrokes("ctrl-a c");
    assert_eq!(space_of(&desk, &vcx), Some(Some(2)));
    close(&mut vcx);
    desk.update(&mut vcx, |d, _| d.work_draft = None);

    // A tab in it: it is listed again.
    desk.update(&mut vcx, |d, _| {
        d.model.spaces[0].terminals = 1;
    });
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Settings, window, cx)));
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("watch-space-7").is_some(),
        "a tab is in it"
    );
}

/// Start types the shared line (the branch as typed) and the sheet then waits: an agent that
/// appears in its space is where the person lands and the sheet closes; one in another space, or
/// no new row, leaves the sheet saying it started.
#[gpui::test]
fn start_lands_on_the_agent_that_appears_in_its_space(cx: &mut TestAppContext) {
    use crate::state::tests::row;
    let (desk, mut vcx, mut rx) = open(cx);
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(5), window, cx)));
    vcx.run_until_parked();
    vcx.simulate_keystrokes("ctrl-a c");
    vcx.simulate_input("fix the flaky login test");
    // The branch is editable: tab to its field and name it.
    for _ in 0..4 {
        vcx.simulate_keystrokes("tab");
    }
    vcx.simulate_input("feat/login-flake");
    sent(&mut rx);
    vcx.simulate_keystrokes("enter");
    let asked = sent(&mut rx)
        .into_iter()
        .find_map(|u| match u {
            Up::Request(r) => match *r {
                ControlRequest::CreateTerminal { id, workspace, .. } => Some((id, workspace)),
                _ => None,
            },
            _ => None,
        })
        .expect("a terminal asked for");
    assert_eq!(asked.1, Some(2), "in the page's space");
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.apply(
                Down::Answer(Box::new(ControlResponse::TerminalCreated {
                    id: asked.0.clone(),
                    terminal: 12,
                })),
                window,
                cx,
            )
        })
    });
    let typed = sent(&mut rx)
        .into_iter()
        .find_map(|u| match u {
            Up::Input(12, bytes) => Some(String::from_utf8(bytes).expect("utf8")),
            _ => None,
        })
        .expect("the line typed");
    assert_eq!(
        typed,
        "qoop worktree new feat/login-flake --space 2 -- claude 'fix the flaky login test' && exit\r"
    );
    let waiting = |desk: &Entity<Desk>, vcx: &VisualTestContext| {
        desk.read_with(vcx, |d, _| {
            (d.page, d.sheet.as_ref().is_some_and(|s| s.started))
        })
    };
    assert_eq!(
        waiting(&desk, &vcx),
        (Page::Session(5), true),
        "started in qoop"
    );

    // The same list, then an agent in another space: nothing lands.
    answer(&desk, &mut vcx, agents_with(vec![]));
    answer(
        &desk,
        &mut vcx,
        agents_with(vec![row(30, 1, "working", "claude", "elsewhere")]),
    );
    assert_eq!(waiting(&desk, &vcx), (Page::Session(5), true));
    // A second Start while it waits starts nothing.
    sent(&mut rx);
    vcx.simulate_keystrokes("enter");
    assert!(!sent(&mut rx).iter().any(|u| matches!(u, Up::Request(_))));

    // An agent of another program in that space (a schedule, qass, another client) is not it.
    answer(
        &desk,
        &mut vcx,
        agents_with(vec![
            row(30, 1, "working", "claude", "elsewhere"),
            row(29, 2, "working", "codex", "somebody else's"),
        ]),
    );
    assert_eq!(
        waiting(&desk, &vcx),
        (Page::Session(5), true),
        "not taken for it"
    );
    // Once started the sheet's chips and fields are gone: the space cannot be changed under it.
    assert!(vcx.debug_bounds("space").is_none() && vcx.debug_bounds("machine").is_none());

    // The new agent in the sheet's space: landed, the sheet gone.
    answer(
        &desk,
        &mut vcx,
        agents_with(vec![
            row(30, 1, "working", "claude", "elsewhere"),
            row(31, 2, "working", "claude", "fix the flaky login test"),
        ]),
    );
    assert_eq!(
        desk.read_with(&vcx, |d, _| (d.page, d.sheet.is_none())),
        (Page::Session(31), true)
    );
}

// Desktop round 5: the Board.

/// A board as a daemon sends it: two items wait on the person, one is doing, one todo, one done.
fn board_items() -> serde_json::Value {
    let item = |id: i64, key: &str, state: &str, extra: serde_json::Value| {
        let mut v = serde_json::json!({
            "id": id, "key": key, "workspace": 1, "title": format!("title of {key}"),
            "state": state, "steps_done": 0, "steps_total": 0, "agents": [], "updated_at": id,
        });
        for (k, val) in extra.as_object().cloned().unwrap_or_default() {
            v[k] = val;
        }
        v
    };
    serde_json::json!([
        item(1, "QOOP-66", "done", serde_json::json!({"closed_at": 5})),
        item(2, "QOOP-68", "doing", serde_json::json!({"open_asks": 1})),
        item(3, "QOOP-71", "doing", serde_json::json!({})),
        item(4, "QOOP-72", "todo", serde_json::json!({})),
        item(5, "QOOP-73", "todo", serde_json::json!({"plan": "asked"})),
    ])
}

fn board_answer(id: &str, items: Option<serde_json::Value>) -> ControlResponse {
    serde_json::from_value(serde_json::json!({
        "op": "Tasks", "id": id, "tasks": [], "events": [], "more": false,
        "items": items,
    }))
    .expect("tasks")
}

/// The Board is a page of its own, from its entry in the column: read as it opens, in four bands (Waiting on you holds
/// what Needs you holds), four columns from a page 920 wide and two below it, narrowed by the search.
#[gpui::test]
fn the_board_lists_four_bands_in_four_columns_or_two(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    sent(&mut rx);
    let at = vcx
        .debug_bounds("nav-board")
        .expect("the Board's entry")
        .center();
    vcx.simulate_click(at, gpui::Modifiers::none());
    let read = request_of(&sent(&mut rx), |r| {
        matches!(r, ControlRequest::TaskRead { board: true, .. })
    })
    .expect("the board is read as it opens");
    assert!(
        matches!(
            &read,
            ControlRequest::TaskRead {
                task: None,
                terminal: None,
                ..
            }
        ),
        "no Via for This Mac: {read:?}"
    );
    answer(
        &desk,
        &mut vcx,
        board_answer(read.request_id(), Some(board_items())),
    );
    vcx.simulate_resize(gpui::size(gpui::px(1600.), gpui::px(900.)));
    vcx.run_until_parked();
    for band in ["band-wait", "band-doing", "band-todo", "band-done"] {
        assert!(vcx.debug_bounds(band).is_some(), "{band}");
    }
    assert!(
        vcx.debug_bounds("board-columns-4").is_some(),
        "four columns on a page 1320 wide"
    );
    // The waiting band is before the others, and its items are the ones that wait.
    assert!(
        vcx.debug_bounds("item-QOOP-68").is_some() && vcx.debug_bounds("item-QOOP-73").is_some()
    );
    let wait = vcx.debug_bounds("band-wait").expect("wait");
    let doing = vcx.debug_bounds("band-doing").expect("doing");
    assert!(wait.origin.x < doing.origin.x, "side by side in order");
    assert!(
        wait.contains(&vcx.debug_bounds("item-QOOP-68").expect("item").origin),
        "QOOP-68 (a question open) is in Waiting on you"
    );
    // A 1280 window leaves a page 1000 wide (the sidebar's 280 is taken): four columns.
    vcx.simulate_resize(gpui::size(gpui::px(1280.), gpui::px(800.)));
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("board-columns-4").is_some(),
        "four columns at 1280"
    );
    // The sidebar counts: dragged to its widest it leaves 1280 - 420 = 860, under 920.
    desk.update(&mut vcx, |d, cx| {
        d.side_w = crate::shell::SIDE_MAX;
        cx.notify();
    });
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("board-columns-2").is_some(),
        "two columns on a page 860 wide"
    );
    desk.update(&mut vcx, |d, cx| {
        d.side_w = crate::shell::SIDE_DEFAULT;
        cx.notify();
    });
    vcx.simulate_resize(gpui::size(gpui::px(1100.), gpui::px(800.)));
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("board-columns-2").is_some(),
        "two columns at 1100"
    );
    let (wait, todo) = (
        vcx.debug_bounds("band-wait").expect("wait"),
        vcx.debug_bounds("band-todo").expect("todo"),
    );
    assert!(
        todo.origin.y > wait.origin.y,
        "the second pair is under the first"
    );
    // The search narrows the items.
    desk.update(&mut vcx, |d, cx| {
        d.board.find.update(cx, |f, cx| {
            f.set_text("QOOP-71");
            cx.notify();
        })
    });
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("item-QOOP-71").is_some() && vcx.debug_bounds("item-QOOP-68").is_none()
    );
}

/// Another computer's board is read through `Via` under its own id, an older qoop there (no
/// `items`) is told by name, and an item added there goes through `Via` with no terminal.
#[gpui::test]
fn another_computers_board_is_read_and_added_to_through_via(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    let remote = 1i64 << 40 | 1;
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Board, window, cx)));
    sent(&mut rx);
    let at = vcx
        .debug_bounds("board-machine-1")
        .expect("staging")
        .center();
    vcx.simulate_click(at, gpui::Modifiers::none());
    let via = via_sent(&mut rx, remote);
    let read = via
        .iter()
        .find(|r| matches!(r, ControlRequest::TaskRead { board: true, .. }))
        .expect("read through Via")
        .clone();
    assert_eq!(read.request_id(), "board-read-1");
    // A qoop from before the board answers with no items.
    answer(&desk, &mut vcx, board_answer("board-read-1", None));
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("board-old").is_some(),
        "the old qoop is told"
    );
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.board.read_of(1)),
        qoop_tui::board::Read::Old
    );
    assert_eq!(
        qoop_tui::board::too_old("staging"),
        "qoop on staging is too old for the board"
    );
    // The board, once it answers: add an item to it.
    answer(
        &desk,
        &mut vcx,
        board_answer("board-read-1", Some(serde_json::json!([]))),
    );
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("board-empty").is_some());
    let at = vcx.debug_bounds("board-add").expect("Add item").center();
    vcx.simulate_click(at, gpui::Modifiers::none());
    vcx.simulate_input("check the soak");
    sent(&mut rx);
    vcx.simulate_keystrokes("enter");
    let writes = via_sent(&mut rx, remote);
    assert!(
        writes.iter().any(|r| matches!(
            r,
            ControlRequest::TaskWrite {
                terminal: None,
                token: None,
                action: qoop_runtime::control::TaskOp::Add { workspace: Some(w), goal, .. },
                ..
            } if *w == remote && goal == "check the soak"
        )),
        "{writes:?}"
    );
}

/// The tell-qass line says words to qass: pasted and sent while qass is at rest, kept (and the
/// line says why) while it works.
#[gpui::test]
fn the_tell_line_reaches_qass_only_while_it_is_at_rest(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Board, window, cx)));
    vcx.run_until_parked();
    let tell = |vcx: &mut VisualTestContext, desk: &Entity<Desk>, words: &str| {
        desk.update(vcx, |d, cx| {
            d.board.tell.update(cx, |f, _| f.set_text(words))
        });
        let at = vcx
            .debug_bounds("board-tell-send")
            .expect("Tell qass")
            .center();
        vcx.simulate_click(at, gpui::Modifiers::none());
    };
    // Working: kept.
    sent(&mut rx);
    tell(&mut vcx, &desk, "work on QOOP-73 next");
    assert!(
        !sent(&mut rx).iter().any(|u| matches!(u, Up::Paste(..))),
        "not while it works"
    );
    assert_eq!(
        desk.read_with(&vcx, |d, cx| d.board.tell.read(cx).text.clone()),
        "work on QOOP-73 next",
        "the words stay in the line"
    );
    // At rest: sent, the line emptied.
    let mut rows = crate::state::tests::fixture().rows;
    for r in rows.iter_mut().filter(|r| r.terminal == 9) {
        r.state = "idle".into();
        r.status = Some("idle".into());
        r.readiness = "ready".into();
    }
    answer(
        &desk,
        &mut vcx,
        ControlResponse::Agents {
            id: "agents".into(),
            agents: rows,
            splits: Vec::new(),
            qass_checks: None,
        },
    );
    vcx.run_until_parked();
    sent(&mut rx);
    let at = vcx
        .debug_bounds("board-tell-send")
        .expect("Tell qass")
        .center();
    vcx.simulate_click(at, gpui::Modifiers::none());
    let ups = sent(&mut rx);
    assert!(
        ups.iter()
            .any(|u| matches!(u, Up::Paste(9, b) if b == b"work on QOOP-73 next")),
        "{ups:?}"
    );
    assert!(
        ups.iter()
            .any(|u| matches!(u, Up::Input(9, b) if b == b"\r"))
    );
    assert_eq!(
        desk.read_with(&vcx, |d, cx| d.board.tell.read(cx).text.clone()),
        ""
    );
}

// Review round 1 of desktop round 5.

/// A card under the terminal never changes the agent's size: each terminal is told the height it
/// has without the card, so a menu card coming and going sends no new size (ADR-007). `split` is
/// the tab's tree of panes, none for a tab of one.
fn card_never_resizes(cx: &mut TestAppContext, split: Option<qoop_runtime::control::Pane>) {
    let (desk, mut vcx, mut rx) = open(cx);
    let tabs = split.clone().map_or(vec![1], |p| p.leaves());
    // The agent lists keep the tab's split.
    let with_split = |mut list: ControlResponse| {
        if let ControlResponse::Agents { splits, .. } = &mut list {
            *splits = split.clone().into_iter().collect();
        }
        list
    };
    desk.update(&mut vcx, |d, _| {
        d.model.splits = split.clone().into_iter().collect()
    });
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(1), window, cx)));
    vcx.run_until_parked();
    let first = sent(&mut rx);
    for t in &tabs {
        assert!(
            first
                .iter()
                .any(|u| matches!(u, Up::Resize(n, ..) if n == t)),
            "terminal {t} was sized when it came on"
        );
    }
    assert!(vcx.debug_bounds("question-card").is_none());
    // With no card each terminal is told what its pane draws: the rows of the area it paints in.
    let drawn: Vec<(u32, u16)> = desk.read_with(&vcx, |d, _| {
        d.term
            .iter()
            .chain(&d.others)
            .map(|t| {
                let area = t.area.get();
                (
                    t.terminal,
                    d.cells.fit((area.size.width, area.size.height)).1,
                )
            })
            .collect()
    });
    assert_eq!(drawn.len(), tabs.len());
    for (terminal, rows) in drawn {
        let told = first
            .iter()
            .rev()
            .find_map(|u| match u {
                Up::Resize(t, _, rows) if *t == terminal => Some(*rows),
                _ => None,
            })
            .expect("sized");
        assert_eq!(told, rows, "terminal {terminal} is told the rows it draws");
    }
    answer(&desk, &mut vcx, with_split(asking(&[(1, 77, 1, 100)])));
    vcx.run_until_parked();
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("question-card").is_some(),
        "the card is drawn"
    );
    assert!(
        !sent(&mut rx).iter().any(|u| matches!(u, Up::Resize(..))),
        "a card appearing sent a size"
    );
    answer(&desk, &mut vcx, with_split(answer_gone()));
    vcx.run_until_parked();
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("question-card").is_none());
    assert!(
        !sent(&mut rx).iter().any(|u| matches!(u, Up::Resize(..))),
        "a card going sent a size"
    );
}

#[gpui::test]
fn a_card_under_the_terminal_never_resizes_the_agent(cx: &mut TestAppContext) {
    card_never_resizes(cx, None);
}

/// The same for panes side by side and for panes one above the other.
#[gpui::test]
fn a_card_never_resizes_side_by_side_panes(cx: &mut TestAppContext) {
    card_never_resizes(cx, Some(crate::state::tests::split(1, 3)));
}

#[gpui::test]
fn a_card_never_resizes_stacked_panes(cx: &mut TestAppContext) {
    use qoop_runtime::control::{Pane, SplitDir};
    card_never_resizes(
        cx,
        Some(Pane::Split {
            dir: SplitDir::H,
            permille: 600,
            questions: 0,
            first: Box::new(Pane::Leaf { terminal: 1 }),
            second: Box::new(Pane::Leaf { terminal: 3 }),
        }),
    );
}

/// The agent list with nobody asking.
fn answer_gone() -> ControlResponse {
    ControlResponse::Agents {
        id: "agents".into(),
        agents: crate::state::tests::fixture().rows,
        splits: Vec::new(),
        qass_checks: None,
    }
}

/// Once Start has typed its line the shell it ran in stays reachable: a refused `qoop worktree
/// new` (the branch exists) shows its error there, so the sheet offers to open it.
#[gpui::test]
fn a_started_sheet_offers_the_shell_that_ran_the_line(cx: &mut TestAppContext) {
    use crate::state::tests::row;
    let (desk, mut vcx, mut rx) = open(cx);
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(5), window, cx)));
    vcx.run_until_parked();
    vcx.simulate_keystrokes("ctrl-a c");
    vcx.simulate_input("fix it");
    sent(&mut rx);
    vcx.simulate_keystrokes("enter");
    let id = desk
        .read_with(&vcx, |d, _| d.pending.clone().map(|p| p.0))
        .expect("asked");
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.apply(
                Down::Answer(Box::new(ControlResponse::TerminalCreated {
                    id,
                    terminal: 12,
                })),
                window,
                cx,
            )
        })
    });
    assert!(
        vcx.debug_bounds("open-shell").is_none(),
        "the shell is not listed yet"
    );
    let mut shell = row(12, 2, "idle", "zsh", "zsh");
    shell.agent = false;
    answer(&desk, &mut vcx, agents_with(vec![shell]));
    vcx.run_until_parked();
    let at = vcx
        .debug_bounds("open-shell")
        .expect("Open its terminal")
        .center();
    vcx.simulate_click(at, gpui::Modifiers::none());
    assert_eq!(
        desk.read_with(&vcx, |d, _| (d.page, d.sheet.is_none())),
        (Page::Session(12), true)
    );
}

/// An add the mux refuses (under the outer `Via` id) says why instead of leaving "Adding.", an
/// add that is written does not pull the person off the computer they moved to, and one nobody
/// answers is given up with a word.
#[gpui::test]
fn board_writes_are_answered_under_either_id_and_never_move_the_person(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    let remote = 1i64 << 40 | 1;
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Board, window, cx)));
    let click = |vcx: &mut VisualTestContext, what: &'static str| {
        let at = vcx
            .debug_bounds(what)
            .unwrap_or_else(|| panic!("{what}"))
            .center();
        vcx.simulate_click(at, gpui::Modifiers::none());
    };
    click(&mut vcx, "board-machine-1");
    answer(
        &desk,
        &mut vcx,
        board_answer("board-read-1", Some(serde_json::json!([]))),
    );
    vcx.run_until_parked();
    let add = |vcx: &mut VisualTestContext, rx: &mut UnboundedReceiver<Up>, words: &str| {
        click(vcx, "board-add");
        vcx.simulate_input(words);
        sent(rx);
        vcx.simulate_keystrokes("enter");
        sent(rx)
            .into_iter()
            .find_map(|u| match u {
                Up::Request(r) => match *r {
                    ControlRequest::Via { id, request, .. } => {
                        Some((id, request.request_id().to_string()))
                    }
                    _ => None,
                },
                _ => None,
            })
            .expect("carried by Via")
    };
    let said = |desk: &Entity<Desk>, vcx: &VisualTestContext| {
        desk.read_with(vcx, |d, _| d.board.said.clone())
    };

    // The mux refuses under the outer id.
    let (outer, _inner) = add(&mut vcx, &mut rx, "one");
    assert_eq!(said(&desk, &vcx).as_deref(), Some("Adding."));
    answer(
        &desk,
        &mut vcx,
        ControlResponse::Failed {
            id: outer,
            reason: "staging went offline".into(),
        },
    );
    assert_eq!(said(&desk, &vcx).as_deref(), Some("staging went offline"));

    // Written while the person has gone to This Mac: they stay there.
    let (_outer, inner) = add(&mut vcx, &mut rx, "two");
    click(&mut vcx, "board-machine-0");
    answer(
        &desk,
        &mut vcx,
        serde_json::from_value(serde_json::json!({
            "op": "TaskWritten", "id": inner, "task": 9, "key": "QOOP-9",
        }))
        .expect("written"),
    );
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.board.machine),
        0,
        "still on This Mac"
    );
    assert_eq!(said(&desk, &vcx).as_deref(), Some("Added QOOP-9."));
    let reads = via_sent(&mut rx, remote);
    assert!(
        reads
            .iter()
            .any(|r| matches!(r, ControlRequest::TaskRead { board: true, .. })),
        "the computer written to is read again"
    );

    // Nobody answers: given up after a while, with a word.
    click(&mut vcx, "board-machine-1");
    let _ = add(&mut vcx, &mut rx, "three");
    desk.update(&mut vcx, |d, _| {
        let at = crate::app::unix_now() + 60;
        d.board_tick(at);
    });
    assert_eq!(
        said(&desk, &vcx).as_deref(),
        Some("The item was not confirmed: look at the board.")
    );
}

/// An item that waits on the person leads somewhere: its agent's page (the terminal as the mux
/// numbers it), else the Inbox on the card about it; by click, and by Tab and Enter.
#[gpui::test]
fn a_waiting_item_opens_its_agent_or_its_card(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    desk.update(&mut vcx, |d, cx| {
        d.asks = vec![{
            let mut card = tab_card("Q7", 1, "open");
            card.item = serde_json::from_value(serde_json::json!({"id": 2, "key": "QOOP-68"})).ok();
            card
        }];
        cx.notify();
    });
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Board, window, cx)));
    sent(&mut rx);
    let items = serde_json::json!([
        {"id": 2, "key": "QOOP-68", "workspace": 1, "title": "Rust toolchain bump", "state": "doing",
         "steps_done": 0, "steps_total": 0, "agents": [], "updated_at": 2, "open_asks": 1},
        {"id": 3, "key": "QOOP-74", "workspace": 1, "title": "Linear token refresh", "state": "doing",
         "steps_done": 0, "steps_total": 0, "updated_at": 3,
         "agents": [{"task": 1, "terminal": 2, "name": "a", "machine": "this Mac", "stage": "x", "status": "waits"}]},
    ]);
    answer(&desk, &mut vcx, board_answer("board-read", Some(items)));
    vcx.run_until_parked();
    // By click: the agent.
    let at = vcx.debug_bounds("item-QOOP-74").expect("item").center();
    vcx.simulate_click(at, gpui::Modifiers::none());
    assert_eq!(desk.read_with(&vcx, |d, _| d.page), Page::Session(2));
    // The one with a question open: the Inbox, the ring on its card.
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Board, window, cx)));
    vcx.run_until_parked();
    let at = vcx.debug_bounds("item-QOOP-68").expect("item").center();
    vcx.simulate_click(at, gpui::Modifiers::none());
    assert_eq!(
        desk.read_with(&vcx, |d, _| (d.page, d.inbox_cursor.clone())),
        (Page::Inbox, Some(crate::state::Slot::Card("Q7".into())))
    );
    // By keyboard: the item is a Tab stop, and Enter opens it.
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.open(Page::Board, window, cx);
            window.focus(&d.focus, cx);
        })
    });
    vcx.run_until_parked();
    let handle = desk
        .read_with(&vcx, |d, _| {
            d.board.item_focus.borrow().get(&(0, 3)).cloned()
        })
        .expect("the item keeps a focus handle");
    let mut reached = false;
    for _ in 0..200 {
        vcx.simulate_keystrokes("tab");
        if vcx.update(|window, _| handle.is_focused(window)) {
            reached = true;
            break;
        }
    }
    assert!(reached, "Tab reaches the item");
    vcx.simulate_keystrokes("enter");
    vcx.update(|window, cx| {
        window.dispatch_event(
            gpui::PlatformInput::KeyUp(gpui::KeyUpEvent {
                keystroke: gpui::Keystroke::parse("enter").expect("enter"),
            }),
            cx,
        );
    });
    vcx.run_until_parked();
    assert_eq!(desk.read_with(&vcx, |d, _| d.page), Page::Session(2));
}

// qass's place is its own agent page.

/// The agent list as the daemon sends it.
fn listed(rows: Vec<qoop_runtime::control::AgentRow>) -> ControlResponse {
    ControlResponse::Agents {
        id: "agents".into(),
        agents: rows,
        splits: Vec::new(),
        qass_checks: None,
    }
}

/// The sidebar's qass opens qass's own agent page while it runs: its terminal, as any agent's,
/// with no ledger, no ask bar and no chat; the qass link is the current one there.
#[gpui::test]
fn the_qass_place_opens_qass_agent_page(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Qass, window, cx)));
    vcx.run_until_parked();
    assert_eq!(
        desk.read_with(&vcx, |d, _| (d.page, d.term.as_ref().map(|t| t.terminal))),
        (Page::Session(9), Some(9))
    );
    for gone in ["qass-page", "qass-columns", "ask-bar", "qass-state"] {
        assert!(vcx.debug_bounds(gone).is_none(), "{gone} is not drawn");
    }
    assert!(desk.read_with(&vcx, |d, _| d.nav_on(Page::Qass) && !d.nav_on(Page::Inbox)));
    // Another agent's page is not qass's.
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(1), window, cx)));
    assert!(desk.read_with(&vcx, |d, _| !d.nav_on(Page::Qass)));
}

/// qass's place follows qass: its agent gone, Start it; its space asleep, Wake it; back, its
/// agent page again.
#[gpui::test]
fn the_qass_place_follows_qass(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Qass, window, cx)));
    vcx.run_until_parked();
    assert_eq!(desk.read_with(&vcx, |d, _| d.page), Page::Session(9));
    let mut rows = crate::state::tests::fixture().rows;
    rows.iter_mut().find(|r| r.terminal == 9).unwrap().agent = false;
    answer(&desk, &mut vcx, listed(rows.clone()));
    assert_eq!(
        desk.read_with(&vcx, |d, _| (d.page, d.term.is_none())),
        (Page::Qass, true),
        "no dead terminal"
    );
    assert!(vcx.debug_bounds("start-qass").is_some(), "Start it");
    assert!(vcx.debug_bounds("wake-qass").is_none());
    assert!(desk.read_with(&vcx, |d, _| d.nav_on(Page::Qass)));
    vcx.update(|_, cx| {
        desk.update(cx, |d, cx| {
            d.model
                .spaces
                .iter_mut()
                .find(|w| w.id == 3)
                .unwrap()
                .asleep = true;
            cx.notify();
        })
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("wake-qass").is_some(), "Wake it");
    assert!(vcx.debug_bounds("start-qass").is_none());
    answer(&desk, &mut vcx, listed(crate::state::tests::fixture().rows));
    assert_eq!(
        desk.read_with(&vcx, |d, _| (d.page, d.term.as_ref().map(|t| t.terminal))),
        (Page::Session(9), Some(9))
    );
}

/// A page left on a terminal of qass's space where qass no longer runs goes to Start it on the
/// next list, though this window never saw qass run there.
#[gpui::test]
fn a_page_on_qass_space_without_qass_shows_start(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    let mut rows = crate::state::tests::fixture().rows;
    rows.iter_mut().find(|r| r.terminal == 9).unwrap().agent = false;
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.model.rows = rows.clone();
            d.open(Page::Session(9), window, cx);
        })
    });
    answer(&desk, &mut vcx, listed(rows));
    assert_eq!(desk.read_with(&vcx, |d, _| d.page), Page::Qass);
    assert!(vcx.debug_bounds("start-qass").is_some());
}

/// A focus on qass's terminal (a notification, `qoop focus`) opens its agent page.
#[gpui::test]
fn a_focus_on_qass_opens_its_agent_page(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.apply(
                Down::Event(qoop_runtime::control::EventMessage::Focus {
                    terminal: 9,
                    zoom: None,
                }),
                window,
                cx,
            );
        })
    });
    assert_eq!(desk.read_with(&vcx, |d, _| d.page), Page::Session(9));
}

/// qass's own question card never offers Hand to qass; another agent's does.
#[gpui::test]
fn qass_own_card_never_offers_hand_to_qass(cx: &mut TestAppContext) {
    use crate::state::Pick;
    let (desk, mut vcx, _rx) = open(cx);
    let mut rows = crate::state::tests::fixture().rows;
    for r in rows.iter_mut().filter(|r| matches!(r.terminal, 2 | 9)) {
        r.state = "blocked".into();
        r.status = Some("blocked".into());
        r.menu = Some(crate::state::tests::edit_menu(77));
    }
    answer(&desk, &mut vcx, listed(rows));
    let hands = |vcx: &mut VisualTestContext, terminal: u32| {
        vcx.update(|window, cx| {
            desk.update(cx, |d, cx| d.open(Page::Session(terminal), window, cx))
        });
        vcx.run_until_parked();
        desk.read_with(vcx, |d, _| {
            let (_, offers, _, _) = d.card_offers().expect("a card");
            offers.iter().filter(|o| o.pick == Pick::Qass).count()
        })
    };
    assert_eq!(hands(&mut vcx, 2), 1, "another agent's card");
    assert_eq!(hands(&mut vcx, 9), 0, "qass's own card");
}

/// Settings holds what qass watches: All asks qass to stop watching, a space's and a tab's
/// switch tell qass to leave them alone; its Schedules link opens Schedules.
#[gpui::test]
fn settings_hold_what_qass_watches_and_schedules(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    // Tall enough that the card is on screen, where a click can reach it.
    vcx.simulate_resize(gpui::size(gpui::px(1280.), gpui::px(2600.)));
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Settings, window, cx)));
    vcx.run_until_parked();
    assert!(
        request_of(&sent(&mut rx), |r| matches!(
            r,
            ControlRequest::QassWatch { on: None, .. }
        ))
        .is_some(),
        "the switch is read as Settings opens"
    );
    let click = |vcx: &mut VisualTestContext, what: &'static str| {
        let at = vcx
            .debug_bounds(what)
            .unwrap_or_else(|| panic!("{what} is not drawn"))
            .center();
        vcx.simulate_click(at, gpui::Modifiers::none());
    };
    click(&mut vcx, "watch-all-0");
    let asked = request_of(&sent(&mut rx), |r| {
        matches!(r, ControlRequest::QassWatch { .. })
    });
    assert!(
        matches!(
            asked,
            Some(ControlRequest::QassWatch {
                on: Some(false),
                ..
            })
        ),
        "{asked:?}"
    );
    // Watching back on, then one tab and one space left alone.
    click(&mut vcx, "watch-all-0");
    sent(&mut rx);
    click(&mut vcx, "watch-tab-1");
    let asked = request_of(&sent(&mut rx), |r| {
        matches!(r, ControlRequest::SetQassSkip { .. })
    });
    assert!(
        matches!(
            asked,
            Some(ControlRequest::SetQassSkip {
                workspace: 1,
                terminal: Some(1),
                on: true,
                ..
            })
        ),
        "{asked:?}"
    );
    click(&mut vcx, "watch-space-2");
    let asked = request_of(&sent(&mut rx), |r| {
        matches!(r, ControlRequest::SetQassSkip { .. })
    });
    assert!(
        matches!(
            asked,
            Some(ControlRequest::SetQassSkip {
                workspace: 2,
                terminal: None,
                on: true,
                ..
            })
        ),
        "{asked:?}"
    );
    click(&mut vcx, "nav-schedules");
    assert_eq!(desk.read_with(&vcx, |d, _| d.page), Page::Schedules);
}

/// A permission question as Claude Code draws it: the command over `lines` rows of context,
/// each `wide` characters or so (the daemon keeps at most 12 rows of 200).
fn bash_menu(seal: u64, lines: usize, wide: usize) -> qoop_runtime::control::AgentMenu {
    let context: Vec<String> = (0..lines)
        .map(|i| format!("Bash(git push origin main) step {i} {}", "x".repeat(wide)))
        .collect();
    serde_json::from_value(serde_json::json!({
        "question": "Do you want to proceed?",
        "seal": seal,
        "context": context,
        "options": [
            {"key": "1", "label": "Yes"},
            {"key": "2", "label": "Yes, and don't ask again for this command"},
            {"key": "3", "label": "No, and tell Claude what to do differently (esc)", "tell": true},
        ],
    }))
    .expect("menu")
}

/// Agents 2 and 3 ask `menus` (agent 3 is blocked for it); the others as the fixture has them.
fn ask(
    desk: &Entity<Desk>,
    vcx: &mut VisualTestContext,
    menus: [Option<qoop_runtime::control::AgentMenu>; 2],
) {
    let mut rows = crate::state::tests::fixture().rows;
    let [two, three] = menus;
    rows[1].menu = two;
    if three.is_some() {
        rows[2] = crate::state::tests::row(3, 1, "blocked", "claude", "Coupon tests");
        rows[2].menu = three;
    }
    answer(
        desk,
        vcx,
        ControlResponse::Agents {
            id: "agents".into(),
            agents: rows,
            splits: Vec::new(),
            qass_checks: None,
        },
    );
}

/// `inner` lies whole inside `outer`, top to bottom.
fn whole_in(outer: gpui::Bounds<gpui::Pixels>, inner: gpui::Bounds<gpui::Pixels>) -> bool {
    inner.top() >= outer.top() && inner.bottom() <= outer.bottom()
}

/// The card's last answer on an agent's page: the highest digit drawn.
fn last_option(vcx: &mut VisualTestContext) -> gpui::Bounds<gpui::Pixels> {
    (1..=9)
        .rev()
        .find_map(|d| vcx.debug_bounds(Box::leak(format!("card-2-{d}").into_boxed_str())))
        .expect("an option with a digit")
}

/// A pick on the agent page's card: its hold line has its own room. A card that fit whole
/// still fits whole with the line (the room grows by it); a card taller than its room scrolls
/// to the line, so the last answer and the line are both in sight. After `u` the line says
/// "Undone, nothing was sent" for a few seconds, and nothing goes to the agent.
#[gpui::test]
fn the_hold_line_takes_its_own_room_on_the_agent_page(cx: &mut TestAppContext) {
    for (w, h) in [(1280., 800.), (1100., 800.)] {
        let (desk, mut vcx, mut rx) = open(cx);
        vcx.simulate_resize(gpui::size(gpui::px(w), gpui::px(h)));
        vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(2), window, cx)));
        // The most context that leaves the card whole in its room, and the most there is: 12
        // rows that wrap, taller than the room before any pick.
        let fits = (0..=12)
            .rev()
            .find(|&n| {
                ask(
                    &desk,
                    &mut vcx,
                    [Some(bash_menu(100 + n as u64, n, 0)), None],
                );
                let below = vcx.debug_bounds("agent-below").expect("the room");
                whole_in(below, vcx.debug_bounds("question-card").expect("the card"))
            })
            .expect("a card that fits");
        ask(&desk, &mut vcx, [Some(bash_menu(150, 12, 170)), None]);
        assert!(
            !whole_in(
                vcx.debug_bounds("agent-below").expect("the room"),
                vcx.debug_bounds("question-card").expect("the card")
            ),
            "the tall card is taller than its room at {w}x{h}"
        );
        for (n, wide) in [(fits, 0), (12, 170)] {
            ask(
                &desk,
                &mut vcx,
                [Some(bash_menu(200 + n as u64, n, wide)), None],
            );
            std::thread::sleep(qoop_tui::answers::ARMING);
            vcx.simulate_keystrokes("1");
            assert!(
                desk.read_with(&vcx, |d, _| d
                    .holds
                    .of(&crate::state::Slot::Agent(2))
                    .is_some()),
                "held"
            );
            vcx.run_until_parked();
            let below = vcx.debug_bounds("agent-below").expect("the room");
            let line = vcx.debug_bounds("held-line").expect("the hold line");
            let window = gpui::Bounds::new(
                gpui::point(gpui::px(0.), gpui::px(0.)),
                gpui::size(gpui::px(w), gpui::px(h)),
            );
            assert!(
                whole_in(below, line) && whole_in(window, below),
                "the hold line in sight at {w}x{h}, {n} rows: {line:?} in {below:?}"
            );
            assert!(
                whole_in(below, last_option(&mut vcx)),
                "the last answer in sight at {w}x{h}, {n} rows"
            );
            if wide == 0 {
                assert!(
                    whole_in(below, vcx.debug_bounds("question-card").expect("the card")),
                    "a card that fit whole still does with its line, at {w}x{h}"
                );
            }
            sent(&mut rx);
            vcx.simulate_keystrokes("u");
            assert!(
                desk.read_with(&vcx, |d, _| d.holds.held.is_empty()),
                "undone"
            );
            assert!(
                request_of(&sent(&mut rx), |r| matches!(
                    r,
                    ControlRequest::AnswerMenu { .. }
                ))
                .is_none(),
                "nothing sent"
            );
            assert!(
                desk.read_with(&vcx, |d, _| matches!(
                    d.said.get(&crate::state::Slot::Agent(2)),
                    Some(crate::question::Said::Undone(_))
                )),
                "said as undone"
            );
            let word = vcx.debug_bounds("said-line").expect("the undone word");
            assert!(
                whole_in(vcx.debug_bounds("agent-below").expect("the room"), word),
                "where the hold line was"
            );
            vcx.executor().advance_clock(crate::question::UNDONE_FOR);
            vcx.run_until_parked();
            assert!(
                vcx.debug_bounds("said-line").is_none(),
                "gone after a few seconds"
            );
        }
    }
}

/// A pick on the Inbox: the hold bar takes the bottom of the page, and the card with the ring
/// is scrolled back above it, its last answer and the bar both whole in the window.
#[gpui::test]
fn the_hold_bar_never_covers_the_card_it_holds(cx: &mut TestAppContext) {
    for (w, h) in [(1280., 800.), (1100., 800.)] {
        let (desk, mut vcx, _rx) = open(cx);
        vcx.simulate_resize(gpui::size(gpui::px(w), gpui::px(h)));
        ask(
            &desk,
            &mut vcx,
            [Some(bash_menu(300, 3, 0)), Some(bash_menu(301, 3, 0))],
        );
        vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Inbox, window, cx)));
        vcx.run_until_parked();
        // The ring to the second card, by the keyboard: the list scrolls it into view, its bottom
        // at the list's bottom edge.
        let ringed = desk.read_with(&vcx, |d, _| d.inbox_cursor.clone());
        vcx.simulate_keystrokes("cmd-j");
        vcx.run_until_parked();
        let ring = desk
            .read_with(&vcx, |d, _| d.inbox_cursor.clone())
            .expect("a ring");
        assert_ne!(Some(ring.clone()), ringed, "the ring moved");
        let name = match &ring {
            crate::state::Slot::Agent(t) => format!("inbox-card-t{t}"),
            crate::state::Slot::Card(c) => format!("inbox-card-{c}"),
        };
        let name: &'static str = Box::leak(name.into_boxed_str());
        std::thread::sleep(qoop_tui::answers::ARMING);
        vcx.simulate_keystrokes("1");
        assert!(
            desk.read_with(&vcx, |d, _| d.holds.of(&ring).is_some()),
            "held"
        );
        vcx.run_until_parked();
        let body = vcx.debug_bounds("inbox-body").expect("the list");
        let card = vcx.debug_bounds(name).expect("the card");
        let bar = vcx.debug_bounds("hold-bar").expect("the hold bar");
        assert!(
            whole_in(body, card),
            "the held card whole above the bar at {w}x{h}: {card:?} in {body:?}"
        );
        assert!(
            bar.top() >= body.bottom() && bar.bottom() <= gpui::px(h),
            "the bar in its own room at {w}x{h}: {bar:?}"
        );
    }
}

/// Which states' marks move: a turn going, and work running in the background; nothing else.
#[test]
fn only_working_and_background_marks_move() {
    use crate::state::Kind;
    use qoop_domain::status::Status;
    for status in [Status::Working, Status::Background] {
        assert!(crate::shell::moves(Kind::Status(status)), "{status:?}");
    }
    for status in [
        Status::Starting,
        Status::Blocked,
        Status::Failed,
        Status::Done,
        Status::Parked,
        Status::Idle,
        Status::Unknown,
    ] {
        assert!(!crate::shell::moves(Kind::Status(status)), "{status:?}");
    }
    assert!(!crate::shell::moves(Kind::Sleeping));
}

/// The working mark's clock: with an agent working the window steps a frame every 160 ms and
/// draws again; once nothing works the clock stops by itself, and a window with nothing
/// working never starts one.
#[gpui::test]
fn the_marks_clock_runs_only_while_a_mark_moves(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    let frame = |vcx: &VisualTestContext| desk.read_with(vcx, |d, _| (d.beat.frame, d.animating()));
    assert!(frame(&vcx).1, "the fixture has agents that work");
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.keep_beating(window, cx)));
    vcx.executor().advance_clock(crate::shell::FRAME * 3);
    vcx.run_until_parked();
    assert_eq!(frame(&vcx).0, 3, "a frame each 160 ms");
    // Nothing works any more: the next frame stops the clock, and it is not started again.
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            for r in &mut d.model.rows {
                r.state = "idle".into();
                r.status = Some("idle".into());
            }
            cx.notify();
            assert!(!d.animating());
        });
        desk.update(cx, |d, cx| d.keep_beating(window, cx));
    });
    vcx.executor().advance_clock(crate::shell::FRAME * 3);
    vcx.run_until_parked();
    assert_eq!(frame(&vcx).0, 3, "still: no frame after nothing moves");
    // The clock stopped itself, so a later second starts it only when a mark moves again.
    desk.update(&mut vcx, |d, _| assert!(!d.beat.is_running()));
}

// The desktop redesign, P1: the top band, the corner and the column's head.

fn click_on(vcx: &mut VisualTestContext, what: &'static str) {
    let at = vcx
        .debug_bounds(what)
        .unwrap_or_else(|| panic!("{what} is not drawn"))
        .center();
    vcx.simulate_click(at, gpui::Modifiers::none());
    vcx.run_until_parked();
}

/// The agents as the fixture has them, terminal 1 under `name`, and terminal 2 waiting on the
/// person or not.
fn listed_with(name: &str, waiting: bool) -> ControlResponse {
    let mut rows = crate::state::tests::fixture().rows;
    for r in &mut rows {
        if r.terminal == 1 {
            r.name = Some(name.into());
        }
        if r.terminal == 2 && !waiting {
            r.state = "working".into();
            r.status = Some("working".into());
        }
    }
    ControlResponse::Agents {
        id: "agents".into(),
        agents: rows,
        splits: Vec::new(),
        qass_checks: None,
    }
}

/// The column's top, in order: the corner's fold toggle, qass, New agent, Inbox, Board and
/// Schedules. No wordmark and no gear in the column: Settings is the band's.
#[gpui::test]
fn the_column_head_is_qass_new_agent_inbox_board_schedules(cx: &mut TestAppContext) {
    let (_desk, mut vcx, _rx) = open(cx);
    let order = [
        "side-toggle",
        "nav-qass",
        "new-work",
        "nav-inbox",
        "nav-board",
        "nav-schedules",
    ];
    let side = vcx.debug_bounds("sidebar").expect("the column");
    let at: Vec<_> = order
        .iter()
        .map(|s| {
            vcx.debug_bounds(s)
                .unwrap_or_else(|| panic!("{s} is not drawn"))
                .origin
        })
        .collect();
    assert!(
        at.windows(2).all(|w| w[0].y < w[1].y),
        "{order:?} at {at:?}"
    );
    assert!(at.iter().all(|p| side.contains(p)), "all in the column");
    assert!(vcx.debug_bounds("nav-settings").is_none(), "no gear link");
    let gear = vcx.debug_bounds("band-settings").expect("the band's gear");
    assert!(
        !side.contains(&gear.origin),
        "the gear is not in the column"
    );
}

/// The Board and Schedules are pages of their own, opened from the column. The Inbox has no
/// Board view any more, Escape on the Board goes back, and the Settings page no longer links
/// to Schedules (the column does).
#[gpui::test]
fn board_and_schedules_are_their_own_pages(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    assert!(vcx.debug_bounds("view-board").is_none(), "no Board view");
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(1), window, cx)));
    vcx.run_until_parked();
    click_on(&mut vcx, "nav-board");
    assert_eq!(desk.read_with(&vcx, |d, _| d.page), Page::Board);
    assert!(vcx.debug_bounds("board-page").is_some());
    vcx.simulate_keystrokes("escape");
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.page),
        Page::Session(1),
        "Escape goes back"
    );
    click_on(&mut vcx, "nav-schedules");
    assert_eq!(desk.read_with(&vcx, |d, _| d.page), Page::Schedules);
    click_on(&mut vcx, "band-settings");
    assert_eq!(desk.read_with(&vcx, |d, _| d.page), Page::Settings);
    let side = vcx.debug_bounds("sidebar").expect("the column");
    let link = vcx.debug_bounds("nav-schedules").expect("the column's");
    assert!(side.contains(&link.origin), "the one link is the column's");
}

/// Agent page, Board, Inbox, then Escape twice: back on the agent page. The Board and the
/// Inbox share one way back, so Escape never bounces between them; the second Escape is the
/// agent's. The agent's terminal stays alive behind both: never hidden and shown again.
#[gpui::test]
fn escape_from_board_then_inbox_returns_to_the_agent(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(1), window, cx)));
    vcx.run_until_parked();
    sent(&mut rx);
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.open(Page::Board, window, cx);
            d.open(Page::Inbox, window, cx);
        })
    });
    vcx.run_until_parked();
    vcx.simulate_keystrokes("escape");
    assert_eq!(desk.read_with(&vcx, |d, _| d.page), Page::Session(1));
    vcx.simulate_keystrokes("escape");
    assert_eq!(desk.read_with(&vcx, |d, _| d.page), Page::Session(1));
    let ups = sent(&mut rx);
    assert!(
        ups.iter()
            .any(|u| matches!(u, Up::Input(1, b) if b == b"\x1b")),
        "the second Escape is the agent's"
    );
    assert!(
        !ups.iter().any(|u| matches!(u, Up::Hide(_) | Up::Show(_))),
        "the terminal was never attached again"
    );
}

/// With a title that wraps, other agents' questions arriving and leaving (the chip from a
/// count to "Nothing else waits" and to "99+") never change the band's height or the width
/// of its right end, and never send the agent a new size.
#[gpui::test]
fn the_waiting_count_never_resizes_the_agent(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    let long = "Fix checkout tax rounding for EU orders and keep every word of this title on \
        screen whatever the width of the window is";
    vcx.simulate_resize(gpui::size(gpui::px(1024.), gpui::px(800.)));
    desk.update(&mut vcx, |d, cx| {
        d.side_w = crate::shell::SIDE_MAX;
        cx.notify();
    });
    answer(&desk, &mut vcx, listed_with(long, true));
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(1), window, cx)));
    vcx.run_until_parked();
    vcx.run_until_parked();
    sent(&mut rx);
    let measure = |vcx: &mut VisualTestContext| {
        (
            vcx.debug_bounds("top-band").expect("the band").size.height,
            vcx.debug_bounds("band-right")
                .expect("its right end")
                .size
                .width,
        )
    };
    let one = measure(&mut vcx);
    assert!(one.0 > gpui::px(crate::shell::TOP_BAND), "the title wraps");
    assert!(
        vcx.debug_bounds("next-waiting-count").is_some(),
        "one waits"
    );
    // Nothing else waits.
    answer(&desk, &mut vcx, listed_with(long, false));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("next-waiting-count").is_none());
    assert_eq!(measure(&mut vcx), one, "nothing else waits");
    // A hundred cards about another agent: "99+".
    desk.update(&mut vcx, |d, cx| {
        d.asks = (0..100)
            .map(|n| tab_card(&format!("Q{n}"), 3, "open"))
            .collect();
        cx.notify();
    });
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("next-waiting-count").is_some());
    assert_eq!(measure(&mut vcx), one, "a hundred wait");
    assert!(
        !sent(&mut rx).iter().any(|u| matches!(u, Up::Resize(..))),
        "the count sent the agent a size"
    );
}

/// Next waiting, the gear and the pet are at the band's right end on every page, the pet
/// drawn with none picked (the picker's first).
#[gpui::test]
fn the_header_right_end_is_on_every_page(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    assert!(
        desk.read_with(&vcx, |d, _| d.my_pet.is_none()),
        "no pet picked"
    );
    for page in [
        Page::Inbox,
        Page::Board,
        Page::Schedules,
        Page::Settings,
        Page::Computer(0),
        Page::Qass,
        Page::Session(1),
    ] {
        vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(page, window, cx)));
        vcx.run_until_parked();
        for part in ["next-waiting", "band-settings", "band-pet"] {
            assert!(vcx.debug_bounds(part).is_some(), "{part} on {page:?}");
        }
    }
    // On the agent that waits nothing else does: the chip is drawn without a count.
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(2), window, cx)));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("next-waiting").is_some());
    assert!(vcx.debug_bounds("next-waiting-count").is_none());
}

/// The toggle folds the column: it and its divider go, the toggle stays in the band, the
/// column's width is given to the page (the Board has four columns at 1024), and the toggle
/// brings it back.
#[gpui::test]
fn the_column_folds_and_the_toggle_stays(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    vcx.simulate_resize(gpui::size(gpui::px(1024.), gpui::px(800.)));
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Board, window, cx)));
    let read = request_of(&sent(&mut rx), |r| {
        matches!(r, ControlRequest::TaskRead { board: true, .. })
    })
    .expect("the board is read");
    answer(
        &desk,
        &mut vcx,
        board_answer(read.request_id(), Some(board_items())),
    );
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("board-columns-2").is_some(),
        "the column takes 280"
    );
    click_on(&mut vcx, "side-toggle");
    assert!(vcx.debug_bounds("sidebar").is_none(), "folded");
    assert!(
        vcx.debug_bounds("divider-side").is_none(),
        "its divider too"
    );
    assert!(
        vcx.debug_bounds("side-toggle").is_some(),
        "the toggle stays"
    );
    assert_eq!(desk.read_with(&vcx, |d, _| d.side_px()), 0.);
    assert!(
        vcx.debug_bounds("board-columns-4").is_some(),
        "four columns at 1024"
    );
    click_on(&mut vcx, "side-toggle");
    assert!(vcx.debug_bounds("sidebar").is_some(), "back");
}

/// The Board entry's count is This Mac's board, read on connecting and again on every board
/// change, whatever the page: here the Inbox shows.
#[gpui::test]
fn the_board_count_is_read_whatever_the_page(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    let note = |desk: &Entity<Desk>, vcx: &VisualTestContext| {
        desk.read_with(vcx, |d, _| d.board_note(&d.inbox()))
    };
    assert_eq!(note(&desk, &vcx), None, "no count before the first read");
    let read = request_of(&sent(&mut rx), |r| {
        matches!(r, ControlRequest::TaskRead { board: true, .. })
    })
    .expect("read on connecting");
    assert_eq!(desk.read_with(&vcx, |d, _| d.page), Page::Inbox);
    answer(
        &desk,
        &mut vcx,
        board_answer(read.request_id(), Some(board_items())),
    );
    assert_eq!(note(&desk, &vcx).as_deref(), Some("1 to do"));
    // The board changes while the Inbox shows: read again, and the count follows.
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.apply(
                Down::Event(qoop_runtime::control::EventMessage::TasksChanged),
                window,
                cx,
            )
        })
    });
    let again = request_of(&sent(&mut rx), |r| {
        matches!(r, ControlRequest::TaskRead { board: true, .. })
    })
    .expect("read again on a board change");
    let todo = |id: i64| {
        serde_json::json!({
            "id": id, "key": format!("QOOP-{id}"), "workspace": 1, "title": "t",
            "state": "todo", "steps_done": 0, "steps_total": 0, "agents": [], "updated_at": id,
        })
    };
    answer(
        &desk,
        &mut vcx,
        board_answer(
            again.request_id(),
            Some(serde_json::json!([todo(1), todo(2), todo(3)])),
        ),
    );
    assert_eq!(note(&desk, &vcx).as_deref(), Some("3 to do"));
}

/// A title that wraps grows the band downward; its first line stays where it was, on the
/// window buttons' centre. The short title on a wide window is one line; the long one at the
/// smallest window with the column at its widest wraps.
#[gpui::test]
fn a_long_title_wraps_and_the_band_grows(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    vcx.simulate_resize(gpui::size(gpui::px(1600.), gpui::px(900.)));
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(1), window, cx)));
    vcx.run_until_parked();
    let band = vcx.debug_bounds("top-band").expect("the band");
    let first = vcx
        .debug_bounds("band-lead")
        .expect("its first line")
        .center()
        .y;
    assert_eq!(band.size.height, gpui::px(crate::shell::TOP_BAND));
    vcx.simulate_resize(gpui::size(gpui::px(1024.), gpui::px(800.)));
    desk.update(&mut vcx, |d, cx| {
        d.side_w = crate::shell::SIDE_MAX;
        cx.notify();
    });
    answer(
        &desk,
        &mut vcx,
        listed_with(
            "Fix checkout tax rounding for EU orders so that the totals match the invoices \
             sent to every customer in the union",
            true,
        ),
    );
    vcx.run_until_parked();
    let grown = vcx.debug_bounds("top-band").expect("the band");
    assert!(grown.size.height > band.size.height, "{grown:?}");
    let lead = vcx.debug_bounds("band-lead").expect("the mark's line");
    assert_eq!(lead.center().y, first, "the first line never moves");
    // The title's own text starts on that line, beside the mark, and wraps under itself: the
    // band grows by the title's extra lines only (its first line is the mark's).
    let title = vcx.debug_bounds("band-title").expect("the title");
    assert_eq!(
        title.top(),
        lead.top(),
        "the title starts on the first line"
    );
    assert!(title.size.height > lead.size.height, "and wraps: {title:?}");
    assert_eq!(
        grown.size.height,
        band.size.height + (title.size.height - lead.size.height),
        "the band grew by the title's extra lines"
    );
}

// Review round 1 of P1.

/// Another computer's board shown on the Board page is read again on its own 30 s clock: This
/// Mac's reads on every board change (for the column's count) never reset it.
#[gpui::test]
fn a_remote_board_is_read_again_while_this_macs_changes(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    let remote = 1i64 << 40 | 1;
    answer(
        &desk,
        &mut vcx,
        board_answer("board-read", Some(serde_json::json!([]))),
    );
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Board, window, cx)));
    vcx.run_until_parked();
    click_on(&mut vcx, "board-machine-1");
    answer(
        &desk,
        &mut vcx,
        board_answer("board-read-1", Some(serde_json::json!([]))),
    );
    answer(
        &desk,
        &mut vcx,
        board_answer("board-read", Some(serde_json::json!([]))),
    );
    // staging was read 31 s ago; This Mac's board changes now.
    desk.update(&mut vcx, |d, _| {
        d.board.read_at.insert(1, crate::app::unix_now() - 31);
    });
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.apply(
                Down::Event(qoop_runtime::control::EventMessage::TasksChanged),
                window,
                cx,
            )
        })
    });
    assert!(
        request_of(&sent(&mut rx), |r| matches!(
            r,
            ControlRequest::TaskRead { board: true, .. }
        ))
        .is_some(),
        "This Mac's board is read for the count"
    );
    desk.update(&mut vcx, |d, _| {
        d.board_tick(crate::app::unix_now());
    });
    assert!(
        via_sent(&mut rx, remote)
            .iter()
            .any(|r| matches!(r, ControlRequest::TaskRead { board: true, .. })),
        "staging is read again on its own clock"
    );
}

/// The Board entry says no count while qoop is not running on this Mac: the one it had is
/// as of before.
#[gpui::test]
fn the_board_count_goes_while_qoop_is_not_running(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    answer(
        &desk,
        &mut vcx,
        board_answer("board-read", Some(board_items())),
    );
    let note = |desk: &Entity<Desk>, vcx: &VisualTestContext| {
        desk.read_with(vcx, |d, _| d.board_note(&d.inbox()))
    };
    assert_eq!(note(&desk, &vcx).as_deref(), Some("1 to do"));
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.apply(Down::Disconnected("gone".into()), window, cx)
        })
    });
    assert_eq!(note(&desk, &vcx), None, "not running: no count");
}

/// With Reduce Motion on, the window's marks stand still and their clock never starts, though
/// agents work; off again, they move.
#[gpui::test]
fn reduce_motion_stills_the_marks_and_their_clock(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    let working = crate::state::Kind::Status(qoop_domain::status::Status::Working);
    assert!(desk.read_with(&vcx, |d, _| d.animating()), "agents work");
    crate::ui::set_reduce_motion(true);
    crate::ui::set_beat(5);
    assert!(!desk.read_with(&vcx, |d, _| d.animating()), "nothing moves");
    assert_eq!(
        crate::ui::mark_dots_now(working),
        crate::state::mark_dots(working, None),
        "the still picture"
    );
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.keep_beating(window, cx)));
    vcx.executor().advance_clock(crate::shell::FRAME * 3);
    vcx.run_until_parked();
    assert!(
        desk.read_with(&vcx, |d, _| !d.beat.is_running()),
        "no clock"
    );
    crate::ui::set_reduce_motion(false);
    assert!(desk.read_with(&vcx, |d, _| d.animating()), "moving again");
}

/// qass's line is said in words a screen reader reads right: "idle 23 minutes, claude", where
/// the column draws "idle 23m, claude".
#[gpui::test]
fn the_qass_row_says_its_age_in_words(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    desk.update(&mut vcx, |d, _| {
        let at = d.now - 23 * 60;
        for r in d.model.rows.iter_mut().filter(|r| r.terminal == 9) {
            r.state = "idle".into();
            r.status = Some("idle".into());
            r.last_active_at_unix = Some(at);
        }
    });
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.qass_note()),
        (
            "idle 23m, claude".to_string(),
            "idle 23 minutes, claude".to_string()
        )
    );
}

/// The mark's ring on qass's avatar is drawn only on the column's own ground: not on the row
/// of the page on screen. And qass not running still wears its agent's face.
#[gpui::test]
fn the_avatar_ring_is_only_on_the_columns_ground(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    assert!(vcx.debug_bounds("qass-ring").is_some(), "on the Inbox");
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Qass, window, cx)));
    vcx.run_until_parked();
    assert!(
        desk.read_with(&vcx, |d, _| d.nav_on(Page::Qass)),
        "qass's row is the current one"
    );
    assert!(vcx.debug_bounds("qass-ring").is_none(), "no patch on it");
    desk.update(&mut vcx, |d, _| d.model.rows.retain(|r| r.terminal != 9));
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.qass_program().map(str::to_string)),
        Some("claude".to_string()),
        "not running: the agent it runs as"
    );
}

// The desktop redesign, P2: the agent list.

/// The pointer onto the page, off the column.
fn pointer_away(vcx: &mut VisualTestContext) {
    vcx.simulate_mouse_move(
        gpui::point(gpui::px(900.), gpui::px(600.)),
        None,
        gpui::Modifiers::none(),
    );
    vcx.run_until_parked();
}

/// The pointer over the column's list.
fn pointer_on_column(vcx: &mut VisualTestContext) {
    vcx.simulate_mouse_move(
        gpui::point(gpui::px(120.), gpui::px(700.)),
        None,
        gpui::Modifiers::none(),
    );
    vcx.run_until_parked();
}

/// A key pressed and let go, as a control takes Enter or Space.
fn press_key(vcx: &mut VisualTestContext, key: &'static str) {
    vcx.simulate_keystrokes(key);
    vcx.update(|window, cx| {
        window.dispatch_event(
            gpui::PlatformInput::KeyUp(gpui::KeyUpEvent {
                keystroke: gpui::Keystroke::parse(key).expect("a key"),
            }),
            cx,
        );
    });
    vcx.run_until_parked();
}

/// The agents as the fixture has them, changed by `change`.
fn listed_changed(change: impl Fn(&mut qoop_runtime::control::AgentRow)) -> ControlResponse {
    let mut rows = fixture().rows;
    rows.iter_mut().for_each(change);
    ControlResponse::Agents {
        id: "agents".into(),
        agents: rows,
        splits: Vec::new(),
        qass_checks: None,
    }
}

/// The spaces as the fixture has them, with `more`.
fn spaces_with(more: Vec<qoop_runtime::control::WorkspaceRow>) -> ControlResponse {
    let mut workspaces = fixture().spaces;
    workspaces.extend(more);
    ControlResponse::Workspaces {
        id: "spaces".into(),
        workspaces,
    }
}

/// The blocks the column draws, by space, and whether each is open.
fn drawn_blocks(desk: &Entity<Desk>, vcx: &VisualTestContext) -> Vec<(Option<i64>, bool)> {
    desk.read_with(vcx, |d, _| {
        d.drawn_column(&d.inbox())
            .blocks
            .iter()
            .map(|b| (b.space, b.open))
            .collect()
    })
}

/// The request of the kind `pick` among what was sent.
fn sent_request(
    rx: &mut UnboundedReceiver<Up>,
    pick: impl Fn(&ControlRequest) -> bool,
) -> Option<ControlRequest> {
    request_of(&sent(rx), pick)
}

/// A space's chevron folds and opens it; a quiet space starts folded; the person's toggle wins
/// over the default afterwards (the agent on screen no longer opens a space they folded).
#[gpui::test]
fn a_space_folds_by_its_chevron_and_a_toggle_wins_over_the_default(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    assert!(
        vcx.debug_bounds("tree-row-1").is_some(),
        "acme-web: 2 waits"
    );
    assert!(
        vcx.debug_bounds("tree-row-4").is_none(),
        "api is quiet: folded"
    );
    click_on(&mut vcx, "side-fold-2");
    assert!(vcx.debug_bounds("tree-row-4").is_some(), "opened");
    click_on(&mut vcx, "side-fold-1");
    assert!(vcx.debug_bounds("tree-row-1").is_none(), "folded");
    pointer_away(&mut vcx);
    // The agent on screen in acme-web would open it by default: the person folded it.
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(2), window, cx)));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("tree-row-2").is_none(), "the toggle wins");
    assert!(vcx.debug_bounds("tree-row-5").is_some(), "api stays open");
}

/// A click on a space's name opens the field with the name selected; what is typed replaces
/// it, and Enter sends `RenameWorkspace` with the name trimmed. The field says Renaming until
/// the answer and the spaces come back, then closes.
#[gpui::test]
fn clicking_a_space_name_renames_it(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    click_on(&mut vcx, "side-name-1");
    assert!(vcx.debug_bounds("side-edit").is_some(), "the field");
    let selected = desk.read_with(&vcx, |d, cx| {
        let f = d.space_edit.as_ref().expect("editing").field.read(cx);
        (f.text.clone(), f.selection())
    });
    assert_eq!(
        selected,
        ("acme-web".to_string(), 0..8),
        "the name, selected"
    );
    vcx.simulate_input("  shop  ");
    sent(&mut rx);
    vcx.simulate_keystrokes("enter");
    let asked = sent_request(&mut rx, |r| {
        matches!(r, ControlRequest::RenameWorkspace { .. })
    })
    .expect("a rename");
    assert!(
        matches!(&asked, ControlRequest::RenameWorkspace { workspace: 1, name, .. } if name == "shop"),
        "{asked:?}"
    );
    let said = |desk: &Entity<Desk>, vcx: &VisualTestContext| {
        desk.read_with(vcx, |d, _| {
            d.space_edit.as_ref().and_then(|e| e.said.clone())
        })
    };
    assert_eq!(said(&desk, &vcx).as_deref(), Some("Renaming"));
    answer(
        &desk,
        &mut vcx,
        ControlResponse::Renamed {
            id: asked.request_id().to_string(),
        },
    );
    assert!(
        sent_request(&mut rx, |r| matches!(
            r,
            ControlRequest::ListWorkspaces { .. }
        ))
        .is_some(),
        "the spaces are read again"
    );
    assert!(said(&desk, &vcx).is_some(), "open until the list shows it");
    let mut spaces = fixture().spaces;
    spaces[0].name = "shop".into();
    answer(
        &desk,
        &mut vcx,
        ControlResponse::Workspaces {
            id: "spaces".into(),
            workspaces: spaces,
        },
    );
    assert!(
        desk.read_with(&vcx, |d, _| d.space_edit.is_none()),
        "closed"
    );
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.model.space(1).map(|w| w.name.clone())),
        Some("shop".to_string())
    );
}

/// A rename the daemon refuses keeps the field open with what was typed, and says why.
#[gpui::test]
fn a_refused_rename_keeps_the_field_and_says_why(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    click_on(&mut vcx, "side-name-1");
    vcx.simulate_input("shop");
    sent(&mut rx);
    vcx.simulate_keystrokes("enter");
    let asked = sent_request(&mut rx, |r| {
        matches!(r, ControlRequest::RenameWorkspace { .. })
    })
    .expect("a rename");
    answer(
        &desk,
        &mut vcx,
        ControlResponse::Failed {
            id: asked.request_id().to_string(),
            reason: "no such workspace".into(),
        },
    );
    let (text, said) = desk.read_with(&vcx, |d, cx| {
        let e = d.space_edit.as_ref().expect("still open");
        (e.field.read(cx).text.clone(), e.said.clone())
    });
    assert_eq!(text, "shop", "what was typed is kept");
    assert_eq!(said.as_deref(), Some("no such workspace"));
    assert!(
        vcx.debug_bounds("side-edit-said").is_some(),
        "said under the field"
    );
}

/// On the Inbox with a card in the ring, a digit typed in the rename field is a letter of the
/// name, never an answer; Escape leaves the field, not the Inbox.
#[gpui::test]
fn typing_in_a_space_field_answers_no_card_and_stays_on_the_inbox(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.asks = vec![tab_card("Q1", 1, "open")];
            d.open(Page::Session(1), window, cx);
            d.open(Page::Inbox, window, cx);
            d.set_cursor(crate::state::Slot::Card("Q1".into()), 0);
        })
    });
    vcx.run_until_parked();
    click_on(&mut vcx, "side-name-1");
    vcx.simulate_keystrokes("v 2");
    let typed = desk.read_with(&vcx, |d, cx| {
        d.space_edit.as_ref().map(|e| e.field.read(cx).text.clone())
    });
    assert_eq!(typed.as_deref(), Some("v2"), "typed into the field");
    assert!(
        desk.read_with(&vcx, |d, _| d.holds.held.is_empty()),
        "no card answered"
    );
    vcx.simulate_keystrokes("escape");
    assert_eq!(
        desk.read_with(&vcx, |d, _| (d.page, d.space_edit.is_none())),
        (Page::Inbox, true),
        "the field left, the Inbox kept"
    );
}

/// `+` in the strip opens the new-space field; Enter sends `CreateWorkspace`, the answer reads
/// the spaces again, and the new space is listed, folded, "0 agents", once they come.
#[gpui::test]
fn the_new_space_field_sends_create_workspace_and_lists_again_on_the_answer(
    cx: &mut TestAppContext,
) {
    let (desk, mut vcx, mut rx) = open(cx);
    click_on(&mut vcx, "side-new-space");
    vcx.simulate_input(" ideas ");
    sent(&mut rx);
    vcx.simulate_keystrokes("enter");
    let asked = sent_request(&mut rx, |r| {
        matches!(r, ControlRequest::CreateWorkspace { .. })
    })
    .expect("a new space");
    assert!(
        matches!(&asked, ControlRequest::CreateWorkspace { name, .. } if name == "ideas"),
        "{asked:?}"
    );
    answer(
        &desk,
        &mut vcx,
        ControlResponse::WorkspaceCreated {
            id: asked.request_id().to_string(),
            workspace: 8,
        },
    );
    assert!(
        sent_request(&mut rx, |r| matches!(
            r,
            ControlRequest::ListWorkspaces { .. }
        ))
        .is_some()
    );
    answer(
        &desk,
        &mut vcx,
        spaces_with(vec![crate::state::tests::space(8, "ideas")]),
    );
    assert!(
        desk.read_with(&vcx, |d, _| d.space_edit.is_none()),
        "closed"
    );
    assert!(vcx.debug_bounds("side-space-8").is_some(), "listed");
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.heading_count(8, false)),
        "0 agents",
        "what its heading says"
    );
    assert!(
        drawn_blocks(&desk, &vcx).contains(&(Some(8), false)),
        "folded"
    );
}

/// `+` on a space's heading opens New agent in that space.
#[gpui::test]
fn plus_on_a_space_opens_new_agent_in_it(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    click_on(&mut vcx, "side-plus-2");
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.sheet.as_ref().map(|s| s.space)),
        Some(Some(2))
    );
}

/// The heading's menu by keyboard: Enter on `...` opens it, Down moves, Escape closes it with
/// the keys back on `...`; opened again, Down and Enter pick Rename.
#[gpui::test]
fn the_space_menu_works_by_keyboard(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    let more = desk
        .read_with(&vcx, |d, _| d.more_focus.borrow().get(&1).cloned())
        .expect("the ... of acme-web");
    vcx.update(|window, cx| window.focus(&more, cx));
    press_key(&mut vcx, "enter");
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.space_menu.as_ref().map(|m| (m.space, m.at))),
        Some((1, 0)),
        "open on its first item"
    );
    assert!(vcx.debug_bounds("side-menu").is_some());
    vcx.simulate_keystrokes("down");
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.space_menu.as_ref().map(|m| m.at)),
        Some(1)
    );
    // The item the keys are on has the focus, so a screen reader says it.
    let second = desk.read_with(&vcx, |d, _| d.menu_items[1].clone());
    assert!(
        vcx.update(|window, _| second.is_focused(window)),
        "Rename has the focus"
    );
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(
        desk.read_with(&vcx, |d, _| d.space_menu.is_none()),
        "closed"
    );
    assert!(
        vcx.update(|window, _| more.is_focused(window)),
        "the keys are back on ..."
    );
    press_key(&mut vcx, "enter");
    vcx.simulate_keystrokes("down enter");
    vcx.run_until_parked();
    let renaming = desk.read_with(&vcx, |d, cx| {
        d.space_edit
            .as_ref()
            .map(|e| (e.target, e.field.read(cx).text.clone()))
    });
    assert_eq!(
        renaming,
        Some((crate::column::EditFor::Rename(1), "acme-web".to_string()))
    );
}

/// A state tab filters the list to that state: a space with matches says "1 of 3", one with
/// none says "none working" and folds, and a line says how many all shows besides; the chosen
/// tab again goes back to all.
#[gpui::test]
fn the_filter_shows_one_state_and_says_how_many_are_hidden(cx: &mut TestAppContext) {
    use crate::column::{count_words, filter_words};
    use crate::state::StateTab;
    let (desk, mut vcx, _rx) = open(cx);
    click_on(&mut vcx, "side-tab-working");
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.side_filter),
        StateTab::Working
    );
    pointer_away(&mut vcx);
    assert!(vcx.debug_bounds("tree-row-1").is_some(), "working");
    assert!(
        vcx.debug_bounds("tree-row-5").is_some(),
        "working, in a quiet space"
    );
    for not in ["tree-row-2", "tree-row-3", "tree-row-4"] {
        assert!(vcx.debug_bounds(not).is_none(), "{not} is not working");
    }
    assert!(vcx.debug_bounds("side-filter-note").is_some());
    // Three of the seven work: the line says so.
    let (shown, all) = desk.read_with(&vcx, |d, _| {
        let agents = d.model.agents();
        (
            agents
                .iter()
                .filter(|e| crate::state::in_filter(e, StateTab::Working))
                .count(),
            agents.len(),
        )
    });
    assert_eq!((shown, all), (3, 7));
    assert_eq!(
        filter_words(StateTab::Working, shown, all - shown),
        (
            "Showing working agents.".to_string(),
            "shows the other 4.".to_string()
        )
    );
    // What the headings say, as drawn: acme-web has one of its three working, open.
    let web_open = drawn_blocks(&desk, &vcx).contains(&(Some(1), true));
    assert!(web_open);
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.heading_count(1, web_open)),
        "1 of 3"
    );
    assert_eq!(count_words(StateTab::Working, 0, 2, false), "none working");
    desk.update(&mut vcx, |d, cx| {
        d.side_filter = StateTab::Failed;
        cx.notify();
    });
    vcx.run_until_parked();
    assert_eq!(
        drawn_blocks(&desk, &vcx).iter().find(|b| b.0 == Some(2)),
        Some(&(Some(2), false)),
        "api, none failed, folded"
    );
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.heading_count(2, false)),
        "none failed"
    );
    desk.update(&mut vcx, |d, cx| {
        d.side_filter = StateTab::Working;
        cx.notify();
    });
    vcx.run_until_parked();
    assert_eq!(filter_words(StateTab::Failed, 0, 7).0, "No failed agents.");
    // The chosen tab again: all.
    click_on(&mut vcx, "side-tab-working");
    assert_eq!(desk.read_with(&vcx, |d, _| d.side_filter), StateTab::All);
    assert!(vcx.debug_bounds("side-filter-note").is_none());
}

/// While the pointer is on the column, or the keys drive inside it, nothing moves: a re-sort,
/// a new Inbox item, a space whose default would open and an agent leaving the filter all
/// wait, the new ones counted at the foot. Off the column it settles once. A focus a click
/// left there holds nothing once the pointer leaves; one Tab brought there holds.
#[gpui::test]
fn the_list_holds_while_hovered_or_focused(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    let before = drawn_blocks(&desk, &vcx);
    assert_eq!(before[0], (Some(1), true), "acme-web first, open");
    pointer_on_column(&mut vcx);
    assert!(desk.read_with(&vcx, |d, _| d.held.is_some()), "held");
    // From elsewhere: api's 5 becomes the newest and its 4 starts to wait.
    answer(
        &desk,
        &mut vcx,
        listed_changed(|r| match r.terminal {
            5 => r.last_active_at_unix = Some(1990),
            4 => {
                r.status = Some("blocked".into());
                r.state = "waiting".into();
            }
            _ => {}
        }),
    );
    assert_eq!(
        drawn_blocks(&desk, &vcx),
        before,
        "nothing moved, api still folded"
    );
    assert!(
        vcx.debug_bounds("side-new").is_some(),
        "the new item is counted"
    );
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.drawn_column(&d.inbox()).inbox.len()),
        1,
        "the small Inbox keeps its one drawn row"
    );
    pointer_away(&mut vcx);
    let after = drawn_blocks(&desk, &vcx);
    assert_eq!(after[0], (Some(2), true), "settled once: api first, open");
    // A row leaving the filter stays, dimmed, while held.
    click_on(&mut vcx, "side-tab-working");
    answer(
        &desk,
        &mut vcx,
        listed_changed(|r| {
            if r.terminal == 1 {
                r.status = Some("done".into());
                r.state = "done".into();
            }
        }),
    );
    assert!(vcx.debug_bounds("tree-row-1").is_some(), "dimmed in place");
    pointer_away(&mut vcx);
    assert!(vcx.debug_bounds("tree-row-1").is_none(), "gone once let go");
    // A focus a click left on the column holds nothing once the pointer is on the page.
    assert!(desk.read_with(&vcx, |d, _| d.held.is_none()));
    // Tab into the column: the keys drive there, and it holds.
    vcx.update(|window, cx| {
        let root = desk.read(cx).focus.clone();
        window.focus(&root, cx);
    });
    vcx.simulate_keystrokes("tab tab");
    vcx.run_until_parked();
    assert!(
        desk.read_with(&vcx, |d, _| d.held.is_some()),
        "held by the keys"
    );
}

/// With the pointer on the column and a re-sort from elsewhere waiting, the person's own
/// changes show at once and only they: a space they made is listed, folded, at the end; their
/// fold opens api; the re-sort still waits. A filter switch takes the live order.
#[gpui::test]
fn the_persons_own_changes_show_while_held(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    // The pointer comes onto the column before it clicks there, as a person's does.
    pointer_on_column(&mut vcx);
    click_on(&mut vcx, "side-new-space");
    assert!(desk.read_with(&vcx, |d, _| d.held.is_some()), "held");
    answer(
        &desk,
        &mut vcx,
        listed_changed(|r| {
            if r.terminal == 5 {
                r.last_active_at_unix = Some(1990);
            }
        }),
    );
    let order = |desk: &Entity<Desk>, vcx: &VisualTestContext| {
        drawn_blocks(desk, vcx)
            .iter()
            .map(|b| b.0)
            .collect::<Vec<_>>()
    };
    assert_eq!(order(&desk, &vcx)[0], Some(1), "the re-sort waits");
    vcx.simulate_input("ideas");
    sent(&mut rx);
    vcx.simulate_keystrokes("enter");
    let asked = sent_request(&mut rx, |r| {
        matches!(r, ControlRequest::CreateWorkspace { .. })
    })
    .expect("a new space");
    answer(
        &desk,
        &mut vcx,
        ControlResponse::WorkspaceCreated {
            id: asked.request_id().to_string(),
            workspace: 8,
        },
    );
    answer(
        &desk,
        &mut vcx,
        spaces_with(vec![crate::state::tests::space(8, "ideas")]),
    );
    let now = order(&desk, &vcx);
    assert_eq!(now.last(), Some(&Some(8)), "listed at once, last: {now:?}");
    assert_eq!(now[0], Some(1), "the re-sort still waits");
    click_on(&mut vcx, "side-fold-2");
    assert!(
        drawn_blocks(&desk, &vcx).contains(&(Some(2), true)),
        "their fold"
    );
    assert_eq!(order(&desk, &vcx)[0], Some(1), "and still nothing else");
    // A filter switch draws the whole list again, from the live order.
    click_on(&mut vcx, "side-tab-working");
    assert_eq!(order(&desk, &vcx)[0], Some(2), "api, the newest, first");
}

/// Two empty Enters before the first answer make "space 4" and "space 5", never two "space
/// 4"; a refusal stops counting.
#[gpui::test]
fn two_quick_empty_new_spaces_get_two_numbers(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    click_on(&mut vcx, "side-new-space");
    sent(&mut rx);
    vcx.simulate_keystrokes("enter");
    vcx.simulate_keystrokes("enter");
    let names: Vec<(String, String)> = sent(&mut rx)
        .into_iter()
        .filter_map(|u| match u {
            Up::Request(r) => match *r {
                ControlRequest::CreateWorkspace { id, name } => Some((id, name)),
                _ => None,
            },
            _ => None,
        })
        .collect();
    let just: Vec<&str> = names.iter().map(|(_, n)| n.as_str()).collect();
    assert_eq!(just, ["space 4", "space 5"]);
    answer(
        &desk,
        &mut vcx,
        ControlResponse::Failed {
            id: names[1].0.clone(),
            reason: "no".into(),
        },
    );
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.next_space_name()),
        "space 5",
        "one still out"
    );
}

/// An empty or blank new space takes the numbered name the field shows, the one `ctrl-a w`
/// gives; an empty rename sends nothing and says why.
#[gpui::test]
fn an_empty_new_space_takes_the_numbered_name_and_an_empty_rename_sends_nothing(
    cx: &mut TestAppContext,
) {
    let (desk, mut vcx, mut rx) = open(cx);
    let tui = qoop_tui::run::space_name_after(&fixture().spaces, None, 0);
    assert_eq!(tui, "space 4");
    click_on(&mut vcx, "side-new-space");
    let shown = desk.read_with(&vcx, |d, cx| {
        d.space_edit
            .as_ref()
            .map(|e| e.field.read(cx).placeholder.to_string())
    });
    assert_eq!(shown.as_deref(), Some("space 4"), "the placeholder");
    vcx.simulate_input("   ");
    sent(&mut rx);
    vcx.simulate_keystrokes("enter");
    let asked = sent_request(&mut rx, |r| {
        matches!(r, ControlRequest::CreateWorkspace { .. })
    });
    assert!(
        matches!(&asked, Some(ControlRequest::CreateWorkspace { name, .. }) if *name == tui),
        "{asked:?}"
    );
    vcx.simulate_keystrokes("escape");
    // An empty rename.
    click_on(&mut vcx, "side-name-1");
    vcx.simulate_keystrokes("backspace");
    sent(&mut rx);
    vcx.simulate_keystrokes("enter");
    assert!(
        sent_request(&mut rx, |r| matches!(
            r,
            ControlRequest::RenameWorkspace { .. }
        ))
        .is_none(),
        "nothing sent"
    );
    assert_eq!(
        desk.read_with(&vcx, |d, _| d
            .space_edit
            .as_ref()
            .and_then(|e| e.said.clone())),
        Some("A space needs a name.".to_string())
    );
}

/// The Inbox row folds its first rows: five at most, then "N more in the Inbox"; nothing
/// waiting, it says so; the chevron folds it away.
#[gpui::test]
fn the_inbox_row_folds_its_first_rows(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    assert!(vcx.debug_bounds("side-item-agent-2").is_some(), "2 waits");
    desk.update(&mut vcx, |d, cx| {
        d.asks = (0..6)
            .map(|n| tab_card(&format!("Q{n}"), 3, "open"))
            .collect();
        cx.notify();
    });
    vcx.run_until_parked();
    let shown = desk.read_with(&vcx, |d, _| d.drawn_column(&d.inbox()).inbox.len());
    assert_eq!(shown, 5, "five at most");
    assert!(vcx.debug_bounds("side-inbox-more").is_some(), "2 more");
    click_on(&mut vcx, "side-inbox-fold");
    assert!(vcx.debug_bounds("side-inbox").is_none(), "folded");
    click_on(&mut vcx, "side-inbox-fold");
    pointer_away(&mut vcx);
    desk.update(&mut vcx, |d, cx| {
        d.asks.clear();
        cx.notify();
    });
    answer(
        &desk,
        &mut vcx,
        listed_changed(|r| {
            if r.terminal == 2 {
                r.status = Some("working".into());
                r.state = "working".into();
            }
        }),
    );
    assert!(
        vcx.debug_bounds("side-inbox-none").is_some(),
        "nothing waits"
    );
}

/// The window in front again reads the spaces: one made or renamed in the TUI or on the phone
/// has no event of its own.
#[gpui::test]
fn the_spaces_are_read_again_on_activation(cx: &mut TestAppContext) {
    let (_desk, mut vcx, mut rx) = open(cx);
    vcx.deactivate_window();
    sent(&mut rx);
    vcx.update(|window, _| window.activate_window());
    vcx.run_until_parked();
    assert!(
        sent_request(&mut rx, |r| matches!(
            r,
            ControlRequest::ListWorkspaces { .. }
        ))
        .is_some(),
        "read again"
    );
}

// Review round 1 of P2.

/// A click on a menu item is the item's only: the agent row under it does not open, and the
/// keys stay in the rename field the item opened. The column stays held meanwhile.
#[gpui::test]
fn a_menu_item_click_never_reaches_the_row_under_it(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    pointer_on_column(&mut vcx);
    click_on(&mut vcx, "side-more-1");
    assert!(desk.read_with(&vcx, |d, _| d.space_menu.is_some()));
    let item = vcx.debug_bounds("side-menu-1").expect("Rename").center();
    let under = ["tree-row-1", "tree-row-2", "tree-row-3"]
        .into_iter()
        .find(|row| vcx.debug_bounds(row).is_some_and(|b| b.contains(&item)));
    assert!(under.is_some(), "an agent row lies under Rename");
    vcx.simulate_mouse_move(item, None, gpui::Modifiers::none());
    assert!(
        desk.read_with(&vcx, |d, _| d.held.is_some()),
        "held with the pointer on the menu"
    );
    vcx.simulate_click(item, gpui::Modifiers::none());
    vcx.run_until_parked();
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.page),
        Page::Inbox,
        "the row under it did not open"
    );
    let in_field = vcx.update(|window, cx| {
        desk.read(cx)
            .space_edit
            .as_ref()
            .is_some_and(|e| e.field.read(cx).focus.is_focused(window))
    });
    assert!(in_field, "the keys are in the rename field");
}

/// The open menu has the keys only while it has the focus: ⌘J to another agent closes it,
/// and Space then goes to that agent, never to the menu's first item.
#[gpui::test]
fn the_menu_lets_the_keys_go_when_the_focus_leaves(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(1), window, cx)));
    vcx.run_until_parked();
    pointer_on_column(&mut vcx);
    click_on(&mut vcx, "side-more-1");
    assert!(desk.read_with(&vcx, |d, _| d.space_menu.is_some()));
    vcx.simulate_keystrokes("cmd-j");
    vcx.run_until_parked();
    assert_eq!(desk.read_with(&vcx, |d, _| d.page), Page::Session(2));
    assert!(
        desk.read_with(&vcx, |d, _| d.space_menu.is_none()),
        "closed"
    );
    sent(&mut rx);
    vcx.simulate_keystrokes("space");
    assert!(
        desk.read_with(&vcx, |d, _| d.sheet.is_none()),
        "no New agent"
    );
    assert!(
        sent(&mut rx)
            .iter()
            .any(|u| matches!(u, Up::Input(2, b) if b == b" ")),
        "the agent got the space"
    );
}

/// Opening the small Inbox while the column is held shows its rows at once: it is his own
/// action.
#[gpui::test]
fn opening_the_small_inbox_while_held_shows_its_rows(cx: &mut TestAppContext) {
    let (_desk, mut vcx, _rx) = open(cx);
    pointer_on_column(&mut vcx);
    click_on(&mut vcx, "side-inbox-fold");
    assert!(vcx.debug_bounds("side-item-agent-2").is_none(), "folded");
    pointer_away(&mut vcx);
    pointer_on_column(&mut vcx);
    click_on(&mut vcx, "side-inbox-fold");
    assert!(
        vcx.debug_bounds("side-item-agent-2").is_some(),
        "its row, at once"
    );
    assert!(vcx.debug_bounds("side-new").is_none(), "not counted as new");
}

/// Escape in the rename field gives the keys back to the space's name; in the new-space
/// field, to the strip's `+`.
#[gpui::test]
fn escape_in_a_space_field_gives_the_keys_back_to_its_name(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    click_on(&mut vcx, "side-name-1");
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    let name = desk.read_with(&vcx, |d, _| d.name_focus.borrow().get(&1).cloned());
    let on_name = vcx.update(|window, _| name.is_some_and(|h| h.is_focused(window)));
    assert!(on_name, "on the name");
    click_on(&mut vcx, "side-new-space");
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    let plus = desk.read_with(&vcx, |d, _| d.new_space_focus.clone());
    assert!(
        vcx.update(|window, _| plus.is_focused(window)),
        "on the strip's +"
    );
}

/// A second click on `...` closes its menu: the press outside the menu closes it, and the
/// click on `...` does not open it again.
#[gpui::test]
fn a_second_click_on_dots_closes_the_menu(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    pointer_on_column(&mut vcx);
    click_on(&mut vcx, "side-more-1");
    assert!(desk.read_with(&vcx, |d, _| d.space_menu.is_some()));
    click_on(&mut vcx, "side-more-1");
    assert!(
        desk.read_with(&vcx, |d, _| d.space_menu.is_none()),
        "closed"
    );
    click_on(&mut vcx, "side-more-1");
    assert!(
        desk.read_with(&vcx, |d, _| d.space_menu.is_some()),
        "a third click opens it again"
    );
}

/// A third empty Enter after the first space was made, before the spaces come back, still
/// gets the next number: a made space counts until it is listed.
#[gpui::test]
fn a_space_made_counts_until_it_is_listed(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    click_on(&mut vcx, "side-new-space");
    sent(&mut rx);
    let names = |rx: &mut UnboundedReceiver<Up>| -> Vec<(String, String)> {
        sent(rx)
            .into_iter()
            .filter_map(|u| match u {
                Up::Request(r) => match *r {
                    ControlRequest::CreateWorkspace { id, name } => Some((id, name)),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    };
    vcx.simulate_keystrokes("enter enter");
    let first = names(&mut rx);
    answer(
        &desk,
        &mut vcx,
        ControlResponse::WorkspaceCreated {
            id: first[0].0.clone(),
            workspace: 8,
        },
    );
    vcx.simulate_keystrokes("enter");
    let third = names(&mut rx);
    assert_eq!(third.len(), 1);
    assert_eq!(third[0].1, "space 6", "not a second space 5");
}

/// The foot's count says only what will show once let go: a new agent in a space that stays
/// folded is not counted.
#[gpui::test]
fn new_rows_in_a_folded_space_are_not_counted(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    pointer_on_column(&mut vcx);
    assert!(desk.read_with(&vcx, |d, _| d.held.is_some()));
    let mut rows = fixture().rows;
    rows.push(crate::state::tests::row(
        6,
        2,
        "idle",
        "claude",
        "a quiet new one",
    ));
    answer(
        &desk,
        &mut vcx,
        ControlResponse::Agents {
            id: "agents".into(),
            agents: rows,
            splits: Vec::new(),
            qass_checks: None,
        },
    );
    assert!(
        vcx.debug_bounds("side-new").is_none(),
        "api stays folded: nothing new to show"
    );
}

/// A chevron on a space with nothing on the filter's list keeps no choice: back on all, the
/// quiet space is folded as its default says.
#[gpui::test]
fn a_chevron_on_a_filtered_out_space_keeps_no_choice(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    click_on(&mut vcx, "side-tab-wants");
    click_on(&mut vcx, "side-fold-2");
    assert!(
        desk.read_with(&vcx, |d, _| !d.folds.contains_key(&2)),
        "no choice kept"
    );
    click_on(&mut vcx, "side-tab-wants");
    pointer_away(&mut vcx);
    assert!(vcx.debug_bounds("tree-row-4").is_none(), "api still folded");
}

/// The computer page lists what asks for the person first, whatever was active last.
#[gpui::test]
fn the_computer_page_lists_what_waits_first(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    answer(
        &desk,
        &mut vcx,
        listed_changed(|r| {
            if r.terminal == 5 {
                r.last_active_at_unix = Some(1990);
            }
        }),
    );
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Computer(0), window, cx)));
    vcx.run_until_parked();
    let y = |vcx: &mut VisualTestContext, what: &'static str| {
        vcx.debug_bounds(what).expect("drawn").origin.y
    };
    let waits = y(&mut vcx, "computer-agent-2");
    let newest = y(&mut vcx, "computer-agent-5");
    assert!(waits < newest, "Needs you above the newest working one");
}

/// An agent row's name says its age in words, the same age the row draws.
#[gpui::test]
fn an_agent_rows_name_says_its_age_in_words(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    answer(
        &desk,
        &mut vcx,
        listed_changed(|r| {
            if r.terminal == 2 {
                r.last_active_at_unix = Some(2000 - 22 * 60);
            }
        }),
    );
    let name = desk.read_with(&vcx, |d, _| {
        let inbox = d.inbox();
        let agents = d.model.agents();
        let two = agents
            .iter()
            .find(|e| e.terminal() == 2)
            .copied()
            .expect("2");
        (d.row_name(two, &inbox), d.side_age(two, &inbox))
    });
    assert!(name.0.ends_with("Needs you, 22 minutes"), "{}", name.0);
    assert_eq!(name.1.as_deref(), Some("22m"), "drawn short");
}

/// The avatar draws its mark's ring only when asked: never as a patch on a row painted
/// otherwise.
#[gpui::test]
fn the_avatar_draws_its_ring_only_when_asked(cx: &mut TestAppContext) {
    use gpui::{Context, Render, Window, div, prelude::*};

    struct Face(bool);
    impl Render for Face {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().child(crate::ui::avatar(
                Some("claude"),
                crate::state::Kind::Status(qoop_domain::status::Status::Idle),
                20.,
                self.0,
                &crate::theme::DARK,
            ))
        }
    }

    for ring in [true, false] {
        let window = cx.add_window(move |_, _| Face(ring));
        let mut vcx = VisualTestContext::from_window(window.into(), cx);
        vcx.run_until_parked();
        assert_eq!(vcx.debug_bounds("avatar-ring").is_some(), ring, "{ring}");
    }
}

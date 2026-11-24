//! The window headless, on GPUI's test platform: home from fixture rows, the prefix keys, New
//! work's keyboard path, and the input method in the text field.

use gpui::{AppContext, Entity, EntityInputHandler, TestAppContext, VisualTestContext};
use qoop_runtime::control::{ControlRequest, ControlResponse, LinkState};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::app::{Desk, Page};
use crate::link::{Down, Latest, Up};
use crate::state::tests::fixture;

/// The window reads (and the shared hand rules may make) the hand id file under the data home:
/// every test gets a folder of its own for it, never the owner's.
fn isolate_data_home() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir = Box::leak(Box::new(tempfile::tempdir().expect("a data home")));
        // Before any window of this run reads it: every test goes through here first.
        unsafe { std::env::set_var("XDG_DATA_HOME", dir.path()) };
    });
}

fn open(cx: &mut TestAppContext) -> (Entity<Desk>, VisualTestContext, UnboundedReceiver<Up>) {
    // Every setting its default (prefix ctrl-a), never the person's own file.
    open_with(cx, std::path::Path::new("/nonexistent/config.toml"))
}

fn open_with(
    cx: &mut TestAppContext,
    config: &std::path::Path,
) -> (Entity<Desk>, VisualTestContext, UnboundedReceiver<Up>) {
    isolate_data_home();
    let (up, rx) = tokio::sync::mpsc::unbounded_channel();
    let config = qoop_tui::config::read_from(config);
    let window = cx.add_window(|window, cx| Desk::new(up, Latest::default(), &config, window, cx));
    let mut vcx = VisualTestContext::from_window(window.into(), cx);
    let desk = window.root(&mut vcx).expect("the window's view");
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.apply(Down::Connected, window, cx);
            // Never the owner's own qass lessons.
            d.lessons_path = std::path::PathBuf::from("/nonexistent/qass/lessons.md");
            d.config_path = std::path::PathBuf::from("/nonexistent/qoop/config.toml");
            d.pet_path = std::path::PathBuf::from("/nonexistent/qoop/avatar.json");
            d.model = fixture();
            d.note_hand();
            d.listed = true;
            d.now = 2000;
            cx.notify();
        })
    });
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

/// The sidebar lists, under each space, the agents that wait for the owner and the one on
/// screen; the rest are folded behind "N more", and a space where nothing waits is one line.
/// Qass's own row is never listed. "N more" lists the whole space, sleepers included.
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
    // acme-web: the agent that waits (2) and "2 more"; api and staging: nothing waits, one line.
    let order = [
        "side-space-1",
        "tree-row-2",
        "side-more-1",
        "side-space-2",
        remote_space,
    ];
    let ys: Vec<_> = order.iter().map(|s| y(&mut vcx, s)).collect();
    assert!(ys.windows(2).all(|w| w[0] < w[1]), "{order:?} at {ys:?}");
    for folded in [
        "tree-row-1",
        "tree-row-3",
        "tree-row-4",
        "tree-row-5",
        "tree-row-9",
    ] {
        assert!(
            vcx.debug_bounds(folded).is_none(),
            "{folded} is folded or qass's"
        );
    }
    assert!(
        vcx.debug_bounds("side-more-2").is_none(),
        "a quiet space is one line"
    );
    // The agent on screen is always listed, and its space is no longer a quiet line.
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(5), window, cx)));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("tree-row-5").is_some(), "the open agent");
    assert!(
        vcx.debug_bounds("side-more-2").is_some(),
        "its space folds the other one"
    );
    assert!(vcx.debug_bounds("tree-row-4").is_none());
    // "N more" opens the whole space.
    vcx.update(|_, cx| {
        desk.update(cx, |d, cx| {
            d.expanded.insert(1);
            d.expanded.insert(remote);
            cx.notify();
        })
    });
    vcx.run_until_parked();
    for listed in [
        "tree-row-1",
        "tree-row-3",
        "tree-row-2164260865",
        "tree-row-2164260866",
    ] {
        assert!(vcx.debug_bounds(listed).is_some(), "{listed} once expanded");
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
        desk.read_with(&vcx, |d, _| d.sheet.is_none()),
        "the sheet closes"
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
/// and is not offered "Let qass handle it" again.
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
                label: "Let qass handle it".into(),
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

/// The qass page follows qass away as well as back: once its tab is a plain shell, the page
/// drops the dead terminal and shows Start at once.
#[gpui::test]
fn the_qass_page_drops_a_terminal_qass_left(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Qass, window, cx)));
    vcx.run_until_parked();
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.term.as_ref().map(|t| t.terminal)),
        Some(9)
    );
    let mut rows = crate::state::tests::fixture().rows;
    rows.iter_mut().find(|r| r.terminal == 9).unwrap().agent = false;
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
        desk.read_with(&vcx, |d, _| d.term.is_none()),
        "no dead terminal"
    );
}

/// What qass did today is asked when the qass page opens and, while it is open, when an agent that
/// waited works again (a write of qass's raises no event of its own); an act names its agent
/// as the status line does.
#[gpui::test]
fn qass_acts_are_read_when_shown_and_named_by_space(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    let acts_asked = |rx: &mut UnboundedReceiver<Up>| {
        request_of(&sent(rx), |r| matches!(r, ControlRequest::QassActs { .. })).is_some()
    };
    sent(&mut rx);
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Qass, window, cx)));
    assert!(acts_asked(&mut rx), "asked as the qass page opened");
    let mut rows = crate::state::tests::fixture().rows;
    rows[2].state = "working".into();
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
        acts_asked(&mut rx),
        "asked once a waiting agent works again"
    );
    let text = desk.read_with(&vcx, |d, _| {
        let act: qoop_runtime::control::QassAct = serde_json::from_value(serde_json::json!({
            "at": 1000, "kind": "wrote", "terminal": 3, "text": "wrote to T3"
        }))
        .expect("act");
        d.model.act_text(&act)
    });
    assert_eq!(text, "wrote to acme-web/Coupon.ts test suggestion");
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

/// Every sidebar link takes the ring: from the window's own focus, Tab once reaches Settings,
/// (twice New agent,) three times Inbox, four times qass, each focused with a key as the last input (what
/// `focus_visible` draws the ring on), and Space opens the link that has it.
#[gpui::test]
fn every_sidebar_link_takes_tab_and_its_ring(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    for (tabs, link, from) in [
        (1, Page::Settings, Page::Inbox),
        (3, Page::Inbox, Page::Settings),
        (4, Page::Qass, Page::Inbox),
    ] {
        vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(from, window, cx)));
        vcx.run_until_parked();
        vcx.simulate_keystrokes(&vec!["tab"; tabs].join(" "));
        let (keyboard, on_link) = vcx.update(|window, cx| {
            let root = desk.read(cx).focus.clone();
            (
                window.last_input_was_keyboard(),
                window.focused(cx).is_some() && !root.is_focused(window),
            )
        });
        assert!(keyboard, "{link:?}: a key was the last input");
        assert!(on_link, "{link:?}: a control has the focus");
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
        assert_eq!(
            desk.read_with(&vcx, |d, _| d.page),
            link,
            "Tab {tabs} times is the {link:?} link"
        );
    }
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
        vcx.debug_bounds("task-strip").is_some(),
        "the task strip stays beside the question"
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

/// The qass page's switches: All asks qass to stop watching, a space's and a tab's switch tell
/// qass to leave them alone.
#[gpui::test]
fn the_qass_switches_send_their_requests(cx: &mut TestAppContext) {
    let (desk, mut vcx, mut rx) = open(cx);
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Qass, window, cx)));
    vcx.run_until_parked();
    sent(&mut rx);
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

/// The rules card counts the lessons file the page is pointed at (a test's own, never the
/// owner's), and qass's own terminal brought forward keeps the qass page.
#[gpui::test]
fn the_qass_page_reads_its_lessons_and_keeps_its_page(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("lessons.md");
    std::fs::write(
        &path,
        "- a release -> escalate (owner)\n- a test run -> do it (E7)\n- a bump -> wait (E9)\n",
    )
    .expect("lessons");
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.lessons_path = path.clone();
            d.open(Page::Qass, window, cx);
        })
    });
    let lessons = desk.read_with(&vcx, |d, _| d.lessons);
    assert_eq!(
        lessons,
        Some(crate::state::Lessons {
            yours: 1,
            alone: 1,
            all: 3,
        })
    );
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.open(Page::Inbox, window, cx);
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
    assert_eq!(desk.read_with(&vcx, |d, _| d.page), Page::Qass);
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
                format!("Next run: {}", crate::state::clock_at(at, d.now, d.zone)),
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
        "ctrl-b c opens New work"
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
            d.expanded.extend([7, 1i64 << 40 | 1]);
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

/// A reply waits while its machine cannot be reached: nothing is typed, the words are kept.
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
    for _ in 0..40 {
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

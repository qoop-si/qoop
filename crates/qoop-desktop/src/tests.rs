//! The window headless, on GPUI's test platform: home from fixture rows, the prefix keys, New
//! work's keyboard path, and the input method in the text field.

use gpui::{Entity, EntityInputHandler, TestAppContext, VisualTestContext};
use qoop_runtime::control::{ControlRequest, ControlResponse};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::app::{Desk, Page};
use crate::link::{Down, Latest, Up};
use crate::state::tests::fixture;

fn open(cx: &mut TestAppContext) -> (Entity<Desk>, VisualTestContext, UnboundedReceiver<Up>) {
    let (up, rx) = tokio::sync::mpsc::unbounded_channel();
    // Every setting its default (prefix ctrl-a), never the person's own file.
    let config = qoop_tui::config::read_from(std::path::Path::new("/nonexistent/config.toml"));
    let window = cx.add_window(|window, cx| Desk::new(up, Latest::default(), &config, window, cx));
    let mut vcx = VisualTestContext::from_window(window.into(), cx);
    let desk = window.root(&mut vcx).expect("the window's view");
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            d.apply(Down::Connected, window, cx);
            d.model = fixture();
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

#[gpui::test]
fn home_lists_the_rows_under_their_spaces_in_rank_order(cx: &mut TestAppContext) {
    let (_desk, mut vcx, _rx) = open(cx);
    let y = |vcx: &mut VisualTestContext, what: &'static str| {
        vcx.debug_bounds(what)
            .unwrap_or_else(|| panic!("{what} is not drawn"))
            .origin
            .y
    };
    // acme-web: Needs you, then Done, then Working; api below it; the remote last.
    let order = [
        "space-1", "row-2", "row-3", "row-1", "space-2", "row-4", "row-5",
    ];
    let ys: Vec<_> = order.iter().map(|s| y(&mut vcx, s)).collect();
    assert!(ys.windows(2).all(|w| w[0] < w[1]), "{order:?} at {ys:?}");
    assert!(
        vcx.debug_bounds("row-2164260865").is_some(),
        "the remote row"
    );
    assert!(y(&mut vcx, "row-5") < y(&mut vcx, "row-2164260865"));
    assert!(
        vcx.debug_bounds("row-9").is_none(),
        "qass's own row is not an agent of the owner"
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
    vcx.simulate_keystrokes("ctrl-a i");
    assert!(desk.read_with(&vcx, |d, _| d.inbox));
    assert!(
        vcx.debug_bounds("inbox-row-2").is_some(),
        "the Needs you row is in the Inbox"
    );
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

/// Home shows a sleeping tab as a current daemon lists it, under its space, and a space where
/// everything sleeps.
#[gpui::test]
fn home_lists_sleeping_tabs_from_the_space_list(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    vcx.update(|window, cx| {
        desk.update(cx, |d, cx| {
            // The sleeping chip, so every row it holds fits the test window's height.
            d.chip = crate::state::Chip::Sleeping;
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
        })
    });
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("row-2164260866").is_some(),
        "the remote's sleeper"
    );
    assert!(
        vcx.debug_bounds("row-2164260867").is_none(),
        "its second pane"
    );
    assert!(
        vcx.debug_bounds("row-70").is_some(),
        "the space that sleeps whole"
    );
}

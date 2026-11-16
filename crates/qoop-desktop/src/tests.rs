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
            // Never the owner's own qass lessons.
            d.lessons_path = std::path::PathBuf::from("/nonexistent/qass/lessons.md");
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

/// Another agent turning Needs you while a session is open shows the corner note; the one on
/// screen never does; `ctrl-a x` dismisses it and `ctrl-a tab` opens its agent.
#[gpui::test]
fn the_corner_note_is_for_a_second_agent_only(cx: &mut TestAppContext) {
    let (desk, mut vcx, _rx) = open(cx);
    vcx.update(|window, cx| desk.update(cx, |d, cx| d.open(Page::Session(1), window, cx)));
    vcx.run_until_parked();
    let agents = |turned: &[u32]| {
        let mut rows = crate::state::tests::fixture().rows;
        for row in &mut rows {
            if turned.contains(&row.terminal) {
                row.status = Some("blocked".into());
            }
        }
        ControlResponse::Agents {
            id: "agents".into(),
            agents: rows,
            splits: Vec::new(),
            qass_checks: None,
        }
    };
    answer(&desk, &mut vcx, agents(&[1]));
    assert_eq!(
        desk.read_with(&vcx, |d, _| d.note),
        None,
        "the agent on screen"
    );
    answer(&desk, &mut vcx, agents(&[1, 3]));
    assert_eq!(desk.read_with(&vcx, |d, _| d.note), Some(3));
    assert!(vcx.debug_bounds("corner-note").is_some(), "drawn");
    vcx.simulate_keystrokes("ctrl-a x");
    assert_eq!(desk.read_with(&vcx, |d, _| d.note), None, "dismissed");
    assert!(vcx.debug_bounds("corner-note").is_none());
    answer(&desk, &mut vcx, agents(&[1]));
    answer(&desk, &mut vcx, agents(&[1, 4]));
    assert_eq!(desk.read_with(&vcx, |d, _| d.note), Some(4));
    vcx.simulate_keystrokes("ctrl-a tab");
    assert_eq!(desk.read_with(&vcx, |d, _| d.page), Page::Session(4));
    assert_eq!(desk.read_with(&vcx, |d, _| d.note), None);
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
    std::thread::sleep(crate::state::ARMING);
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
    assert_eq!(text, crate::state::EXPLAIN);
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
            d.open(Page::Session(1), window, cx);
            d.inbox = true;
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
            d.open(Page::Home, window, cx);
            d.apply(
                Down::Event(qoop_runtime::control::EventMessage::Focus { terminal: 9 }),
                window,
                cx,
            );
        })
    });
    assert_eq!(desk.read_with(&vcx, |d, _| d.page), Page::Qass);
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
            // The sleeping chip: what it lists is the sleeping tabs only.
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
            // As the link's pump does after each batch.
            cx.notify();
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

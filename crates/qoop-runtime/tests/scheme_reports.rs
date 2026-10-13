//! A program that asks to hear of the person's terminal turning light or dark (`CSI ? 2031 h`,
//! as qoop's own pages do) is told by the daemon when an interface says the host colours: the
//! first one after a daemon that started with none, and every later change of scheme. One test
//! in its own binary, because the host colours are the daemon's, process wide.

use std::time::Duration;

use qoop_runtime::control::ControlRequest;
use qoop_testkit::TestDaemon;

const PATIENCE: Duration = Duration::from_secs(10);

fn colours(id: &str, bg: [u8; 3]) -> ControlRequest {
    ControlRequest::HostColors {
        id: id.into(),
        fg: [128, 128, 128],
        bg,
    }
}

#[tokio::test]
async fn a_program_that_asked_hears_the_first_colours_and_each_change_of_scheme() {
    let d = TestDaemon::start().await;
    let stream = d.terminals().streams()[0];
    let mut c = d.attach("scheme-reports").await;
    // Raw, so each byte the program is given shows at once, an escape as `^[` (`cat -v`).
    c.input_to(
        stream,
        b"stty raw -echo; printf '\\033[?2031h'; echo read''y; cat -v\r",
    )
    .await;
    assert!(c.wait_for_text_on(stream, "ready", PATIENCE).await);
    // No interface has said yet: the first to say is news.
    c.control(&colours("light", [250, 250, 250]), PATIENCE)
        .await;
    assert!(
        c.wait_for_text_on(stream, "^[[?997;2n", PATIENCE).await,
        "the first colours were not told"
    );
    c.control(&colours("dark", [30, 30, 30]), PATIENCE).await;
    assert!(
        c.wait_for_text_on(stream, "^[[?997;2n^[[?997;1n", PATIENCE)
            .await,
        "a change to dark was not told, or told twice"
    );
}

//! `[[link_handlers]]`: a link a person ctrl+clicks in a pane, run by the first plugin handler
//! whose pattern it matches, as herdr runs it. The interface finds the link (the cell's `OSC 8`
//! target, else a web address in the row's text) and asks the host with `qoop.link`; the
//! answer says whether a plugin took it, and an untaken click goes on as a plain click.

use serde_json::{Value, json};

use super::host::{self, Host, Refused, param};
use super::install::{self, Home};

/// The handler a link goes to: the first of [`handlers`] whose pattern matches it, unanchored
/// as herdr's `is_match`.
fn handler_for(home: &Home, url: &str) -> Option<(String, String, String)> {
    handlers(home)
        .into_iter()
        .find_map(|(id, handler, action, pattern)| {
            pattern.is_match(url).then_some((id, handler, action))
        })
}

/// Whether any plugin agreed to here has a link handler for this platform: what the interface
/// asks before it holds a ctrl+click at all.
pub(super) fn any_handler(home: &Home) -> bool {
    !handlers(home).is_empty()
}

/// Every link handler that may run, in the order a link tries them: plugins agreed to by id;
/// each one's handlers for this platform in manifest order, whose action runs here too; with
/// its plugin, its id, its action and its pattern. A pattern that does not compile is passed
/// over.
fn handlers(home: &Home) -> Vec<(String, String, String, regex::Regex)> {
    let mut ids = install::installed(home);
    ids.sort();
    ids.into_iter()
        .filter_map(|id| Some((install::admitted(home, &id).ok()?.manifest.here(), id)))
        .flat_map(|(here, id)| {
            here.link_handlers
                .iter()
                .filter(|handler| here.actions.iter().any(|a| a.id == handler.action))
                .filter_map(|handler| {
                    let pattern = regex::Regex::new(&handler.pattern).ok()?;
                    Some((
                        id.clone(),
                        handler.id.clone(),
                        handler.action.clone(),
                        pattern,
                    ))
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

impl Host {
    /// `qoop.link {terminal, url}`: run the handler the link matches, with the clicked pane
    /// as its context and `link_click` as how it came; herdr's `{url, handled}` back. A
    /// handler that cannot start is herdr's `plugin_link_failed`. An ask past its `until_unix_ms`
    /// (the interface has stopped waiting) runs nothing and says `handled: false`.
    pub(super) fn link(&self, params: &Value) -> Result<Value, Refused> {
        let url = param(params, "url").unwrap_or_default().to_string();
        let terminal = params
            .get("terminal")
            .and_then(Value::as_u64)
            .and_then(|t| u32::try_from(t).ok());
        let Some((plugin, handler, action)) = handler_for(&self.home, &url) else {
            return Ok(json!({"type": "pane_link_activated", "url": url, "handled": false}));
        };
        let mut context = {
            let state = self.state.lock().unwrap();
            terminal
                .and_then(|t| state.now.rows.iter().find(|r| r.terminal == t))
                .map(|row| host::scope(&state.now, row.workspace, Some(row.terminal)).context)
                .unwrap_or_else(|| json!({}))
        };
        // Past the interface's wait the click has gone on as a plain one: nothing runs for it.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let until = params.get("until_unix_ms").and_then(Value::as_u64);
        if until.is_some_and(|until| now_ms > u128::from(until)) {
            return Ok(json!({"type": "pane_link_activated", "url": url, "handled": false}));
        }
        context["invocation_source"] = json!("link_click");
        context["clicked_url"] = json!(url);
        context["link_handler_id"] = json!(handler);
        self.invoke(&plugin, &action, Some(&context))
            .map_err(|(_, message)| ("plugin_link_failed", message))?;
        Ok(json!({"type": "pane_link_activated", "url": url, "handled": true}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plant(home: &Home, at: &std::path::Path, id: &str, body: &str) {
        let folder = at.join(id);
        std::fs::create_dir_all(&folder).expect("mkdir");
        let text = format!("{}{body}", super::super::manifest::head(id));
        std::fs::write(folder.join(super::super::manifest::FILE), text).expect("manifest");
        install::link_for_tests(home, &folder);
    }

    #[test]
    fn the_first_matching_handler_wins_by_plugin_id_then_manifest_order() {
        let dir = tempfile::tempdir().expect("dir");
        let home = Home::at(dir.path().join("data/plugins"), dir.path().join("config"));
        let at = dir.path().join("src");
        let plant = |id: &str, body: &str| plant(&home, &at, id, body);
        assert!(
            !any_handler(&home),
            "no plugin, no handler: a ctrl+click goes straight on"
        );
        let action =
            |id: &str| format!("[[actions]]\nid = \"{id}\"\ntitle = \"t\"\ncommand = [\"true\"]\n");
        let handler = |id: &str, pattern: &str, action: &str| {
            format!(
                "[[link_handlers]]\nid = \"{id}\"\ntitle = \"t\"\npattern = '{pattern}'\naction = \"{action}\"\n"
            )
        };
        plant(
            "b.second",
            &format!("{}{}", action("open"), handler("any", ".", "open")),
        );
        plant(
            "a.first",
            &format!(
                "{}{}{}{}",
                action("file"),
                handler("elsewhere", "^file:", "file").replace(
                    "[[link_handlers]]",
                    "[[link_handlers]]\nplatforms = [\"windows\"]"
                ),
                handler("files", "^file://", "file"),
                handler("later", "^file", "file"),
            ),
        );
        assert_eq!(
            handler_for(&home, "file:///tmp/a.rs"),
            Some(("a.first".into(), "files".into(), "file".into())),
            "a.first sorts first; one for another platform is passed over, the first of the rest wins"
        );
        assert_eq!(
            handler_for(&home, "https://example.com"),
            Some(("b.second".into(), "any".into(), "open".into())),
            "unanchored, as herdr matches"
        );
        assert_eq!(
            handler_for(&home, ""),
            None,
            "an empty link matches nothing"
        );
        assert!(any_handler(&home), "so a ctrl+click is held for them");
    }
}

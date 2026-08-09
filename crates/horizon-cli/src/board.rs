//! The `horizon board ...` subcommand family — calls `horizon-board`
//! directly, bypassing the control-plane socket entirely (the board is a
//! local file store, not a daemon-mediated resource).
//!
//! Intercepted in [`crate::run`] *before* [`crate::cli::parse`] because the
//! board's own flags (`--body`, `--after`, `--before`, `--top`, `--status`,
//! `--author`, `--as`) would be rejected by the global flag parser's
//! "unrecognized flag" guard.

use std::io::Write;

use horizon_board::{Item, ListResult, Position, Store, StoreError};

/// If `args[0]` is `"board"`, dispatches the board subcommand family and
/// returns `Some(exit_code)`. Returns `None` when this isn't a board
/// invocation, so [`crate::run`] falls through to the normal pipeline.
pub fn try_run(args: &[String], stdout: &mut impl Write, stderr: &mut impl Write) -> Option<u8> {
    if args.first().map(String::as_str) != Some("board") {
        return None;
    }
    let rest = &args[1..];
    Some(run_board(rest, stdout, stderr))
}

const BOARD_USAGE: &str = "\
Usage: horizon board <command> [options]

Commands:
  add <title> [--body <text>] [--parent <id>] [--after <id> | --before <id> | --top]
      Create a new item. Default position is the bottom of the queue.
  list [--status <s>] [--all]
      List items in rank order (closed items hidden by default; --all shows
      them). Always shows all existing statuses.
  show <id>
      Show all fields and comments for one item.
  comment <id> --author <author> <text>
      Add a comment to an item.
  set-status <id> <status>
      Set an item's status (free-form; recommended: proposed / ready /
      in-progress / review / done / blocked / archived). `done` and `archived`
      are hidden from the default `list` view; use `list --all` to see them.
  assign <id> <who>
      Assign an item (empty string to unassign).
  edit <id> [--title <t>] [--body <text>]
      Edit an item's title and/or body. At least one of --title/--body is
      required; fields not given are left unchanged.
  move <id> [--after <id> | --before <id> | --top]
      Re-rank an item within the queue.
  claim [--as <who>]
      Atomically claim the first ready+unassigned item: sets it to
      in-progress and assigns it to <who> (default: owner).
  watch [--since <seq>]
      Stream subscription pokes as NDJSON lines. Pipe to jq or other
      line-oriented tools. Runs until interrupted.
";

fn run_board(args: &[String], stdout: &mut impl Write, stderr: &mut impl Write) -> u8 {
    let mut iter = args.iter().peekable();
    let command = match iter.next() {
        Some(c) => c.as_str(),
        None => {
            let _ = writeln!(stdout, "{BOARD_USAGE}");
            return 0;
        }
    };

    // Collect positionals and flags for the subcommand.
    let mut positionals: Vec<String> = Vec::new();
    let mut body: Option<String> = None;
    let mut title: Option<String> = None;
    let mut parent: Option<String> = None;
    let mut after: Option<String> = None;
    let mut before: Option<String> = None;
    let mut top = false;
    let mut status: Option<String> = None;
    let mut author: Option<String> = None;
    let mut as_who: Option<String> = None;
    let mut since: Option<String> = None;
    let mut json = false;
    let mut all = false;

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--body" => match iter.next() {
                Some(v) => body = Some(v.clone()),
                None => {
                    let _ = writeln!(stderr, "error: --body requires a value");
                    return 2;
                }
            },
            "--title" => match iter.next() {
                Some(v) => title = Some(v.clone()),
                None => {
                    let _ = writeln!(stderr, "error: --title requires a value");
                    return 2;
                }
            },
            "--parent" => match iter.next() {
                Some(v) => parent = Some(v.clone()),
                None => {
                    let _ = writeln!(stderr, "error: --parent requires a value");
                    return 2;
                }
            },
            "--after" => match iter.next() {
                Some(v) => after = Some(v.clone()),
                None => {
                    let _ = writeln!(stderr, "error: --after requires a value");
                    return 2;
                }
            },
            "--before" => match iter.next() {
                Some(v) => before = Some(v.clone()),
                None => {
                    let _ = writeln!(stderr, "error: --before requires a value");
                    return 2;
                }
            },
            "--top" => top = true,
            "--status" => match iter.next() {
                Some(v) => status = Some(v.clone()),
                None => {
                    let _ = writeln!(stderr, "error: --status requires a value");
                    return 2;
                }
            },
            "--author" => match iter.next() {
                Some(v) => author = Some(v.clone()),
                None => {
                    let _ = writeln!(stderr, "error: --author requires a value");
                    return 2;
                }
            },
            "--as" => match iter.next() {
                Some(v) => as_who = Some(v.clone()),
                None => {
                    let _ = writeln!(stderr, "error: --as requires a value");
                    return 2;
                }
            },
            "--since" => match iter.next() {
                Some(v) => since = Some(v.clone()),
                None => {
                    let _ = writeln!(stderr, "error: --since requires a value");
                    return 2;
                }
            },
            "--json" => json = true,
            "--all" => all = true,
            s if s.starts_with("--") => {
                let _ = writeln!(stderr, "error: unrecognized flag: {s}");
                return 2;
            }
            _ => positionals.push(arg.clone()),
        }
    }

    let store = match Store::from_cwd() {
        Ok(s) => s,
        Err(e) => {
            let _ = writeln!(stderr, "error: {e}");
            return 1;
        }
    };

    // The board write methods are async (they make a remoc rtc round-trip to
    // `horizon-logd`). The CLI is a plain synchronous process, so this is the
    // one place that owns a tokio runtime — `block_on` here is safe because no
    // outer runtime exists. Reads (`list`/`show`) stay synchronous file folds.
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let _ = writeln!(stderr, "error: {e}");
            return 1;
        }
    };
    let result = runtime.block_on(dispatch(
        command,
        &positionals,
        &body,
        &title,
        &parent,
        &after,
        &before,
        top,
        &status,
        &author,
        &as_who,
        &since,
        json,
        all,
        &store,
        stdout,
    ));

    if let Err(e) = result {
        let _ = writeln!(stderr, "error: {e}");
        return 1;
    }
    0
}

#[allow(clippy::too_many_arguments)]
async fn dispatch(
    command: &str,
    positionals: &[String],
    body: &Option<String>,
    title: &Option<String>,
    parent: &Option<String>,
    after: &Option<String>,
    before: &Option<String>,
    top: bool,
    status: &Option<String>,
    author: &Option<String>,
    as_who: &Option<String>,
    since: &Option<String>,
    json: bool,
    all: bool,
    store: &Store,
    stdout: &mut impl Write,
) -> Result<(), String> {
    match command {
        "add" => {
            let title = positionals
                .first()
                .ok_or_else(|| "add requires a <title>".to_string())?;
            let parent_id = parent
                .as_deref()
                .map(parse_id)
                .transpose()
                .map_err(|e| e.to_string())?;
            let pos = resolve_position(top, after.as_deref(), before.as_deref())?;
            let item = store
                .add(title, body.as_deref().unwrap_or(""), parent_id, pos)
                .await
                .map_err(|e| e.to_string())?;
            if json {
                let _ = writeln!(
                    stdout,
                    "{}",
                    serde_json::to_string(&item_json(&item)).unwrap()
                );
            } else {
                print_item_brief(stdout, &item);
            }
            Ok(())
        }
        "list" => {
            let result = store
                .list(status.as_deref(), all)
                .map_err(|e| e.to_string())?;
            if json {
                print_list_json(stdout, &result);
            } else {
                print_list_human(stdout, &result);
            }
            Ok(())
        }
        "show" => {
            let id = parse_id(
                positionals
                    .first()
                    .ok_or_else(|| "show requires an <id>".to_string())?,
            )
            .map_err(|e| e.to_string())?;
            match store.show(id).map_err(|e| e.to_string())? {
                Some(item) => {
                    if json {
                        let _ = writeln!(
                            stdout,
                            "{}",
                            serde_json::to_string(&item_json(&item)).unwrap()
                        );
                    } else {
                        print_item_full(stdout, &item);
                    }
                    Ok(())
                }
                None => Err(format!("item {id} not found")),
            }
        }
        "comment" => {
            let id = parse_id(
                positionals
                    .first()
                    .ok_or_else(|| "comment requires an <id>".to_string())?,
            )
            .map_err(|e| e.to_string())?;
            let text = positionals
                .get(1)
                .ok_or_else(|| "comment requires <text>".to_string())?;
            let author = author
                .as_deref()
                .ok_or_else(|| "comment requires --author".to_string())?;
            store
                .comment(id, author, text)
                .await
                .map_err(|e| e.to_string())?;
            let _ = writeln!(stdout, "Comment added to item {id}");
            Ok(())
        }
        "edit" => {
            let id = parse_id(
                positionals
                    .first()
                    .ok_or_else(|| "edit requires an <id>".to_string())?,
            )
            .map_err(|e| e.to_string())?;
            if title.is_none() && body.is_none() {
                return Err("edit requires at least one of --title / --body".to_string());
            }
            store
                .edit(id, title.clone(), body.clone())
                .await
                .map_err(|e| e.to_string())?;
            if let Some(t) = title {
                let _ = writeln!(stdout, "Item {id} -> title: {t}");
            }
            if let Some(b) = body {
                let _ = writeln!(stdout, "Item {id} -> body updated ({} chars)", b.len());
            }
            Ok(())
        }
        "set-status" => {
            let id = parse_id(
                positionals
                    .first()
                    .ok_or_else(|| "set-status requires an <id>".to_string())?,
            )
            .map_err(|e| e.to_string())?;
            let s = positionals
                .get(1)
                .ok_or_else(|| "set-status requires a <status>".to_string())?;
            store.set_status(id, s).await.map_err(|e| e.to_string())?;
            let _ = writeln!(stdout, "Item {id} -> status: {s}");
            Ok(())
        }
        "assign" => {
            let id = parse_id(
                positionals
                    .first()
                    .ok_or_else(|| "assign requires an <id>".to_string())?,
            )
            .map_err(|e| e.to_string())?;
            let who = positionals
                .get(1)
                .ok_or_else(|| "assign requires a <who>".to_string())?;
            store.assign(id, who).await.map_err(|e| e.to_string())?;
            let _ = writeln!(stdout, "Item {id} -> assignee: {who}");
            Ok(())
        }
        "move" => {
            let id = parse_id(
                positionals
                    .first()
                    .ok_or_else(|| "move requires an <id>".to_string())?,
            )
            .map_err(|e| e.to_string())?;
            let pos = resolve_position(top, after.as_deref(), before.as_deref())?;
            let rank = store.move_item(id, pos).await.map_err(|e| e.to_string())?;
            let _ = writeln!(stdout, "Item {id} -> rank: {rank}");
            Ok(())
        }
        "claim" => {
            let who = as_who.as_deref().unwrap_or("owner");
            match store.claim(who).await.map_err(|e| e.to_string())? {
                Some(item) => {
                    if json {
                        let _ = writeln!(
                            stdout,
                            "{}",
                            serde_json::to_string(&item_json(&item)).unwrap()
                        );
                    } else {
                        print_item_brief(stdout, &item);
                    }
                    Ok(())
                }
                None => {
                    let _ = writeln!(stdout, "No ready+unassigned items to claim");
                    Ok(())
                }
            }
        }
        "watch" => {
            let since_seq = since
                .as_deref()
                .map(|s| {
                    s.parse::<u64>()
                        .map_err(|_| format!("--since must be a number, got: {s}"))
                })
                .transpose()?;
            let mut stream = store
                .subscribe(since_seq)
                .await
                .map_err(|e| e.to_string())?;
            // Read NDJSON lines and pipe them to stdout verbatim. Each
            // line is one {"log":"board","seq":N} — the cursor reply
            // first, then one poke per appended event. Runs until logd
            // closes the connection (drain/shutdown) or the pipe breaks.
            while let Some(line) = stream.next_line().await.map_err(|e| e.to_string())? {
                if writeln!(stdout, "{line}").is_err() {
                    break; // broken pipe (e.g. jq exited)
                }
                let _ = stdout.flush();
            }
            Ok(())
        }
        other => Err(format!("unknown board command: {other}\n\n{BOARD_USAGE}")),
    }
}

fn parse_id(s: &str) -> Result<u64, StoreError> {
    s.parse::<u64>().map_err(|_| StoreError::ItemNotFound(0))
}

fn resolve_position(
    top: bool,
    after: Option<&str>,
    before: Option<&str>,
) -> Result<Position, String> {
    let count = [top, after.is_some(), before.is_some()]
        .iter()
        .filter(|&&b| b)
        .count();
    if count > 1 {
        return Err("specify at most one of --top, --after, --before".to_string());
    }
    if top {
        Ok(Position::Top)
    } else if let Some(id) = after {
        Ok(Position::After(parse_id(id).map_err(|e| e.to_string())?))
    } else if let Some(id) = before {
        Ok(Position::Before(parse_id(id).map_err(|e| e.to_string())?))
    } else {
        Ok(Position::Bottom)
    }
}

// -- output formatting ------------------------------------------------

fn print_item_brief(stdout: &mut impl Write, item: &Item) {
    let status_display = if item.status.is_empty() {
        "—"
    } else {
        &item.status
    };
    let _ = writeln!(
        stdout,
        "#{:<3} [{:<12}] {}",
        item.id, status_display, item.title
    );
    if !item.assignee.is_empty() {
        let _ = writeln!(stdout, "     assigned: {}", item.assignee);
    }
}

fn print_item_full(stdout: &mut impl Write, item: &Item) {
    let _ = writeln!(stdout, "Item #{}", item.id);
    let _ = writeln!(stdout, "  title:    {}", item.title);
    let _ = writeln!(
        stdout,
        "  status:   {}",
        if item.status.is_empty() {
            "—"
        } else {
            &item.status
        }
    );
    let _ = writeln!(stdout, "  rank:     {}", item.rank);
    let _ = writeln!(
        stdout,
        "  assignee: {}",
        if item.assignee.is_empty() {
            "—"
        } else {
            &item.assignee
        }
    );
    if let Some(p) = item.parent {
        let _ = writeln!(stdout, "  parent:   #{p}");
    }
    if !item.depends_on.is_empty() {
        let _ = writeln!(stdout, "  depends:  {:?}", item.depends_on);
    }
    if !item.links.is_empty() {
        let _ = writeln!(stdout, "  links:    {:?}", item.links);
    }
    if !item.body.is_empty() {
        let _ = writeln!(stdout, "  body:");
        for line in item.body.lines() {
            let _ = writeln!(stdout, "    {line}");
        }
    }
    if !item.comments.is_empty() {
        let _ = writeln!(stdout, "  comments:");
        for c in &item.comments {
            let _ = writeln!(stdout, "    [{}] {}: {}", c.at, c.author, c.text);
        }
    }
}

fn print_list_human(stdout: &mut impl Write, result: &ListResult) {
    if result.items.is_empty() {
        let _ = writeln!(stdout, "(no items)");
    } else {
        for item in &result.items {
            print_item_brief(stdout, item);
        }
    }
    if !result.statuses.is_empty() {
        let _ = writeln!(stdout, "\nStatuses: {}", result.statuses.join(", "));
    }
    if let Some(skipped) = &result.skipped {
        let _ = writeln!(stdout, "\nWarning: {skipped}");
    }
}

fn print_list_json(stdout: &mut impl Write, result: &ListResult) {
    let items: Vec<_> = result.items.iter().map(item_json).collect();
    let body = serde_json::json!({
        "items": items,
        "statuses": result.statuses,
        "skipped": result.skipped,
    });
    let _ = writeln!(stdout, "{}", serde_json::to_string_pretty(&body).unwrap());
}

fn item_json(item: &Item) -> serde_json::Value {
    serde_json::json!({
        "id": item.id,
        "title": item.title,
        "body": item.body,
        "status": item.status,
        "rank": item.rank,
        "assignee": item.assignee,
        "parent": item.parent,
        "depends_on": item.depends_on,
        "links": item.links,
        "comments": item.comments,
    })
}

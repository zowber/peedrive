//! What GNOME Files shows for each item, and the selection changes its menu makes.
use crate::{local, model::*};
use std::path::Path;

fn within(path: &str, parent: &str) -> bool {
    parent.is_empty() || path == parent || path.starts_with(&format!("{parent}/"))
}

/// `synced`, `syncing`, `attention` or `paused`; `ignored` for excluded items and `none`
/// for anything else Peedrive does not sync.
/// `synced(mapping, path)` says whether the item has a last-synced baseline.
pub fn file_state(
    status: &Status,
    path: &Path,
    synced: &dyn Fn(&str, &str) -> bool,
) -> &'static str {
    let Some(ms) = status
        .mappings
        .iter()
        .find(|m| path.starts_with(&m.mapping.local))
    else {
        return "none";
    };
    let m = &ms.mapping;
    let waiting = if m.paused { "paused" } else { "syncing" };
    let Some(rel) = path.strip_prefix(&m.local).ok().and_then(|p| p.to_str()) else {
        return "none";
    };
    if rel.is_empty() {
        return match ms.state.as_str() {
            "Up to date" => "synced",
            "Needs attention" | "Review changes" => "attention",
            _ => waiting,
        };
    }
    let Ok(ignores) = local::ignores(&m.ignores) else {
        return "none";
    };
    if local::ignored(rel, &ignores) {
        return "ignored";
    }
    if !local::selected(m, rel, true) {
        return "none";
    }
    if status
        .conflicts
        .iter()
        .any(|c| c.mapping == m.id && within(&c.copy, rel))
    {
        return "attention";
    }
    let mut pending = false;
    for op in ms.plan.iter().flat_map(|p| &p.operations) {
        if matches!(op.action, Action::Record | Action::Forget)
            || !(within(&op.path, rel) || op.from.as_deref().is_some_and(|f| within(f, rel)))
        {
            continue;
        }
        let held = ms.state == "Review changes"
            && matches!(op.action, Action::TrashLocal | Action::TrashRemote);
        if op.action == Action::Conflict || held {
            return "attention";
        }
        pending = true;
    }
    if pending
        || status
            .transfers
            .iter()
            .any(|t| t["path"].as_str().is_some_and(|p| within(p, rel)))
    {
        return waiting;
    }
    if synced(&m.id, rel) {
        "synced"
    } else {
        waiting
    }
}

/// The selection after adding `path`. None already syncs everything.
pub fn select(selection: Option<Vec<String>>, path: &str) -> Option<Vec<String>> {
    let mut paths = selection?;
    if !paths.iter().any(|s| within(path, s)) {
        paths.retain(|s| !within(s, path));
        paths.push(path.into());
    }
    Some(paths)
}

/// The selection after removing `path`. A selected ancestor, or everything, is replaced by
/// its other items level by level, so the rest keeps syncing. `children` lists a folder's
/// items as relative paths; "" is the top level.
pub fn deselect(
    selection: Option<Vec<String>>,
    path: &str,
    children: &mut dyn FnMut(&str) -> Vec<String>,
) -> Vec<String> {
    let mut result = Vec::new();
    for s in selection.unwrap_or_else(|| vec![String::new()]) {
        if within(&s, path) {
            continue;
        }
        if !within(path, &s) {
            result.push(s);
            continue;
        }
        let mut dir = s;
        for part in path[dir.len()..].trim_start_matches('/').split('/') {
            let next = if dir.is_empty() {
                part.to_string()
            } else {
                format!("{dir}/{part}")
            };
            result.extend(children(&dir).into_iter().filter(|c| *c != next));
            dir = next;
        }
    }
    result.sort();
    result.dedup();
    result
}

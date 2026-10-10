use crate::model::*;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

fn changed(now: Option<&Entry>, then: &Entry) -> bool {
    now.is_none_or(|e| e.kind != then.kind || e.version != then.version || e.uid != then.uid)
}
fn same_content(a: &Entry, b: &Entry) -> bool {
    a.kind == b.kind
        && (a.kind == Kind::Folder || (a.hash.is_some() && a.hash == b.hash && a.size == b.size))
}
fn within(path: &str, parent: &str) -> bool {
    path == parent || path.starts_with(&format!("{parent}/"))
}

/// True if `tree` holds `path` or anything below it.
fn occupied<V>(tree: &BTreeMap<String, V>, path: &str) -> bool {
    tree.range(path.to_string()..)
        .take_while(|(p, _)| p.starts_with(path))
        .any(|(p, _)| within(p, path))
}
/// Rename the subtree at `from` to `to`.
fn rebase<V>(tree: &mut BTreeMap<String, V>, from: &str, to: &str) {
    let moved: Vec<String> = tree
        .range(from.to_string()..)
        .take_while(|(p, _)| p.starts_with(from))
        .filter(|(p, _)| within(p, from))
        .map(|(p, _)| p.clone())
        .collect();
    for p in moved {
        let value = tree.remove(&p).unwrap();
        tree.insert(format!("{to}{}", &p[from.len()..]), value);
    }
}

/// Items moved or renamed on one side since the last sync, as (from, to, moved locally).
/// Drive keeps a node's uid across moves. Locally the inode survives a move, and a file must
/// also keep its content, so a reused inode is never mistaken for a move; a file copied and
/// deleted elsewhere is matched by content when exactly one deletion and one creation share it.
/// Moves inside another move's source or destination are left to ordinary create and delete.
fn moves(local: &Tree, remote: &Tree, history: &History) -> Vec<(String, String, bool)> {
    let mut inodes: HashMap<&str, Vec<&String>> = HashMap::new();
    let mut hashes: HashMap<&str, Vec<&String>> = HashMap::new();
    let mut nodes: HashMap<&str, Vec<&String>> = HashMap::new();
    for (p, e) in local {
        if !history.contains_key(p) && !remote.contains_key(p) {
            inodes.entry(&e.uid).or_default().push(p);
            if let Some(h) = e.hash.as_deref().filter(|_| e.size > 0) {
                hashes.entry(h).or_default().push(p);
            }
        }
    }
    for (p, e) in remote {
        if !history.contains_key(p) && !local.contains_key(p) {
            nodes.entry(&e.uid).or_default().push(p);
        }
    }
    let mut deleted: HashMap<&str, usize> = HashMap::new();
    for (p, b) in history {
        if let (None, Some(h)) = (local.get(p), &b.local.hash) {
            *deleted.entry(h).or_default() += 1;
        }
    }
    let unique = |v: Option<&Vec<&String>>| match v.map(Vec::as_slice) {
        Some([only]) => Some((*only).clone()),
        _ => None,
    };
    let mut found: Vec<(String, String, bool)> = Vec::new();
    for (p, b) in history {
        let candidate = match (local.get(p), remote.get(p)) {
            (None, Some(r)) if r.uid == b.remote.uid && r.kind == b.remote.kind => {
                let fits = |q: &String| {
                    let e = &local[q];
                    e.kind == b.local.kind && (e.kind == Kind::Folder || same_content(e, &b.local))
                };
                let by_content = || {
                    let h = b.local.hash.as_deref()?;
                    (deleted.get(h) == Some(&1)).then(|| unique(hashes.get(h)))?
                };
                unique(inodes.get(b.local.uid.as_str()))
                    .filter(fits)
                    .or_else(|| by_content().filter(fits))
                    .map(|q| (q, true))
            }
            (Some(l), None) if l.kind == b.local.kind => unique(nodes.get(b.remote.uid.as_str()))
                .filter(|q| remote[q].kind == b.remote.kind)
                .map(|q| (q, false)),
            _ => None,
        };
        if let Some((q, locally)) = candidate
            && !occupied(history, &q)
        {
            found.push((p.clone(), q, locally));
        }
    }
    // History is sorted, so a moved folder is accepted before the items it carried.
    let mut accepted: Vec<(String, String, bool)> = Vec::new();
    let mut used = HashSet::new();
    for (p, q, locally) in found {
        let overlaps = |a: &str, b: &str| within(a, b) || within(b, a);
        if used.insert(q.clone())
            && !accepted.iter().any(|(p2, q2, _)| {
                overlaps(&p, p2) || overlaps(&p, q2) || overlaps(&q, p2) || overlaps(&q, q2)
            })
        {
            accepted.push((p, q, locally));
        }
    }
    accepted
}

/// Pure three-way reconciliation. No I/O, timestamps, or implicit winner selection.
/// Moves are applied to the other side's view and the history first, so whatever else
/// changed inside a moved item is reconciled at its new path.
pub fn reconcile(local: &Tree, remote: &Tree, history: &History) -> Plan {
    let found = moves(local, remote, history);
    if found.is_empty() {
        return reconcile_paths(local, remote, history, Vec::new());
    }
    let (mut l, mut r, mut h) = (local.clone(), remote.clone(), history.clone());
    let mut moved = Vec::new();
    for (from, to, locally) in found {
        moved.push(Operation {
            action: if locally {
                Action::MoveRemote
            } else {
                Action::MoveLocal
            },
            local: local.get(if locally { &to } else { &from }).cloned(),
            remote: remote.get(if locally { &from } else { &to }).cloned(),
            reason: if locally {
                "Moved on this computer"
            } else {
                "Moved in Drive"
            }
            .into(),
            path: to.clone(),
            from: Some(from.clone()),
        });
        rebase(&mut h, &from, &to);
        rebase(if locally { &mut r } else { &mut l }, &from, &to);
    }
    reconcile_paths(&l, &r, &h, moved)
}

fn reconcile_paths(local: &Tree, remote: &Tree, history: &History, moved: Vec<Operation>) -> Plan {
    let paths: BTreeSet<_> = local
        .keys()
        .chain(remote.keys())
        .chain(history.keys())
        .cloned()
        .collect();
    // If a removed directory contains edits on the surviving side, preserve its entire subtree.
    let preserved: Vec<_> = history
        .iter()
        .filter_map(|(p, b)| {
            if b.local.kind != Kind::Folder {
                return None;
            }
            let survivor = if !local.contains_key(p) && remote.contains_key(p) {
                remote
            } else if !remote.contains_key(p) && local.contains_key(p) {
                local
            } else {
                return None;
            };
            let remote_survives = std::ptr::eq(survivor, remote);
            let dirty = survivor.iter().any(|(q, e)| {
                within(q, p)
                    && history.get(q).is_none_or(|old| {
                        changed(
                            Some(e),
                            if remote_survives {
                                &old.remote
                            } else {
                                &old.local
                            },
                        )
                    })
            });
            dirty.then_some(p.clone())
        })
        .collect();
    let mut plan = Plan {
        operations: moved,
        ..Plan::default()
    };
    for path in paths {
        let l = local.get(&path);
        let r = remote.get(&path);
        let preserved_tree = preserved.iter().any(|p| within(&path, p));
        let base = if preserved_tree {
            None
        } else {
            history.get(&path)
        };
        let (action, reason) = match (l, r, base) {
            (None, None, _) => (Action::Forget, "Removed on both sides"),
            (Some(a), Some(b), _) if same_content(a, b) => (Action::Record, "Content agrees"),
            (Some(a), None, None) => (
                if a.kind == Kind::Folder {
                    Action::MkdirRemote
                } else {
                    Action::Upload
                },
                "New local item",
            ),
            (None, Some(b), None) => (
                if b.kind == Kind::Folder {
                    Action::MkdirLocal
                } else {
                    Action::Download
                },
                "New remote item",
            ),
            (Some(_), Some(_), None) => (Action::Conflict, "Different content at the same path"),
            (l, r, Some(b)) => {
                let lc = changed(l, &b.local);
                let rc = changed(r, &b.remote);
                match (lc, rc, l, r) {
                    (false, false, _, _) => (Action::Record, "Unchanged"),
                    (true, false, None, _) => (Action::TrashRemote, "Deleted locally"),
                    (false, true, _, None) => (Action::TrashLocal, "Deleted remotely"),
                    (true, false, Some(a), _) if a.kind == b.local.kind => {
                        (Action::Upload, "Edited locally")
                    }
                    (false, true, _, Some(a)) if a.kind == b.remote.kind => {
                        (Action::Download, "Edited remotely")
                    }
                    (true, true, None, Some(a)) => (
                        if a.kind == Kind::Folder {
                            Action::MkdirLocal
                        } else {
                            Action::Download
                        },
                        "Remote edit preserved after local deletion",
                    ),
                    (true, true, Some(a), None) => (
                        if a.kind == Kind::Folder {
                            Action::MkdirRemote
                        } else {
                            Action::Upload
                        },
                        "Local edit preserved after remote deletion",
                    ),
                    _ => (Action::Conflict, "Both sides changed"),
                }
            }
        };
        if matches!(action, Action::TrashLocal | Action::TrashRemote) {
            plan.deletions += 1;
        }
        if action == Action::Conflict {
            plan.conflicts += 1;
        }
        plan.operations.push(Operation {
            path,
            action,
            local: l.cloned(),
            remote: r.cloned(),
            reason: reason.into(),
            from: None,
        });
    }
    // Parents before creation, children before deletion. Conflicts are processed before their
    // descendants, and a move before anything else at its destination. At each depth, folder
    // changes come first so the transfers after them can run together.
    plan.operations.sort_by_key(|o| {
        let depth = o.path.matches('/').count();
        let transfer = matches!(
            o.action,
            Action::Upload | Action::Download | Action::Conflict
        );
        let after_move = o.from.is_none();
        if matches!(o.action, Action::TrashLocal | Action::TrashRemote) {
            (2, usize::MAX - depth, false, o.path.clone(), after_move)
        } else if o.action == Action::Forget {
            (3, depth, false, o.path.clone(), after_move)
        } else {
            (1, depth, transfer, o.path.clone(), after_move)
        }
    });
    plan
}

//! Object-graph reachability GC (P3.2 D-RD4, I-RD4).
//!
//! Redaction deletes objects. Deleting an object from `Document.objects` is necessary but
//! not sufficient: an object that was only ever referenced *by* a deleted object is still
//! in the map, still written to disk, and still extractable with any recovery tool. That
//! survivor is the exact failure the feature exists to prevent, so the rewrite is followed
//! by a reachability sweep from the trailer and everything unreachable is dropped.
//!
//! The sweep is the *only* thing that decides what survives. It is deliberately not
//! incremental and takes no hints: a caller cannot ask for an object to be kept, because
//! "keep this one, it is probably fine" is how a leak gets reintroduced.
//!
//! **What lopdf already guarantees, measured rather than assumed.** `save_to` builds a
//! fresh `Xref::new(max_id + 1)` and inserts an entry only while serializing each object it
//! actually writes (`write_indirect_object`). The on-disk xref is therefore derived from
//! `Document.objects`, never merged from the parsed table, so dropping an object frees its
//! entry automatically. The half of I-RD4 that this leaves to us is the other half: that no
//! *surviving* object still points at a dropped one. [`dangling_references`] is that check,
//! and it is what makes the GC falsifiable rather than merely present.

use std::collections::BTreeSet;

use lopdf::{Dictionary, Document, Object, ObjectId};

use crate::model::AppError;

/// Drops every object not reachable from the trailer, in place (D-RD4).
///
/// Returns the ids removed, which is what tests assert on — a test that only checked the
/// file still opens would pass on a GC that did nothing at all.
pub fn sweep_unreachable(document: &mut Document) -> Result<Vec<ObjectId>, AppError> {
    let reachable = reachable_ids(document);

    let mut removed: Vec<ObjectId> = document
        .objects
        .keys()
        .copied()
        .filter(|id| !reachable.contains(id))
        .collect();

    if removed.is_empty() {
        return Ok(removed);
    }

    removed.sort();
    for id in &removed {
        document.objects.remove(id);
    }

    // A dropped object may have been the highest-numbered one. `max_id` drives the xref
    // `/Size`, and a `Size` larger than the highest id is legal, but keeping `max_id` equal
    // to the real maximum is what makes `dangling_references` and the xref agree exactly.
    document.max_id = document
        .objects
        .keys()
        .map(|(id, _)| *id)
        .max()
        .unwrap_or(0);
    document.trailer.set("Size", i64::from(document.max_id + 1));

    Ok(removed)
}

/// The set of object ids reachable from the trailer (D-RD4).
///
/// Roots are the trailer's own values — `/Root`, `/Info`, `/Encrypt`, anything else a
/// producer put there — because the trailer is what a reader starts from. Reference cycles
/// are why this is a worklist with a visited set and not recursion: a self-referencing
/// object is legal and common, and a recursive walk would not terminate.
pub fn reachable_ids(document: &Document) -> BTreeSet<ObjectId> {
    let mut visited = BTreeSet::new();
    let mut stack: Vec<ObjectId> = Vec::new();

    collect_refs_from_dict(&document.trailer, &mut stack);

    while let Some(id) = stack.pop() {
        if !visited.insert(id) {
            continue;
        }
        // A reference to a missing object is not a traversal error here: `dangling_references`
        // reports it, and the sweep must not fail on a document it is supposed to repair.
        if let Some(object) = document.objects.get(&id) {
            collect_refs(object, &mut stack);
        }
    }

    visited
}

/// Every reference in every reachable object that fails to resolve (I-RD4).
///
/// Reports `(referrer, missing)` pairs so a failure names both ends; an empty result is the
/// invariant. Only *reachable* objects are checked, matching the invariant exactly — an
/// orphan pointing at nothing is not a defect, it is garbage the sweep is about to remove.
pub fn dangling_references(document: &Document) -> Vec<(ObjectId, ObjectId)> {
    let reachable = reachable_ids(document);
    let mut dangling = Vec::new();

    for id in &reachable {
        let Some(object) = document.objects.get(id) else {
            // The referrer itself is missing; that is reported against its own referrers
            // below, and naming it here as well would double-count.
            continue;
        };
        let mut refs = Vec::new();
        collect_refs(object, &mut refs);
        for target in refs {
            if !document.objects.contains_key(&target) {
                dangling.push((*id, target));
            }
        }
    }

    dangling
}

fn collect_refs(object: &Object, out: &mut Vec<ObjectId>) {
    match object {
        Object::Reference(id) => out.push(*id),
        Object::Array(items) => items.iter().for_each(|item| collect_refs(item, out)),
        Object::Dictionary(dict) => collect_refs_from_dict(dict, out),
        Object::Stream(stream) => collect_refs_from_dict(&stream.dict, out),
        _ => {}
    }
}

fn collect_refs_from_dict(dict: &Dictionary, out: &mut Vec<ObjectId>) {
    for (_, value) in dict.iter() {
        collect_refs(value, out);
    }
}

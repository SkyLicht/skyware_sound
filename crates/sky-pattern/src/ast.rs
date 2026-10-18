// SPDX-License-Identifier: GPL-3.0-or-later
//! The pattern tree that the parser builds and queries walk.
//!
//! Every node describes an infinite timeline in its own local time, where one
//! unit is one cycle. Nodes are shared through `Arc`, so `x!100` stores one
//! copy of `x`. Times inside the tree use 128-bit rationals, and each node
//! records the [`Bounds`] that prove queries on it can't overflow.

use std::sync::Arc;

use num_rational::Ratio;

use crate::bounds::Bounds;
use crate::controls::Controls;
use crate::ops::{Arg, Op};
use crate::signal::Signal;
use crate::{Span, Value};

/// Rational time inside the tree and during queries.
pub(crate) type Q = Ratio<i128>;

#[derive(Debug)]
pub(crate) struct Node {
    pub kind: Kind,
    pub bounds: Bounds,
    /// About how many nodes a query of one cycle visits, counting shared
    /// nodes each time they are reached. Code can build small trees that
    /// are costly to walk, such as many `sometimes` in a row.
    pub cost: u64,
}

#[derive(Debug)]
pub(crate) enum Kind {
    /// Plays nothing.
    Silence,
    /// One event per cycle, filling the cycle.
    Atom {
        value: Value,
        span: Span,
        controls: Controls,
    },
    /// Children squeezed into fixed slots of every cycle. Cycle `c` of a child
    /// plays inside its slot of cycle `c`.
    Seq(Vec<Slot>),
    /// Children playing at the same time.
    Stack(Vec<Arc<Node>>),
    /// The child sped up by `factor` (below 1 slows it down).
    Fast { factor: Q, node: Arc<Node> },
    /// One child per cycle, picked at random.
    Choice { salt: u64, alts: Vec<Arc<Node>> },
    /// The child with each event dropped with probability `prob`, or with
    /// `invert` only those events kept: the same salt splits a pattern
    /// into two halves that never overlap.
    Degrade {
        salt: u64,
        prob: f64,
        invert: bool,
        node: Arc<Node>,
    },
    /// The child moved later by `by` cycles (earlier when negative).
    Shift { by: Q, node: Arc<Node> },
    /// Each cycle of the child played backwards.
    Rev(Arc<Node>),
    /// Cycle `c` of the child starts `(c mod n) / n` of a cycle in, or with
    /// `back` that much earlier.
    Iter {
        n: i128,
        back: bool,
        node: Arc<Node>,
    },
    /// Cycle `c` plays `nodes[c mod nodes.len()]`.
    Cycles(Vec<Arc<Node>>),
    /// Per cycle, `yes` with probability `prob`, otherwise `no`.
    CycleChance {
        salt: u64,
        prob: f64,
        yes: Arc<Node>,
        no: Arc<Node>,
    },
    /// Each event of the child split into `n` equal events.
    Ply { n: i128, node: Arc<Node> },
    /// Strudel's `struct`: an event at each event of `mask`, with the
    /// values of `node` sounding at that moment.
    Struct { mask: Arc<Node>, node: Arc<Node> },
    /// The events of `node` that start while an event of `mask` sounds.
    Mask { mask: Arc<Node>, node: Arc<Node> },
    /// Each event of `node` changed by `op` with the argument's value at
    /// the event's start.
    Set { node: Arc<Node>, op: Op, arg: Arg },
    /// A function with a patterned argument, such as `fast("<1 2>")`:
    /// while `arg` plays a value, the events of the matching case play.
    Switch {
        arg: Arc<Node>,
        cases: Vec<(Value, Arc<Node>)>,
    },
    /// `n` steps per cycle, each the value of `signal` at its start.
    Segment { n: i128, signal: Signal, span: Span },
    /// Events in the second half of each `1/n` of a cycle moved later by
    /// `by` cycles.
    Swing { n: i128, by: Q, node: Arc<Node> },
}

/// Part of a cycle, as fractions of the cycle: `[start, end)` with `len = end - start`.
#[derive(Debug)]
pub(crate) struct Slot {
    pub start: Q,
    pub end: Q,
    pub len: Q,
    pub node: Arc<Node>,
}

fn new(kind: Kind, bounds: Bounds) -> Arc<Node> {
    let mut node = Node {
        kind,
        bounds,
        cost: 0,
    };
    let children = node.children().into_iter().map(|n| n.cost);
    node.cost = match &node.kind {
        // One alternative plays at a time.
        Kind::Choice { .. } | Kind::Cycles(_) | Kind::CycleChance { .. } => {
            children.max().unwrap_or(0)
        }
        Kind::Switch { arg, cases } => cases
            .iter()
            .map(|(_, n)| n.cost)
            .max()
            .unwrap_or(0)
            .saturating_add(arg.cost),
        _ => children.fold(0, u64::saturating_add),
    }
    .saturating_add(1);
    Arc::new(node)
}

/// The children's bounds combined with [`Bounds::max`] (one of them plays) or
/// [`Bounds::both`] (all of them play).
fn combine(nodes: &[Arc<Node>], op: fn(Bounds, Bounds) -> Bounds) -> Bounds {
    nodes.iter().map(|n| n.bounds).fold(Bounds::SILENT, op)
}

impl Node {
    /// Pushes every atom under this node. A node shared by several places,
    /// as `x!100` shares `x`, is visited once.
    pub fn atoms<'a>(
        &'a self,
        seen: &mut std::collections::HashSet<*const Node>,
        out: &mut Vec<(&'a Value, Span)>,
    ) {
        if !seen.insert(self as *const Node) {
            return;
        }
        match &self.kind {
            Kind::Silence | Kind::Segment { .. } => {}
            Kind::Atom { value, span, .. } => out.push((value, *span)),
            Kind::Seq(slots) => slots.iter().for_each(|s| s.node.atoms(seen, out)),
            Kind::Stack(nodes) | Kind::Choice { alts: nodes, .. } | Kind::Cycles(nodes) => {
                nodes.iter().for_each(|n| n.atoms(seen, out));
            }
            Kind::Fast { node, .. }
            | Kind::Degrade { node, .. }
            | Kind::Shift { node, .. }
            | Kind::Rev(node)
            | Kind::Iter { node, .. }
            | Kind::Ply { node, .. }
            | Kind::Swing { node, .. } => node.atoms(seen, out),
            Kind::CycleChance { yes, no, .. } => {
                yes.atoms(seen, out);
                no.atoms(seen, out);
            }
            Kind::Struct { mask, node } | Kind::Mask { mask, node } => {
                mask.atoms(seen, out);
                node.atoms(seen, out);
            }
            Kind::Set { node, arg, .. } => {
                node.atoms(seen, out);
                if let Arg::Pattern(arg) = arg {
                    arg.atoms(seen, out);
                }
            }
            Kind::Switch { arg, cases } => {
                arg.atoms(seen, out);
                cases.iter().for_each(|(_, n)| n.atoms(seen, out));
            }
        }
    }

    /// The children of this node, for walks over the whole tree.
    pub fn children(&self) -> Vec<&Arc<Node>> {
        match &self.kind {
            Kind::Silence | Kind::Atom { .. } | Kind::Segment { .. } => Vec::new(),
            Kind::Seq(slots) => slots.iter().map(|s| &s.node).collect(),
            Kind::Stack(nodes) | Kind::Choice { alts: nodes, .. } | Kind::Cycles(nodes) => {
                nodes.iter().collect()
            }
            Kind::Fast { node, .. }
            | Kind::Degrade { node, .. }
            | Kind::Shift { node, .. }
            | Kind::Rev(node)
            | Kind::Iter { node, .. }
            | Kind::Ply { node, .. }
            | Kind::Swing { node, .. } => vec![node],
            Kind::CycleChance { yes, no, .. } => vec![yes, no],
            Kind::Struct { mask, node } | Kind::Mask { mask, node } => vec![mask, node],
            Kind::Set { node, arg, .. } => match arg {
                Arg::Pattern(arg) => vec![node, arg],
                _ => vec![node],
            },
            Kind::Switch { arg, cases } => std::iter::once(arg)
                .chain(cases.iter().map(|(_, n)| n))
                .collect(),
        }
    }

    pub fn silence() -> Arc<Node> {
        new(Kind::Silence, Bounds::SILENT)
    }

    pub fn atom(value: Value, span: Span) -> Arc<Node> {
        Node::atom_with(value, span, Controls::default())
    }

    pub fn atom_with(value: Value, span: Span, controls: Controls) -> Arc<Node> {
        new(
            Kind::Atom {
                value,
                span,
                controls,
            },
            Bounds::ONE,
        )
    }

    pub fn is_silence(&self) -> bool {
        matches!(self.kind, Kind::Silence)
    }

    /// Steps of `(node, weight)` sharing a cycle in proportion to their weights.
    pub fn sequence(steps: &[(Arc<Node>, Q)]) -> Arc<Node> {
        let total: Q = steps.iter().map(|(_, weight)| *weight).sum();
        let mut offset = Q::from_integer(0);
        let mut slots = Vec::new();
        for (node, weight) in steps {
            let start = offset / total;
            offset += weight;
            if !node.is_silence() {
                let end = offset / total;
                slots.push(Slot {
                    start,
                    end,
                    len: end - start,
                    node: Arc::clone(node),
                });
            }
        }
        Node::from_slots(slots)
    }

    /// `node` on the onsets of a Euclidean rhythm, one slot per entry of `hits`.
    pub fn euclid(node: Arc<Node>, hits: &[bool]) -> Arc<Node> {
        if node.is_silence() {
            return node;
        }
        let steps = hits.len() as i128;
        let len = Q::new(1, steps);
        let slots = (0..steps)
            .zip(hits)
            .filter(|(_, hit)| **hit)
            .map(|(i, _)| Slot {
                start: Q::new(i, steps),
                end: Q::new(i + 1, steps),
                len,
                node: Arc::clone(&node),
            })
            .collect();
        Node::from_slots(slots)
    }

    fn from_slots(mut slots: Vec<Slot>) -> Arc<Node> {
        match slots.len() {
            0 => Node::silence(),
            1 if slots[0].len == Q::from_integer(1) => slots.pop().expect("one slot").node,
            _ => {
                let bounds = slots
                    .iter()
                    .map(|slot| Bounds::slot(slot.start, slot.len).then(slot.node.bounds))
                    .fold(Bounds::SILENT, Bounds::both);
                new(Kind::Seq(slots), bounds)
            }
        }
    }

    pub fn stack(layers: Vec<Arc<Node>>) -> Arc<Node> {
        let mut layers: Vec<_> = layers.into_iter().filter(|n| !n.is_silence()).collect();
        match layers.len() {
            0 => Node::silence(),
            1 => layers.pop().expect("one layer"),
            _ => {
                let bounds = combine(&layers, Bounds::both);
                new(Kind::Stack(layers), bounds)
            }
        }
    }

    pub fn fast(node: Arc<Node>, factor: Q) -> Arc<Node> {
        if node.is_silence() || factor == Q::from_integer(1) {
            return node;
        }
        let bounds = Bounds::factor(factor).then(node.bounds);
        new(Kind::Fast { factor, node }, bounds)
    }

    pub fn choice(alts: Vec<Arc<Node>>, salt: u64) -> Arc<Node> {
        if alts.iter().all(|n| n.is_silence()) {
            return Node::silence();
        }
        let bounds = combine(&alts, Bounds::max);
        new(Kind::Choice { salt, alts }, bounds)
    }

    pub fn degrade(node: Arc<Node>, prob: f64, salt: u64) -> Arc<Node> {
        if node.is_silence() || prob <= 0.0 {
            return node;
        }
        let bounds = node.bounds;
        new(
            Kind::Degrade {
                salt,
                prob,
                invert: false,
                node,
            },
            bounds,
        )
    }

    /// The events [`Node::degrade`] with the same `salt` and `prob` drops.
    pub fn undegrade(node: Arc<Node>, prob: f64, salt: u64) -> Arc<Node> {
        if node.is_silence() || prob >= 1.0 {
            return node;
        }
        if prob <= 0.0 {
            return Node::silence();
        }
        let bounds = node.bounds;
        new(
            Kind::Degrade {
                salt,
                prob,
                invert: true,
                node,
            },
            bounds,
        )
    }

    /// `node` moved later by `by` cycles.
    pub fn shift(node: Arc<Node>, by: Q) -> Arc<Node> {
        if node.is_silence() || by == Q::from_integer(0) {
            return node;
        }
        let bounds = Bounds::shift(by).then(node.bounds);
        new(Kind::Shift { by, node }, bounds)
    }

    pub fn rev(node: Arc<Node>) -> Arc<Node> {
        if node.is_silence() {
            return node;
        }
        let bounds = node.bounds;
        new(Kind::Rev(node), bounds)
    }

    pub fn iter(node: Arc<Node>, n: i128, back: bool) -> Arc<Node> {
        if node.is_silence() || n <= 1 {
            return node;
        }
        let bounds = Bounds::shift(Q::new(1, n)).then(node.bounds);
        new(Kind::Iter { n, back, node }, bounds)
    }

    /// Cycle `c` plays `nodes[c mod len]`.
    pub fn cycles(mut nodes: Vec<Arc<Node>>) -> Arc<Node> {
        if nodes.iter().all(|n| n.is_silence()) {
            return Node::silence();
        }
        if nodes.len() == 1 || nodes.windows(2).all(|w| Arc::ptr_eq(&w[0], &w[1])) {
            return nodes.swap_remove(0);
        }
        let bounds = combine(&nodes, Bounds::max);
        new(Kind::Cycles(nodes), bounds)
    }

    pub fn cycle_chance(yes: Arc<Node>, no: Arc<Node>, prob: f64, salt: u64) -> Arc<Node> {
        if prob <= 0.0 {
            return no;
        }
        if prob >= 1.0 {
            return yes;
        }
        let bounds = yes.bounds.max(no.bounds);
        new(
            Kind::CycleChance {
                salt,
                prob,
                yes,
                no,
            },
            bounds,
        )
    }

    pub fn ply(node: Arc<Node>, n: i128) -> Arc<Node> {
        if node.is_silence() || n == 1 {
            return node;
        }
        let bounds = Bounds::ply(Q::from_integer(n)).then(node.bounds);
        new(Kind::Ply { n, node }, bounds)
    }

    pub fn structure(mask: Arc<Node>, node: Arc<Node>) -> Arc<Node> {
        if mask.is_silence() || node.is_silence() {
            return Node::silence();
        }
        let bounds = Bounds::lookup(mask.bounds, node.bounds, layers(&node));
        new(Kind::Struct { mask, node }, bounds)
    }

    pub fn mask(mask: Arc<Node>, node: Arc<Node>) -> Arc<Node> {
        if mask.is_silence() || node.is_silence() {
            return Node::silence();
        }
        let bounds = Bounds::lookup(node.bounds, mask.bounds, 1);
        new(Kind::Mask { mask, node }, bounds)
    }

    pub fn set(node: Arc<Node>, op: Op, arg: Arg) -> Arc<Node> {
        if node.is_silence() {
            return node;
        }
        let mut bounds = match &arg {
            Arg::Pattern(a) => Bounds::lookup(node.bounds, a.bounds, layers(a)),
            Arg::Const(_) | Arg::Signal(_) => node.bounds,
        };
        if op == Op::Legato {
            // Events can grow up to the legato limit.
            bounds.slow = bounds.slow.saturating_mul(crate::ops::MAX_LEGATO as u64);
        }
        new(Kind::Set { node, op, arg }, bounds)
    }

    pub fn switch(arg: Arc<Node>, cases: Vec<(Value, Arc<Node>)>) -> Arc<Node> {
        let nodes: Vec<Arc<Node>> = cases.iter().map(|(_, n)| Arc::clone(n)).collect();
        let inner = combine(&nodes, Bounds::max);
        let bounds = Bounds::lookup(inner, arg.bounds, layers(&arg));
        new(Kind::Switch { arg, cases }, bounds)
    }

    pub fn segment(signal: Signal, n: i128, span: Span) -> Arc<Node> {
        let bounds = Bounds::factor(Q::from_integer(n));
        new(Kind::Segment { n, signal, span }, bounds)
    }

    pub fn swing(node: Arc<Node>, by: Q, n: i128) -> Arc<Node> {
        if node.is_silence() || by == Q::from_integer(0) {
            return node;
        }
        let bounds = Bounds::shift(by / (n * 2)).then(node.bounds);
        new(Kind::Swing { n, by, node }, bounds)
    }
}

/// `node` with each atom replaced by what `f` makes of its value and
/// span: a new value, or `None` for a rest. Shared nodes stay shared.
pub(crate) fn map_atoms(
    node: &Arc<Node>,
    f: &mut dyn FnMut(&Value, Span) -> Option<Value>,
) -> Arc<Node> {
    fn go(
        node: &Arc<Node>,
        f: &mut dyn FnMut(&Value, Span) -> Option<Value>,
        memo: &mut std::collections::HashMap<*const Node, Arc<Node>>,
    ) -> Arc<Node> {
        if let Some(done) = memo.get(&Arc::as_ptr(node)) {
            return Arc::clone(done);
        }
        let mut m = |n: &Arc<Node>| go(n, f, memo);
        let out = match &node.kind {
            Kind::Silence | Kind::Segment { .. } => Arc::clone(node),
            Kind::Atom {
                value,
                span,
                controls,
            } => match f(value, *span) {
                Some(v) => Node::atom_with(v, *span, *controls),
                None => Node::silence(),
            },
            Kind::Seq(slots) => {
                let slots = slots
                    .iter()
                    .map(|s| Slot {
                        start: s.start,
                        end: s.end,
                        len: s.len,
                        node: m(&s.node),
                    })
                    .collect();
                Node::from_slots(slots)
            }
            Kind::Stack(nodes) => Node::stack(nodes.iter().map(&mut m).collect()),
            Kind::Fast { factor, node } => Node::fast(m(node), *factor),
            Kind::Choice { salt, alts } => Node::choice(alts.iter().map(&mut m).collect(), *salt),
            Kind::Degrade {
                salt,
                prob,
                invert,
                node,
            } => {
                let inner = m(node);
                if *invert {
                    Node::undegrade(inner, *prob, *salt)
                } else {
                    Node::degrade(inner, *prob, *salt)
                }
            }
            Kind::Shift { by, node } => Node::shift(m(node), *by),
            Kind::Rev(node) => Node::rev(m(node)),
            Kind::Iter { n, back, node } => Node::iter(m(node), *n, *back),
            Kind::Cycles(nodes) => Node::cycles(nodes.iter().map(&mut m).collect()),
            Kind::CycleChance {
                salt,
                prob,
                yes,
                no,
            } => {
                let yes = m(yes);
                Node::cycle_chance(yes, m(no), *prob, *salt)
            }
            Kind::Ply { n, node } => Node::ply(m(node), *n),
            Kind::Struct { mask, node } => Node::structure(Arc::clone(mask), m(node)),
            Kind::Mask { mask, node } => Node::mask(Arc::clone(mask), m(node)),
            Kind::Set { node, op, arg } => Node::set(m(node), op.clone(), arg.clone()),
            Kind::Switch { arg, cases } => {
                let cases = cases.iter().map(|(v, n)| (v.clone(), m(n))).collect();
                Node::switch(Arc::clone(arg), cases)
            }
            Kind::Swing { n, by, node } => Node::swing(m(node), *by, *n),
        };
        memo.insert(Arc::as_ptr(node), Arc::clone(&out));
        out
    }
    go(node, f, &mut std::collections::HashMap::new())
}

/// Most layers of a pattern sounding at once that a lookup takes values
/// from; more are ignored.
pub(crate) const MAX_LAYERS: u64 = 16;

/// About how many events of `node` can sound at once: stacks add up their
/// layers. Used to bound how many events a lookup can make.
pub(crate) fn layers(node: &Node) -> u64 {
    fn walk(node: &Node, memo: &mut std::collections::HashMap<*const Node, u64>) -> u64 {
        if let Some(n) = memo.get(&(node as *const Node)) {
            return *n;
        }
        let n = match &node.kind {
            Kind::Silence => 0,
            Kind::Atom { .. } | Kind::Segment { .. } => 1,
            Kind::Stack(nodes) => nodes
                .iter()
                .map(|n| walk(n, memo))
                .fold(0, u64::saturating_add),
            Kind::Struct { mask, node } => walk(mask, memo).saturating_mul(walk(node, memo)),
            Kind::Set { node, .. } => walk(node, memo),
            // Shifted copies may overlap the original.
            Kind::Shift { node, .. } | Kind::Swing { node, .. } => {
                walk(node, memo).saturating_mul(2)
            }
            _ => node
                .children()
                .into_iter()
                .map(|n| walk(n, memo))
                .max()
                .unwrap_or(0),
        };
        memo.insert(node as *const Node, n);
        n
    }
    walk(node, &mut std::collections::HashMap::new()).clamp(1, MAX_LAYERS)
}

/// Spreads `onsets` hits as evenly as possible over `steps` steps (Bjorklund's
/// algorithm), then rotates the result left by `rotation` steps. The unrotated
/// rhythm always starts with a hit. Requires `onsets <= steps`.
pub(crate) fn euclid_hits(onsets: usize, steps: usize, rotation: i64) -> Vec<bool> {
    debug_assert!(onsets <= steps);
    let mut hits = if onsets == 0 {
        vec![false; steps]
    } else {
        // Start with one group per hit and one per rest, then keep appending
        // the trailing groups onto the leading ones until at most one trailing
        // group is left over.
        let mut front: Vec<Vec<bool>> = vec![vec![true]; onsets];
        let mut back: Vec<Vec<bool>> = vec![vec![false]; steps - onsets];
        while back.len() > 1 {
            let pairs = front.len().min(back.len());
            let rest = if front.len() > pairs {
                front.split_off(pairs)
            } else {
                back.split_off(pairs)
            };
            for (group, tail) in front.iter_mut().zip(back) {
                group.extend(tail);
            }
            back = rest;
        }
        front.into_iter().chain(back).flatten().collect()
    };
    if steps > 0 {
        let shift = rotation.rem_euclid(steps as i64) as usize;
        hits.rotate_left(shift);
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rhythm(onsets: usize, steps: usize, rotation: i64) -> String {
        euclid_hits(onsets, steps, rotation)
            .into_iter()
            .map(|hit| if hit { 'x' } else { '.' })
            .collect()
    }

    #[test]
    fn euclidean_rhythms() {
        assert_eq!(rhythm(3, 8, 0), "x..x..x.");
        assert_eq!(rhythm(5, 8, 0), "x.xx.xx.");
        assert_eq!(rhythm(2, 5, 0), "x.x..");
        assert_eq!(rhythm(3, 4, 0), "xxx.");
        assert_eq!(rhythm(4, 7, 0), "x.x.x.x");
        assert_eq!(rhythm(5, 12, 0), "x..x.x..x.x.");
        assert_eq!(rhythm(7, 16, 0), "x..x.x.x..x.x.x.");
        assert_eq!(rhythm(4, 4, 0), "xxxx");
        assert_eq!(rhythm(1, 4, 0), "x...");
        assert_eq!(rhythm(0, 4, 0), "....");
        assert_eq!(rhythm(4, 16, 0), "x...x...x...x...");
    }

    #[test]
    fn euclidean_rotation() {
        assert_eq!(rhythm(3, 8, 2), ".x..x.x.");
        assert_eq!(rhythm(3, 8, 8), "x..x..x.");
        assert_eq!(rhythm(3, 8, -1), ".x..x..x");
        assert_eq!(rhythm(3, 8, 10), rhythm(3, 8, 2));
    }

    #[test]
    fn euclid_counts_are_exact() {
        for steps in 1..=32 {
            for onsets in 0..=steps {
                let hits = euclid_hits(onsets, steps, 0);
                assert_eq!(hits.len(), steps);
                assert_eq!(hits.iter().filter(|h| **h).count(), onsets);
                if onsets > 0 {
                    assert!(hits[0], "({onsets},{steps}) starts with a hit");
                }
            }
        }
    }

    #[test]
    fn node_bounds() {
        let atom = || Node::atom(Value::Note(60), Span { start: 0, end: 1 });
        let one = Q::from_integer(1);
        assert_eq!(atom().bounds, Bounds::ONE);
        let seq3 = Node::sequence(&[(atom(), one), (atom(), one), (atom(), one)]);
        assert_eq!((seq3.bounds.den, seq3.bounds.win), (3, 3));
        let fast = Node::fast(Arc::clone(&seq3), Q::new(3, 2));
        assert_eq!(
            (fast.bounds.den, fast.bounds.win, fast.bounds.fast),
            (9, 6, 2)
        );
        let stack = Node::stack(vec![fast, Node::fast(seq3, Q::new(1, 5))]);
        assert_eq!(
            (stack.bounds.den, stack.bounds.slow, stack.bounds.win),
            (9, 5, 15)
        );
    }
}

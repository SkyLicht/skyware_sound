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
use crate::{Span, Value};

/// Rational time inside the tree and during queries.
pub(crate) type Q = Ratio<i128>;

#[derive(Debug)]
pub(crate) struct Node {
    pub kind: Kind,
    pub bounds: Bounds,
}

#[derive(Debug)]
pub(crate) enum Kind {
    /// Plays nothing.
    Silence,
    /// One event per cycle, filling the cycle.
    Atom { value: Value, span: Span },
    /// Children squeezed into fixed slots of every cycle. Cycle `c` of a child
    /// plays inside its slot of cycle `c`.
    Seq(Vec<Slot>),
    /// Children playing at the same time.
    Stack(Vec<Arc<Node>>),
    /// The child sped up by `factor` (below 1 slows it down).
    Fast { factor: Q, node: Arc<Node> },
    /// One child per cycle, picked at random.
    Choice { salt: u64, alts: Vec<Arc<Node>> },
    /// The child with each event dropped with probability `prob`.
    Degrade {
        salt: u64,
        prob: f64,
        node: Arc<Node>,
    },
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
    Arc::new(Node { kind, bounds })
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
            Kind::Silence => {}
            Kind::Atom { value, span } => out.push((value, *span)),
            Kind::Seq(slots) => slots.iter().for_each(|s| s.node.atoms(seen, out)),
            Kind::Stack(nodes) | Kind::Choice { alts: nodes, .. } => {
                nodes.iter().for_each(|n| n.atoms(seen, out));
            }
            Kind::Fast { node, .. } | Kind::Degrade { node, .. } => node.atoms(seen, out),
        }
    }

    pub fn silence() -> Arc<Node> {
        new(Kind::Silence, Bounds::SILENT)
    }

    pub fn atom(value: Value, span: Span) -> Arc<Node> {
        new(Kind::Atom { value, span }, Bounds::ONE)
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
        new(Kind::Degrade { salt, prob, node }, bounds)
    }
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

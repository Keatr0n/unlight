//! Chooses which frame each part of a panorama comes from. A seam should run where neighbouring
//! frames agree, around a person who moved rather than through them, and that's a minimum cut:
//! pixels are nodes, and cutting between two of them costs however much the frames differ there
//! (Kwatra et al. 2003). Frames are added in turn, each cut against everything laid so far. The
//! flow solver is Boykov & Kolmogorov's (2004), which is at its best on grids like this.

use crate::Rgb;
use std::collections::VecDeque;

const NO_ARC: u32 = u32::MAX;
const TERMINAL: u32 = u32::MAX - 1;
const ORPHAN: u32 = u32::MAX - 2;
const FREE: u8 = 0;
const SOURCE: u8 = 1;
const SINK: u8 = 2;

/// Marks a canvas pixel no frame covers.
pub const NO_FRAME: u16 = u16::MAX;
/// Differences are measured in log light, floored here, so a seam cares about what the eye does.
const LOG_FLOOR: f32 = 0.01;
const COST_SCALE: f32 = 64.0;
/// What each cut edge costs at the very edge of the overlap, on top of any difference between
/// the frames. This is what puts a seam mid-overlap across empty sky, as far from both frames'
/// edges as it can be, where multi-band blending has the most room to work.
const CENTRE_BIAS: f32 = 6.0;
const WIRED: i32 = i32::MAX / 4;

/// A graph for minimum s-t cuts, with whole-number capacities so the solver terminates exactly.
#[derive(Clone, Debug)]
pub struct SeamGraph {
    first: Vec<u32>,
    next: Vec<u32>,
    head: Vec<u32>,
    residual: Vec<i32>,
    /// Positive for capacity from the source, negative for capacity to the sink.
    terminal: Vec<i32>,
}

struct Trees {
    tree: Vec<u8>,
    parent: Vec<u32>,
    stamp: Vec<u32>,
    distance: Vec<u32>,
    active: VecDeque<u32>,
    queued: Vec<bool>,
    orphans: Vec<u32>,
    time: u32,
}

impl Trees {
    fn activate(&mut self, node: u32) {
        if !self.queued[node as usize] {
            self.queued[node as usize] = true;
            self.active.push_back(node);
        }
    }

    fn orphan(&mut self, node: u32) {
        self.parent[node as usize] = ORPHAN;
        self.orphans.push(node);
    }
}

impl SeamGraph {
    pub fn new(nodes: usize) -> Self {
        Self {
            first: vec![NO_ARC; nodes],
            next: Vec::new(),
            head: Vec::new(),
            residual: Vec::new(),
            terminal: vec![0; nodes],
        }
    }

    pub fn add_edge(&mut self, a: u32, b: u32, forward: i32, backward: i32) {
        let arc = self.head.len() as u32;
        self.head.push(b);
        self.residual.push(forward);
        self.next.push(self.first[a as usize]);
        self.first[a as usize] = arc;
        self.head.push(a);
        self.residual.push(backward);
        self.next.push(self.first[b as usize]);
        self.first[b as usize] = arc + 1;
    }

    pub fn add_terminal(&mut self, node: u32, from_source: i32, to_sink: i32) {
        let total = i64::from(self.terminal[node as usize]) + i64::from(from_source) - i64::from(to_sink);
        self.terminal[node as usize] = total.clamp(-i64::from(WIRED), i64::from(WIRED)) as i32;
    }

    fn start_trees(&self) -> Trees {
        let nodes = self.first.len();
        let mut trees = Trees {
            tree: vec![FREE; nodes],
            parent: vec![NO_ARC; nodes],
            stamp: vec![0; nodes],
            distance: vec![0; nodes],
            active: VecDeque::new(),
            queued: vec![false; nodes],
            orphans: Vec::new(),
            time: 0,
        };
        for node in 0..nodes {
            let flow = self.terminal[node];
            if flow != 0 {
                trees.tree[node] = if flow > 0 { SOURCE } else { SINK };
                trees.parent[node] = TERMINAL;
                trees.distance[node] = 1;
                trees.activate(node as u32);
            }
        }
        trees
    }

    /// Grows the trees until they touch, returning the arc from source side to sink side.
    fn grow(&self, trees: &mut Trees) -> Option<u32> {
        while let Some(node) = trees.active.pop_front() {
            trees.queued[node as usize] = false;
            let side = trees.tree[node as usize];
            if side == FREE {
                continue;
            }
            let mut arc = self.first[node as usize];
            while arc != NO_ARC {
                let open = if side == SOURCE { self.residual[arc as usize] } else { self.residual[(arc ^ 1) as usize] };
                if open > 0 {
                    let other = self.head[arc as usize] as usize;
                    let other_side = trees.tree[other];
                    if other_side == FREE {
                        trees.tree[other] = side;
                        trees.parent[other] = arc ^ 1;
                        trees.stamp[other] = trees.stamp[node as usize];
                        trees.distance[other] = trees.distance[node as usize] + 1;
                        trees.activate(other as u32);
                    } else if other_side != side {
                        trees.activate(node);
                        return Some(if side == SOURCE { arc } else { arc ^ 1 });
                    } else if trees.stamp[other] <= trees.stamp[node as usize]
                        && trees.distance[other] > trees.distance[node as usize]
                    {
                        trees.parent[other] = arc ^ 1;
                        trees.stamp[other] = trees.stamp[node as usize];
                        trees.distance[other] = trees.distance[node as usize] + 1;
                    }
                }
                arc = self.next[arc as usize];
            }
        }
        None
    }

    fn augment(&mut self, trees: &mut Trees, middle: u32) {
        let tail = self.head[(middle ^ 1) as usize];
        let front = self.head[middle as usize];
        let mut flow = self.residual[middle as usize];
        let mut node = tail;
        loop {
            let arc = trees.parent[node as usize];
            if arc == TERMINAL {
                break;
            }
            flow = flow.min(self.residual[(arc ^ 1) as usize]);
            node = self.head[arc as usize];
        }
        flow = flow.min(self.terminal[node as usize]);
        node = front;
        loop {
            let arc = trees.parent[node as usize];
            if arc == TERMINAL {
                break;
            }
            flow = flow.min(self.residual[arc as usize]);
            node = self.head[arc as usize];
        }
        flow = flow.min(-self.terminal[node as usize]);

        self.residual[middle as usize] -= flow;
        self.residual[(middle ^ 1) as usize] += flow;
        node = tail;
        loop {
            let arc = trees.parent[node as usize];
            if arc == TERMINAL {
                break;
            }
            self.residual[arc as usize] += flow;
            self.residual[(arc ^ 1) as usize] -= flow;
            if self.residual[(arc ^ 1) as usize] == 0 {
                trees.orphan(node);
            }
            node = self.head[arc as usize];
        }
        self.terminal[node as usize] -= flow;
        if self.terminal[node as usize] == 0 {
            trees.orphan(node);
        }
        node = front;
        loop {
            let arc = trees.parent[node as usize];
            if arc == TERMINAL {
                break;
            }
            self.residual[(arc ^ 1) as usize] += flow;
            self.residual[arc as usize] -= flow;
            if self.residual[arc as usize] == 0 {
                trees.orphan(node);
            }
            node = self.head[arc as usize];
        }
        self.terminal[node as usize] += flow;
        if self.terminal[node as usize] == 0 {
            trees.orphan(node);
        }
    }

    /// Length of the path from a node to its terminal, if it still has one.
    fn origin_distance(&self, trees: &mut Trees, start: u32) -> Option<u32> {
        let time = trees.time;
        let mut distance = 0;
        let mut node = start as usize;
        loop {
            if trees.stamp[node] == time {
                distance += trees.distance[node];
                break;
            }
            distance += 1;
            let arc = trees.parent[node];
            if arc == TERMINAL {
                trees.stamp[node] = time;
                trees.distance[node] = 1;
                break;
            }
            if arc == ORPHAN || arc == NO_ARC {
                return None;
            }
            node = self.head[arc as usize] as usize;
        }
        let mut remaining = distance;
        let mut node = start as usize;
        while trees.stamp[node] != time {
            trees.stamp[node] = time;
            trees.distance[node] = remaining;
            remaining -= 1;
            node = self.head[trees.parent[node] as usize] as usize;
        }
        Some(distance)
    }

    fn adopt(&self, trees: &mut Trees) {
        while let Some(orphan) = trees.orphans.pop() {
            let orphan_at = orphan as usize;
            let side = trees.tree[orphan_at];
            let open = |graph: &Self, arc: u32| {
                if side == SOURCE { graph.residual[(arc ^ 1) as usize] } else { graph.residual[arc as usize] }
            };
            let mut best = (NO_ARC, u32::MAX);
            let mut arc = self.first[orphan_at];
            while arc != NO_ARC {
                let other = self.head[arc as usize];
                if trees.tree[other as usize] == side
                    && open(self, arc) > 0
                    && let Some(distance) = self.origin_distance(trees, other)
                    && distance < best.1
                {
                    best = (arc, distance);
                }
                arc = self.next[arc as usize];
            }
            if best.0 != NO_ARC {
                trees.parent[orphan_at] = best.0;
                trees.stamp[orphan_at] = trees.time;
                trees.distance[orphan_at] = best.1 + 1;
                continue;
            }
            let mut arc = self.first[orphan_at];
            while arc != NO_ARC {
                let other = self.head[arc as usize];
                if trees.tree[other as usize] == side {
                    if open(self, arc) > 0 {
                        trees.activate(other);
                    }
                    let parent = trees.parent[other as usize];
                    if parent != TERMINAL && parent != ORPHAN && parent != NO_ARC && self.head[parent as usize] == orphan {
                        trees.orphan(other);
                    }
                }
                arc = self.next[arc as usize];
            }
            trees.tree[orphan_at] = FREE;
            trees.parent[orphan_at] = NO_ARC;
        }
    }

    /// Finds the minimum cut. Returns, per node, whether it stays with the source.
    pub fn solve(mut self) -> Vec<bool> {
        let mut trees = self.start_trees();
        while let Some(middle) = self.grow(&mut trees) {
            trees.time += 1;
            self.augment(&mut trees, middle);
            self.adopt(&mut trees);
        }
        trees.tree.iter().map(|side| *side == SOURCE).collect()
    }
}

/// One frame on the small working canvas, already corrected for exposure and vignetting.
#[derive(Clone, Debug, Default)]
pub struct Layer {
    pub left: usize,
    pub top: usize,
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<Rgb>,
    pub covered: Vec<bool>,
}

impl Layer {
    fn pixel(&self, x: usize, y: usize) -> Option<Rgb> {
        if x < self.left || y < self.top || x >= self.left + self.width || y >= self.top + self.height {
            return None;
        }
        let at = (y - self.top) * self.width + x - self.left;
        self.covered[at].then(|| self.pixels[at])
    }
}

fn perceived(pixel: Rgb) -> Rgb {
    pixel.map(|value| (value.max(0.0) + LOG_FLOOR).ln())
}

fn difference(a: Rgb, b: Rgb) -> f32 {
    (0..3).map(|c| (a[c] - b[c]).abs()).sum()
}

/// Steps from each node to the nearest in `seeds`, along the grid.
fn walk_distance(nodes: &[u32], width: usize, height: usize, seeds: &[u32], at_node: &[usize]) -> Vec<u32> {
    let mut distance = vec![u32::MAX; at_node.len()];
    let mut queue = VecDeque::new();
    for &seed in seeds {
        distance[seed as usize] = 0;
        queue.push_back(seed);
    }
    while let Some(node) = queue.pop_front() {
        let at = at_node[node as usize];
        let (x, y) = (at % width, at / width);
        let here = distance[node as usize];
        let mut step = |next: usize| {
            let neighbour = nodes[next];
            if neighbour != u32::MAX && distance[neighbour as usize] == u32::MAX {
                distance[neighbour as usize] = here + 1;
                queue.push_back(neighbour);
            }
        };
        if x > 0 {
            step(at - 1);
        }
        if x + 1 < width {
            step(at + 1);
        }
        if y > 0 {
            step(at - width);
        }
        if y + 1 < height {
            step(at + width);
        }
    }
    distance
}

/// Cuts `layer` into what's been laid so far. Pixels where it overlaps are left to a graph cut,
/// pinned to whichever frame has the only claim on the pixels beside them.
fn cut_in(labels: &mut [u16], composite: &mut [Rgb], width: usize, height: usize, layer: &Layer, id: u16) {
    let mut nodes = vec![u32::MAX; width * height];
    let mut at_node = Vec::new();
    for y in layer.top..(layer.top + layer.height).min(height) {
        for x in layer.left..(layer.left + layer.width).min(width) {
            let at = y * width + x;
            match layer.pixel(x, y) {
                Some(pixel) if labels[at] == NO_FRAME => {
                    labels[at] = id;
                    composite[at] = pixel;
                }
                Some(_) => {
                    nodes[at] = at_node.len() as u32;
                    at_node.push(at);
                }
                None => {}
            }
        }
    }
    if at_node.is_empty() {
        return;
    }

    let mismatch: Vec<f32> = at_node
        .iter()
        .map(|&at| {
            let new = layer.pixel(at % width, at / width).unwrap_or([0.0; 3]);
            difference(perceived(composite[at]), perceived(new))
        })
        .collect();
    let (mut pinned_old, mut pinned_new) = (Vec::new(), Vec::new());
    for (node, &at) in at_node.iter().enumerate() {
        let (x, y) = (at % width, at / width);
        let mut look = |next: usize| {
            if nodes[next] != u32::MAX {
                return;
            }
            if labels[next] == id {
                pinned_new.push(node as u32);
            } else if labels[next] != NO_FRAME {
                pinned_old.push(node as u32);
            }
        };
        if x > 0 {
            look(at - 1);
        }
        if x + 1 < width {
            look(at + 1);
        }
        if y > 0 {
            look(at - width);
        }
        if y + 1 < height {
            look(at + width);
        }
    }
    // A frame entirely inside another has nothing to pin it to, and adds nothing anyway.
    if pinned_old.is_empty() || pinned_new.is_empty() {
        return;
    }

    let from_old = walk_distance(&nodes, width, height, &pinned_old, &at_node);
    let from_new = walk_distance(&nodes, width, height, &pinned_new, &at_node);
    let off_centre = |node: usize| {
        let (a, b) = (from_old[node].min(1 << 24) as f32, from_new[node].min(1 << 24) as f32);
        (a - b).abs() / (a + b + 1.0)
    };
    let capacity = |a: usize, b: usize| {
        let cost = (mismatch[a] + mismatch[b]) * COST_SCALE;
        let bias = 1.0 + CENTRE_BIAS * (off_centre(a) + off_centre(b)) * 0.5;
        (cost + bias) as i32
    };

    let mut graph = SeamGraph::new(at_node.len());
    for (node, &at) in at_node.iter().enumerate() {
        let (x, y) = (at % width, at / width);
        if x + 1 < width && nodes[at + 1] != u32::MAX {
            let weight = capacity(node, nodes[at + 1] as usize);
            graph.add_edge(node as u32, nodes[at + 1], weight, weight);
        }
        if y + 1 < height && nodes[at + width] != u32::MAX {
            let weight = capacity(node, nodes[at + width] as usize);
            graph.add_edge(node as u32, nodes[at + width], weight, weight);
        }
    }
    for &node in &pinned_old {
        graph.add_terminal(node, WIRED, 0);
    }
    for &node in &pinned_new {
        graph.add_terminal(node, 0, WIRED);
    }
    for (node, stays) in graph.solve().into_iter().enumerate() {
        if !stays {
            let at = at_node[node];
            labels[at] = id;
            if let Some(pixel) = layer.pixel(at % width, at / width) {
                composite[at] = pixel;
            }
        }
    }
}

/// Labels each canvas pixel with the index of the frame it should come from.
pub fn select_seams(layers: &[Layer], width: usize, height: usize) -> Vec<u16> {
    let mut labels = vec![NO_FRAME; width * height];
    let mut composite = vec![[0.0f32; 3]; width * height];
    let mut order: Vec<usize> = (0..layers.len()).collect();
    order.sort_by_key(|&index| (layers[index].left + layers[index].width / 2, layers[index].top));
    for index in order {
        cut_in(&mut labels, &mut composite, width, height, &layers[index], index as u16);
    }
    labels
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Random(u64);

    impl Random {
        fn below(&mut self, limit: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % limit
        }
    }

    /// The cheapest cut by trying them all, and what a given labelling costs.
    fn exhaustive(nodes: usize, edges: &[(u32, u32, i32)], terminals: &[(i32, i32)]) -> i64 {
        (0u32..1 << nodes).map(|mask| cut_cost(mask, edges, terminals)).min().unwrap()
    }

    fn cut_cost(mask: u32, edges: &[(u32, u32, i32)], terminals: &[(i32, i32)]) -> i64 {
        let source_side = |node: u32| mask >> node & 1 == 1;
        let mut cost = 0i64;
        for (node, (from_source, to_sink)) in terminals.iter().enumerate() {
            cost += i64::from(if source_side(node as u32) { *to_sink } else { *from_source });
        }
        for &(a, b, weight) in edges {
            if source_side(a) != source_side(b) {
                cost += i64::from(weight);
            }
        }
        cost
    }

    #[test]
    fn min_cut_matches_brute_force_on_random_graphs() {
        let mut random = Random(0x1234_5678_9abc_def1);
        for round in 0..300 {
            let nodes = 3 + (random.below(8) as usize);
            let mut edges = Vec::new();
            for a in 0..nodes as u32 {
                for b in a + 1..nodes as u32 {
                    if random.below(100) < 50 {
                        edges.push((a, b, random.below(40) as i32));
                    }
                }
            }
            let terminals: Vec<(i32, i32)> =
                (0..nodes).map(|_| (random.below(60) as i32, random.below(60) as i32)).collect();
            let mut graph = SeamGraph::new(nodes);
            for &(a, b, weight) in &edges {
                graph.add_edge(a, b, weight, weight);
            }
            for (node, &(from_source, to_sink)) in terminals.iter().enumerate() {
                graph.add_terminal(node as u32, from_source, to_sink);
            }
            let side = graph.solve();
            let mask = side.iter().enumerate().fold(0u32, |mask, (node, stays)| mask | u32::from(*stays) << node);
            assert_eq!(cut_cost(mask, &edges, &terminals), exhaustive(nodes, &edges, &terminals), "round {round}");
        }
    }

    fn band(left: usize, width: usize, height: usize, pixel: impl Fn(usize, usize) -> Rgb) -> Layer {
        Layer {
            left,
            top: 0,
            width,
            height,
            pixels: (0..width * height).map(|at| pixel(at % width + left, at / width)).collect(),
            covered: vec![true; width * height],
        }
    }

    #[test]
    fn a_seam_runs_mid_overlap_when_nothing_else_matters() {
        let flat = |_: usize, _: usize| [0.2; 3];
        let layers = [band(0, 60, 40, flat), band(40, 60, 40, flat)];
        let labels = select_seams(&layers, 100, 40);
        assert_eq!(labels[20 * 100 + 5], 0);
        assert_eq!(labels[20 * 100 + 95], 1);
        let crossing = (0..100).find(|&x| labels[20 * 100 + x] == 1).unwrap();
        assert!((44..56).contains(&crossing), "{crossing}");
    }

    #[test]
    fn a_seam_goes_around_something_only_one_frame_saw() {
        let background = |x: usize, y: usize| [0.1 + ((x * 7 + y * 3) % 5) as f32 * 0.01; 3];
        let with_person = move |x: usize, y: usize| {
            if (48..56).contains(&x) && (10..30).contains(&y) { [0.9; 3] } else { background(x, y) }
        };
        let layers = [band(0, 60, 40, background), band(40, 60, 40, with_person)];
        let labels = select_seams(&layers, 100, 40);
        let from_person_frame = (10..30).flat_map(|y| (48..56).map(move |x| (x, y))).filter(|(x, y)| labels[y * 100 + x] == 1).count();
        let from_clean_frame = 8 * 20 - from_person_frame;
        assert!(from_person_frame == 0 || from_clean_frame == 0, "{from_person_frame} {from_clean_frame}");
    }

    #[test]
    fn every_covered_pixel_gets_a_frame() {
        let flat = |_: usize, _: usize| [0.2; 3];
        let layers = [band(0, 40, 30, flat), band(30, 40, 30, flat), band(60, 40, 30, flat)];
        let labels = select_seams(&layers, 110, 30);
        assert!(labels[..100].iter().all(|label| *label != NO_FRAME));
        assert_eq!(labels[105], NO_FRAME);
    }

    #[test]
    fn a_frame_inside_another_leaves_nothing_uncovered() {
        let flat = |_: usize, _: usize| [0.2; 3];
        let layers = [band(0, 60, 30, flat), band(10, 20, 30, flat)];
        let labels = select_seams(&layers, 60, 30);
        assert!(labels.iter().all(|label| *label != NO_FRAME));
        assert_eq!(labels[5], 0);
        assert_eq!(labels[45], 0);
    }

    #[test]
    fn frames_that_never_touch_leave_a_gap() {
        let flat = |_: usize, _: usize| [0.2; 3];
        let layers = [band(0, 20, 10, flat), band(40, 20, 10, flat)];
        let labels = select_seams(&layers, 60, 10);
        assert_eq!(labels[5], 0);
        assert_eq!(labels[30], NO_FRAME);
        assert_eq!(labels[50], 1);
    }
}
